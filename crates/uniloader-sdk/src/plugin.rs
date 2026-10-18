//! The trait a plugin implements.

use uniloader_api::StopReason;

use crate::deps::Dependency;
use crate::host::{Host, PluginError, PluginRef};

/// A loader plugin. Every callback has a no-op default.
///
/// The lifecycle, in order: [`Plugin::start`] creates the plugin and is the only place
/// registrations are allowed; callbacks run until it is paused by `plugin disable`
/// ([`Plugin::on_disable`]) and resumed ([`Plugin::on_enable`]); [`Plugin::stop`] runs once,
/// with a [`StopReason`] saying whether the plugin was unloaded or the game is exiting.
///
/// Callbacks take `&self` because the loader may re-enter the plugin (a setting changed from
/// inside an event handler), so state needs interior mutability (`Mutex`, atomics). Callbacks
/// may run on any thread, including the game's own; keep them short.
pub trait Plugin: Sized + Send + Sync + 'static {
    /// Unique short name, `[a-z0-9_-]+`. Names settings, hotkeys, commands and the config file.
    const NAME: &'static str;
    /// Human readable name for lists. Defaults to [`Plugin::NAME`].
    const TITLE: &'static str = Self::NAME;
    /// Version, dot separated numbers first.
    const VERSION: &'static str;
    /// Authors, comma separated.
    const AUTHORS: &'static str = "";
    /// One-line description.
    const DESCRIPTION: &'static str;
    /// What must be present before this plugin starts: other plugins, libraries or files.
    /// The loader checks the list and logs why it skipped the plugin, rather than letting
    /// the plugin discover the problem at run time.
    const DEPENDENCIES: &'static [Dependency] = &[];
    /// Services this plugin provides from `start`, so plugins declaring
    /// [`Dependency::service`] start after it. Providing a service not listed here still
    /// works, but nothing is ordered after it.
    const PROVIDES: &'static [&'static str] = &[];

    /// Create the plugin. Register settings, hotkeys, commands and subscriptions here;
    /// registration is only allowed during this call.
    ///
    /// # Errors
    /// Return an error to stay unloaded; the loader logs it and continues with other plugins.
    fn start(host: Host) -> Result<Self, PluginError>;

    /// The plugin is being stopped: `plugin stop`, the game exiting, or a `start` that failed
    /// partway. [`StopReason::EXIT`] is the one case where doing less is better, because the
    /// process is about to vanish anyway.
    ///
    /// Hooks registered through the host are torn down by the loader either way; this is for
    /// everything the plugin owns itself.
    fn stop(&self, _host: &Host, _reason: StopReason) {}

    /// Callback delivery resumed after `plugin disable`.
    fn on_enable(&self, _host: &Host) {}

    /// Callback delivery is being paused by `plugin disable`. The plugin stays loaded and
    /// keeps its state; nothing but this callback runs until it is enabled again.
    fn on_disable(&self, _host: &Host) {}

    /// A registered hotkey action was pressed while the game window had focus.
    fn on_hotkey(&self, _host: &Host, _action: &str) {}

    /// One of this plugin's settings changed. `value` is the canonical text form.
    fn on_setting_changed(&self, _host: &Host, _key: &str, _value: &str) {}

    /// A registered console command was invoked with its raw argument text.
    ///
    /// # Errors
    /// Return an error to have the console print it.
    fn on_command(&self, _host: &Host, _name: &str, _args: &str) -> Result<(), PluginError> {
        Err(PluginError::Unsupported)
    }

    /// Another plugin sent a direct message. Return bytes to reply synchronously.
    ///
    /// # Errors
    /// Return an error to have the sender see a failure status.
    fn on_message(
        &self,
        _host: &Host,
        _from: PluginRef,
        _topic: &str,
        _payload: &[u8],
    ) -> Result<Option<Vec<u8>>, PluginError> {
        Err(PluginError::Unsupported)
    }

    /// A subscribed broadcast topic was published.
    fn on_event(&self, _host: &Host, _from: PluginRef, _topic: &str, _payload: &[u8]) {}
}
