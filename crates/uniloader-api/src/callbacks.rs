//! Function table a plugin fills in during `start`. Every entry is optional.

use core::ffi::c_void;

use crate::host::ReplyFn;
use crate::types::{Bytes, PluginHandle, Status, Str};

/// Callbacks a plugin exposes. Null entries are skipped.
///
/// The loader guards every call against panics and hardware faults and disables a plugin
/// that faults. Hotkeys arrive on the loader's input thread; messages and events on the
/// thread of whoever sent them, which for an adapter's frame event is the game's own thread.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginCallbacks {
    /// `size_of::<PluginCallbacks>()` as compiled into the plugin.
    pub struct_size: usize,
    /// Opaque plugin state passed as the first argument to every callback and to `stop`.
    pub ctx: *mut c_void,
    /// A registered hotkey action was pressed.
    pub on_hotkey: Option<unsafe extern "C" fn(ctx: *mut c_void, action: Str)>,
    /// One of this plugin's settings changed (console, another plugin or config reload).
    pub on_setting_changed: Option<unsafe extern "C" fn(ctx: *mut c_void, key: Str, value: Str)>,
    /// A registered console command was invoked with the raw argument text.
    pub on_command: Option<unsafe extern "C" fn(ctx: *mut c_void, name: Str, args: Str) -> Status>,
    /// Another plugin sent a direct message.
    pub on_message: Option<
        unsafe extern "C" fn(
            ctx: *mut c_void,
            from: PluginHandle,
            topic: Str,
            payload: Bytes,
            reply: Option<ReplyFn>,
            reply_ctx: *mut c_void,
        ) -> Status,
    >,
    /// A subscribed broadcast topic was published.
    pub on_event: Option<
        unsafe extern "C" fn(ctx: *mut c_void, from: PluginHandle, topic: Str, payload: Bytes),
    >,
    /// Callback delivery resumed after a pause. Never called for the initial start.
    pub on_enable: Option<unsafe extern "C" fn(ctx: *mut c_void)>,
    /// Callback delivery is being paused. The plugin keeps its state and stays loaded.
    ///
    /// Not called when the plugin faulted: a plugin whose callback just crashed is not asked
    /// to run more code.
    pub on_disable: Option<unsafe extern "C" fn(ctx: *mut c_void)>,
    /// The key of a registered hotkey action went up after `on_hotkey` fired for it, for
    /// actions that last while a key is held. Not reported for `sc..` binds.
    pub on_hotkey_release: Option<unsafe extern "C" fn(ctx: *mut c_void, action: Str)>,
}

impl PluginCallbacks {
    /// A table with no callbacks, for the loader to pass into `start`.
    #[must_use]
    pub const fn empty() -> Self {
        PluginCallbacks {
            struct_size: core::mem::size_of::<PluginCallbacks>(),
            ctx: core::ptr::null_mut(),
            on_hotkey: None,
            on_setting_changed: None,
            on_command: None,
            on_message: None,
            on_event: None,
            on_enable: None,
            on_disable: None,
            on_hotkey_release: None,
        }
    }
}
