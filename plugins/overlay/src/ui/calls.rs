//! Calls into consumers: panel bodies, dialog answers, present subscribers.
//!
//! Every one goes through the loader's fault guard for its owner, so a consumer that panics
//! or faults is disabled on its own and the overlay keeps drawing. The loader then announces
//! it as faulted, and the overlay drops everything it registered.

// FFI module: the callbacks are raw function pointers with raw contexts.
#![allow(unsafe_code)]

use core::ffi::c_void;

use uniloader_overlay::PresentInfo;
use uniloader_sdk::api::Str;
use uniloader_sdk::{Host, PluginRef};

use super::frame;
use crate::notices::Answered;
use crate::registry::{Panel, Subscriber};

/// Fill one panel body. `false` when the owner is not running or faulted inside.
pub(crate) fn panel(host: &Host, panel: &Panel, ui: &mut egui::Ui, token: u64) -> bool {
    let owner_name = panel.id.split_once('.').map_or("", |(owner, _)| owner);
    host.guarded(PluginRef::from_raw(panel.owner), || {
        frame::with(token, panel.owner, owner_name, ui, || {
            // SAFETY: `draw` and `ctx` are what the owner registered together, and the
            // owner promised the callback is valid while it runs; the name lives for the call.
            unsafe { (panel.draw)(panel.ctx as *mut c_void, Str::new(&panel.name), token) };
        });
    })
    .is_ok()
}

/// Tell a dialog's owner how it ended.
pub(crate) fn answer(host: &Host, answered: &Answered) {
    let callback = answered.callback;
    let result = host.guarded(PluginRef::from_raw(answered.owner), || {
        // SAFETY: the owner registered this callback with this context for this dialog, and
        // it is called exactly once; the text lives for the call.
        unsafe {
            (callback.callback)(
                callback.ctx as *mut c_void,
                answered.id,
                answered.answer,
                Str::new(&answered.text),
            );
        }
    });
    if let Err(e) = result {
        log::debug!("dialog {} answer not delivered: {e}", answered.id);
    }
}

/// Hand one frame to a present subscriber.
pub(crate) fn present(host: &Host, subscriber: &Subscriber, info: &PresentInfo) {
    let _ = host.guarded(PluginRef::from_raw(subscriber.owner), || {
        // SAFETY: the owner registered this callback with this context; `info` lives for the
        // call.
        unsafe { (subscriber.callback)(subscriber.ctx as *mut c_void, info) };
    });
}
