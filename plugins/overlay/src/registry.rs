//! Panels and present subscribers, by owner.
//!
//! Callbacks are stored with their context as an address, so the lists are plain `Send` data
//! behind a mutex; they are only turned back into pointers on the render thread, under the
//! loader's fault guard for the owner.

use std::sync::{Mutex, MutexGuard};

use uniloader_overlay::{PanelFn, PresentFn};
use uniloader_sdk::api::PluginHandle;

/// One registered panel.
#[derive(Debug, Clone)]
pub(crate) struct Panel {
    /// The plugin that fills it.
    pub(crate) owner: PluginHandle,
    /// `<plugin>.<name>`: what the layout remembers it by.
    pub(crate) id: String,
    /// Name within the owner.
    pub(crate) name: String,
    /// Window title.
    pub(crate) title: String,
    /// Whether it is on screen.
    pub(crate) open: bool,
    /// The owner's draw callback.
    pub(crate) draw: PanelFn,
    /// Its context, as an address.
    pub(crate) ctx: usize,
}

/// One present subscriber.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Subscriber {
    pub(crate) owner: PluginHandle,
    pub(crate) callback: PresentFn,
    pub(crate) ctx: usize,
}

static PANELS: Mutex<Vec<Panel>> = Mutex::new(Vec::new());
static SUBSCRIBERS: Mutex<Vec<Subscriber>> = Mutex::new(Vec::new());

fn panels() -> MutexGuard<'static, Vec<Panel>> {
    PANELS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn subscribers() -> MutexGuard<'static, Vec<Subscriber>> {
    SUBSCRIBERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether `name` is a valid panel name: `[a-z0-9_]{1,64}`.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Add a panel. `false` when the owner already has one by that name.
pub(crate) fn add(panel: Panel) -> bool {
    let mut list = panels();
    if list
        .iter()
        .any(|p| p.owner == panel.owner && p.name == panel.name)
    {
        return false;
    }
    log::debug!("panel {} added", panel.id);
    list.push(panel);
    true
}

/// Remove one of an owner's panels. `false` when there was none.
pub(crate) fn remove(owner: PluginHandle, name: &str) -> bool {
    let mut list = panels();
    let before = list.len();
    list.retain(|p| !(p.owner == owner && p.name == name));
    list.len() != before
}

/// Change one panel's open state with `f`. `None` when there is no such panel.
pub(crate) fn update_open(
    owner: PluginHandle,
    name: &str,
    f: impl FnOnce(bool) -> bool,
) -> Option<bool> {
    let mut list = panels();
    let panel = list
        .iter_mut()
        .find(|p| p.owner == owner && p.name == name)?;
    panel.open = f(panel.open);
    Some(panel.open)
}

/// Close a panel by its qualified id, from its window's close button.
pub(crate) fn close_id(id: &str) {
    if let Some(panel) = panels().iter_mut().find(|p| p.id == id) {
        panel.open = false;
    }
}

/// Every panel, open or not, in registration order.
pub(crate) fn snapshot() -> Vec<Panel> {
    panels().clone()
}

/// How many panels are open.
pub(crate) fn open_count() -> usize {
    panels().iter().filter(|p| p.open).count()
}

/// Forget everything an owner registered.
pub(crate) fn remove_owner(owner: PluginHandle) {
    panels().retain(|p| p.owner != owner);
    subscribers().retain(|s| s.owner != owner);
}

/// Add a present subscriber.
pub(crate) fn subscribe(subscriber: Subscriber) {
    subscribers().push(subscriber);
}

/// Every present subscriber.
pub(crate) fn present_subscribers() -> Vec<Subscriber> {
    subscribers().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn draw(_ctx: *mut core::ffi::c_void, _panel: uniloader_sdk::api::Str, _f: u64) {}

    fn panel(owner: u32, name: &str) -> Panel {
        Panel {
            owner: PluginHandle(owner),
            id: format!("p{owner}.{name}"),
            name: name.to_owned(),
            title: name.to_owned(),
            open: false,
            draw,
            ctx: 0,
        }
    }

    #[test]
    fn names_follow_the_rule() {
        assert!(valid_name("map_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("Map"));
        assert!(!valid_name("a.b"));
    }

    #[test]
    fn panels_are_per_owner_and_go_with_it() {
        assert!(add(panel(100, "map")));
        assert!(!add(panel(100, "map")), "duplicate");
        assert!(add(panel(101, "map")), "another owner may reuse the name");
        assert_eq!(update_open(PluginHandle(100), "map", |o| !o), Some(true));
        assert_eq!(update_open(PluginHandle(100), "nope", |o| !o), None);
        remove_owner(PluginHandle(100));
        assert!(!snapshot().iter().any(|p| p.owner == PluginHandle(100)));
        assert!(remove(PluginHandle(101), "map"));
    }
}
