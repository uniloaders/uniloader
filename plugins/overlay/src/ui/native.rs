//! Native textures in panels (`UiWidget::GPU_TEXTURE`): the meeting point of the widget
//! half, which runs inside the egui pass, and the backend, which can wrap a game texture
//! for its renderer only around that pass. Both run on the render thread.
//!
//! A panel asks for a handle during the pass; the panel shows it in a column beside its
//! other widgets (as the game's own menus do), drawn once the backend registered it on an
//! earlier frame and an empty box of its size until then. After the pass the backend
//! registers what was asked for and drops what no longer is.
//!
//! The handle is only known alive during the widget call (the plugin keeps it so for
//! that long), so the call itself takes a reference of ours; everything after works on
//! that, never on the address.

use std::cell::RefCell;
use std::collections::HashMap;

use egui::TextureId;

/// This frame's asks and the textures the backend has registered.
#[derive(Default)]
pub(crate) struct Natives {
    /// The backend can show this device's textures (set before each pass).
    pub(crate) supported: bool,
    /// Handles asked for during this pass, each with our reference to it.
    pub(crate) asked: Vec<(usize, Owned)>,
    /// Registered textures by handle.
    pub(crate) ids: HashMap<usize, TextureId>,
    /// The panel being drawn's pictures, for the column beside it.
    pub(crate) aside: Vec<Picture>,
}

/// One texture a panel asked for.
pub(crate) struct Picture {
    handle: usize,
    size: egui::Vec2,
    caption: String,
}

/// Our reference to a native texture.
#[cfg(windows)]
pub(crate) type Owned = windows::Win32::Graphics::Direct3D12::ID3D12Resource;
#[cfg(not(windows))]
pub(crate) type Owned = ();

thread_local! {
    pub(crate) static NATIVES: RefCell<Natives> = RefCell::new(Natives::default());
}

/// Before a pass: whether the backend can show native textures; the asks start over.
pub(crate) fn begin(supported: bool) {
    NATIVES.with_borrow_mut(|n| {
        n.supported = supported;
        n.asked.clear();
        if !supported {
            n.ids.clear();
        }
    });
}

/// The handles asked for in the pass that just ran, with our references to them.
pub(crate) fn take_asked() -> Vec<(usize, Owned)> {
    NATIVES.with_borrow_mut(|n| std::mem::take(&mut n.asked))
}

/// After registering: what the next passes can draw.
pub(crate) fn set_registered(ids: HashMap<usize, TextureId>) {
    NATIVES.with_borrow_mut(|n| n.ids = ids);
}

/// The widget: puts `handle` at `size` with `caption` in the panel's picture column;
/// `false` when the backend cannot show native textures. `handle` must be alive for
/// this call; a reference of ours is taken in it.
pub(crate) fn show(handle: usize, size: egui::Vec2, caption: &str) -> bool {
    NATIVES.with_borrow_mut(|n| {
        if !n.supported {
            return false;
        }
        let Some(owned) = retain(handle) else {
            return false;
        };
        n.asked.push((handle, owned));
        n.aside.push(Picture {
            handle,
            size,
            caption: caption.to_owned(),
        });
        true
    })
}

/// Our own reference to the texture at `handle` (alive for this call), when there is one.
#[cfg(windows)]
fn retain(handle: usize) -> Option<Owned> {
    let raw = handle as *mut std::ffi::c_void;
    // SAFETY: the plugin keeps the resource alive for the widget call this runs in
    // (`GPU_TEXTURE`'s contract); `cloned` takes our reference before the call returns.
    #[allow(unsafe_code)]
    unsafe { <Owned as windows::core::Interface>::from_raw_borrowed(&raw) }.cloned()
}

#[cfg(not(windows))]
fn retain(_handle: usize) -> Option<Owned> {
    None
}

/// A panel body: `body` draws the widgets (scrolling when they outgrow the window), and
/// the pictures they asked for stand in a column to their right.
pub(crate) fn panel<R>(ui: &mut egui::Ui, salt: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let before = NATIVES.with_borrow_mut(|n| std::mem::take(&mut n.aside));
    ui.horizontal_top(|ui| {
        let result = ui
            .vertical(|ui| {
                egui::ScrollArea::vertical()
                    .id_salt(salt)
                    .show(ui, body)
                    .inner
            })
            .inner;
        let pictures = NATIVES.with_borrow_mut(|n| std::mem::replace(&mut n.aside, before));
        if !pictures.is_empty() {
            ui.separator();
            ui.vertical(|ui| {
                for picture in &pictures {
                    draw(ui, picture);
                }
            });
        }
        result
    })
    .inner
}

fn draw(ui: &mut egui::Ui, picture: &Picture) {
    match NATIVES.with_borrow(|n| n.ids.get(&picture.handle).copied()) {
        Some(id) => {
            ui.image((id, picture.size));
        }
        None => {
            ui.allocate_space(picture.size);
        }
    }
    if !picture.caption.is_empty() {
        ui.label(&picture.caption);
    }
}
