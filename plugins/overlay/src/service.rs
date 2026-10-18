//! The `overlay` service table: the functions behind [`uniloader_overlay::OverlayApi`].
//!
//! Every function validates what it is given and never trusts a pointer it does not need.
//! Nothing here calls back into a consumer: callbacks run on the render thread, after the
//! frame that triggered them, under the loader's fault guard for their owner.

// FFI module: called by other plugins through raw pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;

use uniloader_overlay::{
    DialogDesc, DialogFn, NoticeDesc, OverlayApi, PanelDesc, PanelFn, PresentFn, UiValue, UiWidget,
};
use uniloader_sdk::api::{PluginHandle, Status, Str};
use uniloader_sdk::{read_str, PluginRef};

use crate::{notices, registry, shared, ui};

/// The table registered as the service.
pub(crate) static TABLE: OverlayApi = OverlayApi {
    struct_size: core::mem::size_of::<OverlayApi>(),
    panel_add,
    panel_remove,
    panel_set_open,
    panel_toggle,
    panel_is_open,
    widget,
    notice_show,
    dialog_open,
    close,
    present_subscribe,
    is_capturing,
};

/// Read a struct a consumer passed, checking it is at least as large as ours.
///
/// # Safety
/// `ptr` must be null or point at a readable struct whose first field is its `usize` size.
unsafe fn read_desc<T: Copy>(ptr: *const T) -> Option<T> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null, and the ABI puts the size first in every descriptor.
    let size = unsafe { *ptr.cast::<usize>() };
    if size < core::mem::size_of::<T>() {
        return None;
    }
    // SAFETY: the caller's struct is at least as large as ours.
    Some(unsafe { *ptr })
}

/// The name of the plugin behind a handle, for qualified ids.
fn owner_name(owner: PluginHandle) -> Option<String> {
    let host = crate::host()?;
    let me = PluginRef::from_raw(owner);
    host.modules()
        .into_iter()
        .find(|m| m.plugin == Some(me))
        .map(|m| m.name)
}

unsafe extern "C" fn panel_add(
    owner: PluginHandle,
    desc: *const PanelDesc,
    draw: Option<PanelFn>,
    ctx: *mut c_void,
) -> Status {
    // SAFETY: the ABI requires a valid descriptor or null.
    let (Some(desc), Some(draw)) = (unsafe { read_desc(desc) }, draw) else {
        return Status::INVALID_ARGUMENT;
    };
    // SAFETY: the descriptor's strings are valid for the call.
    let (name, title) = unsafe { (read_str(desc.name), read_str(desc.title)) };
    if !registry::valid_name(&name) {
        return Status::INVALID_ARGUMENT;
    }
    let Some(plugin) = owner_name(owner) else {
        return Status::NOT_FOUND;
    };
    let added = registry::add(registry::Panel {
        owner,
        id: format!("{plugin}.{name}"),
        title: if title.is_empty() {
            name.clone()
        } else {
            title
        },
        name,
        open: desc.default_open,
        draw,
        ctx: ctx as usize,
    });
    if !added {
        return Status::ALREADY_EXISTS;
    }
    shared::refresh();
    Status::OK
}

unsafe extern "C" fn panel_remove(owner: PluginHandle, name: Str) -> Status {
    // SAFETY: valid for the call, per the ABI.
    let name = unsafe { read_str(name) };
    if !registry::remove(owner, &name) {
        return Status::NOT_FOUND;
    }
    shared::refresh();
    Status::OK
}

/// # Safety
/// `name` must be valid for the call, as the ABI promises.
unsafe fn set_open_with(owner: PluginHandle, name: Str, f: impl FnOnce(bool) -> bool) -> Status {
    // SAFETY: this function's contract.
    let name = unsafe { read_str(name) };
    if registry::update_open(owner, &name, f).is_none() {
        return Status::NOT_FOUND;
    }
    shared::refresh();
    Status::OK
}

unsafe extern "C" fn panel_set_open(owner: PluginHandle, name: Str, open: bool) -> Status {
    // SAFETY: `name` comes straight from the caller, under the same ABI promise.
    unsafe { set_open_with(owner, name, |_| open) }
}

unsafe extern "C" fn panel_toggle(owner: PluginHandle, name: Str) -> Status {
    // SAFETY: as above.
    unsafe { set_open_with(owner, name, |open| !open) }
}

unsafe extern "C" fn panel_is_open(owner: PluginHandle, name: Str, out: *mut bool) -> Status {
    if out.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    // SAFETY: valid for the call, per the ABI.
    let name = unsafe { read_str(name) };
    let Some(open) = registry::snapshot()
        .into_iter()
        .find(|p| p.owner == owner && p.name == name)
        .map(|p| p.open)
    else {
        return Status::NOT_FOUND;
    };
    // SAFETY: checked non-null; the ABI requires a writable bool.
    unsafe { out.write(open) };
    Status::OK
}

unsafe extern "C" fn widget(
    owner: PluginHandle,
    frame: u64,
    kind: UiWidget,
    text: Str,
    value: *mut UiValue,
) -> Status {
    // SAFETY: valid for the call, per the ABI.
    let text = unsafe { read_str(text) };
    let value = if value.is_null() {
        None
    } else {
        // SAFETY: non-null; the size is checked before anything past it is touched.
        let size = unsafe { (*value).struct_size };
        if size < core::mem::size_of::<UiValue>() {
            return Status::INVALID_ARGUMENT;
        }
        // SAFETY: a writable value of at least our size, exclusively ours for the call.
        Some(unsafe { &mut *value })
    };
    ui::frame::widget(owner, frame, kind, &text, value)
}

unsafe extern "C" fn notice_show(
    owner: PluginHandle,
    desc: *const NoticeDesc,
    out_id: *mut u64,
) -> Status {
    // SAFETY: the ABI requires a valid descriptor or null.
    let Some(desc) = (unsafe { read_desc(desc) }) else {
        return Status::INVALID_ARGUMENT;
    };
    // SAFETY: the descriptor's strings are valid for the call.
    let (title, text) = unsafe { (read_str(desc.title), read_str(desc.text)) };
    let id = notices::passing(
        Some(owner),
        desc.kind,
        desc.level,
        &title,
        &text,
        desc.seconds,
    );
    shared::recount();
    if !out_id.is_null() {
        // SAFETY: checked non-null; the ABI requires a writable `u64`.
        unsafe { out_id.write(id) };
    }
    Status::OK
}

unsafe extern "C" fn dialog_open(
    owner: PluginHandle,
    desc: *const DialogDesc,
    answer: Option<DialogFn>,
    ctx: *mut c_void,
    out_id: *mut u64,
) -> Status {
    // SAFETY: the ABI requires a valid descriptor or null.
    let Some(desc) = (unsafe { read_desc(desc) }) else {
        return Status::INVALID_ARGUMENT;
    };
    // SAFETY: the descriptor's strings are valid for the call.
    let strings = unsafe {
        [
            read_str(desc.title),
            read_str(desc.text),
            read_str(desc.default_text),
            read_str(desc.accept_label),
            read_str(desc.cancel_label),
        ]
    };
    let [title, text, default_text, accept, cancel] = &strings;
    let words = notices::DialogText {
        title,
        text,
        default_text,
        accept,
        cancel,
    };
    let answer = answer.map(|callback| notices::Answer {
        callback,
        ctx: ctx as usize,
    });
    let id = notices::dialog(owner, desc.kind, desc.level, &words, answer);
    shared::recount();
    if !out_id.is_null() {
        // SAFETY: checked non-null; the ABI requires a writable `u64`.
        unsafe { out_id.write(id) };
    }
    Status::OK
}

unsafe extern "C" fn close(owner: PluginHandle, id: u64) -> Status {
    match notices::take_owned(owner, id) {
        Err(()) => Status::NOT_FOUND,
        Ok(answered) => {
            if let Some(answered) = answered {
                // Delivered on the next frame, never from inside the owner's own call.
                ui::queue_answer(answered);
            }
            shared::recount();
            Status::OK
        }
    }
}

unsafe extern "C" fn present_subscribe(
    owner: PluginHandle,
    callback: Option<PresentFn>,
    ctx: *mut c_void,
) -> Status {
    let Some(callback) = callback else {
        return Status::INVALID_ARGUMENT;
    };
    registry::subscribe(registry::Subscriber {
        owner,
        callback,
        ctx: ctx as usize,
    });
    Status::OK
}

unsafe extern "C" fn is_capturing() -> bool {
    shared::is_capturing()
}
