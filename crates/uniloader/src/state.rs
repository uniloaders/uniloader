//! The loader's registries. Pure data plus the rules for mutating it; the Windows side
//! holds this behind a mutex and performs the plugin calls that these methods *request*
//! (see [`Notify`]) after releasing the lock, so plugins can re-enter the host freely.
//!
//! From the DayZ loader's `state.rs` (`eaebd9b`) without panels, the window layout, the
//! backbuffer request and the DayZ console hooks.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use uniloader_api::{PluginHandle, Status};
use uniloader_core::{hotkeys, keys, names, settings, store};

use crate::config::{LoaderConfig, Paths};
use crate::{scrollback, services};

/// Lines the console keeps for display.
pub const CONSOLE_HISTORY: usize = 500;

/// A console command registered by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInfo {
    /// One-line help.
    pub help: String,
    /// Argument syntax.
    pub usage: String,
}

/// What a plugin's describe export said about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Described {
    /// Unique name.
    pub name: String,
    /// Human readable name.
    pub title: String,
    /// Version text.
    pub version: String,
    /// Authors, comma separated.
    pub authors: String,
    /// One-line description.
    pub description: String,
}

/// Everything the loader tracks about one plugin DLL.
#[derive(Debug)]
pub struct PluginRecord {
    /// What the describe export said.
    pub info: Described,
    /// DLL file name, for messages.
    pub file: String,
    /// Settings registry.
    pub settings: settings::Registry,
    /// Topics this plugin receives.
    pub subscriptions: BTreeSet<String>,
    /// Registered console commands by unqualified name.
    pub commands: BTreeMap<String, CommandInfo>,
    /// Dependencies the plugin declared, one display line each.
    pub dependencies: Vec<String>,
    /// Start returned success and no fault happened since.
    pub enabled: bool,
}

impl PluginRecord {
    /// Unique name from the describe export.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.info.name
    }
}

/// A DLL the loader loaded that is not a uniloader plugin: an `.asi` file, or a library listed
/// under `load` in `loader.toml` such as UE4SS. Listed with the plugins, never called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreign {
    /// File stem, for display.
    pub name: String,
    /// Path relative to the game directory.
    pub file: String,
    /// Product version from the file's version resource, or empty.
    pub version: String,
    /// `asi` or `library`.
    pub kind: &'static str,
}

/// A plugin call the Windows side must make once the state lock is released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notify {
    /// `on_setting_changed(key, value)` on `plugin`.
    SettingChanged {
        /// Owner of the setting.
        plugin: PluginHandle,
        /// Unqualified key.
        key: String,
        /// Canonical value.
        value: String,
    },
}

/// Which registrations are currently allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Before any plugin starts; registrations are refused.
    Idle,
    /// Inside the start call of this plugin.
    Starting(PluginHandle),
    /// All plugins started; registrations are refused.
    Running,
}

/// Loader state.
#[derive(Debug)]
pub struct State {
    /// Directory layout.
    pub paths: Paths,
    /// Parsed `loader.toml`.
    pub config: LoaderConfig,
    /// Plugins in load order; handle `n` is index `n - 1`.
    pub plugins: Vec<PluginRecord>,
    /// All hotkeys.
    pub hotkeys: hotkeys::Registry,
    /// User bindings from `hotkeys.toml`.
    pub hotkey_overrides: BTreeMap<String, String>,
    /// Current phase.
    pub phase: Phase,
    /// Plugins that described themselves but are waiting for a dependency: name and reason.
    ///
    /// They are not rejected, only not started yet. `plugin load <name>` starts one, and so
    /// does the dependency it waits for becoming available.
    pub pending: Vec<(String, String)>,
    /// Loaded DLLs that are not plugins, in load order.
    pub foreign: Vec<Foreign>,
    /// Services plugins provide to each other.
    pub services: services::Registry,
    /// Recent console output and log records, oldest first.
    pub console: VecDeque<scrollback::Line>,
    /// How many lines the console has ever held.
    ///
    /// The buffer above is bounded, so a position in it says nothing once it is full. This
    /// counter is what [`State::console_mark`] hands out and never goes backwards.
    pub console_printed: u64,
    /// Produces the lines the `hooks` command prints. A function pointer because the hook
    /// registry belongs to the platform layer and this module must stay free of it.
    pub hook_lines: fn() -> Vec<String>,
    /// Closes the game for `quit`, and returns what to print. A function pointer for the
    /// same reason: ending the process is the platform layer's business.
    pub quit: fn() -> Vec<String>,
}

/// Default for [`State::hook_lines`]: no platform layer, so no hooks.
fn no_hooks() -> Vec<String> {
    Vec::new()
}

/// Default for [`State::quit`]: no platform layer, so no game to close.
fn no_quit() -> Vec<String> {
    vec!["closing the game needs the platform layer".to_owned()]
}

impl State {
    /// Fresh state for a game directory.
    #[must_use]
    pub fn new(paths: Paths, config: LoaderConfig) -> Self {
        let hotkey_overrides = store::read(&paths.hotkeys_file()).unwrap_or_else(|e| {
            log::warn!("ignoring hotkey overrides: {e}");
            BTreeMap::new()
        });
        State {
            paths,
            config,
            plugins: Vec::new(),
            hotkeys: hotkeys::Registry::default(),
            hotkey_overrides,
            phase: Phase::Idle,
            pending: Vec::new(),
            foreign: Vec::new(),
            services: services::Registry::default(),
            console: VecDeque::new(),
            console_printed: 0,
            hook_lines: no_hooks,
            quit: no_quit,
        }
    }

    /// Record a plugin after a successful describe; returns its handle.
    ///
    /// # Errors
    /// [`Status::INVALID_ARGUMENT`] for a name that breaks the naming rule or is taken.
    pub fn add_plugin(&mut self, info: Described, file: &str) -> Result<PluginHandle, Status> {
        if names::plugin_name(&info.name).is_err() || self.find_plugin(&info.name).is_some() {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.plugins.push(PluginRecord {
            info,
            file: file.to_owned(),
            settings: settings::Registry::default(),
            subscriptions: BTreeSet::new(),
            commands: BTreeMap::new(),
            dependencies: Vec::new(),
            enabled: false,
        });
        Ok(PluginHandle(
            u32::try_from(self.plugins.len()).map_err(|_| Status::ERROR)?,
        ))
    }

    /// Record by handle.
    #[must_use]
    pub fn plugin(&self, handle: PluginHandle) -> Option<&PluginRecord> {
        handle
            .0
            .checked_sub(1)
            .and_then(|i| self.plugins.get(i as usize))
    }

    fn plugin_mut(&mut self, handle: PluginHandle) -> Option<&mut PluginRecord> {
        handle
            .0
            .checked_sub(1)
            .and_then(|i| self.plugins.get_mut(i as usize))
    }

    /// Handle of a recorded plugin by name.
    #[must_use]
    pub fn find_plugin(&self, name: &str) -> Option<PluginHandle> {
        self.plugins
            .iter()
            .position(|p| p.name() == name)
            .and_then(|i| u32::try_from(i + 1).ok())
            .map(PluginHandle)
    }

    fn require_starting(&self, plugin: PluginHandle) -> Result<(), Status> {
        if self.phase == Phase::Starting(plugin) {
            Ok(())
        } else {
            Err(Status::WRONG_PHASE)
        }
    }

    /// Register a setting for the plugin currently starting.
    ///
    /// # Errors
    /// Outside the plugin's start, a bad key, a duplicate or a bad descriptor.
    pub fn register_setting(
        &mut self,
        plugin: PluginHandle,
        desc: settings::Desc,
    ) -> Result<(), Status> {
        self.require_starting(plugin)?;
        names::setting_key(&desc.key).map_err(|_| Status::INVALID_ARGUMENT)?;
        let record = self.plugin(plugin).ok_or(Status::NOT_FOUND)?;
        let stored = crate::config::load_plugin_values(&self.paths, record.name());
        let value = stored.get(&desc.key).map(String::as_str);
        let record = self.plugin_mut(plugin).ok_or(Status::NOT_FOUND)?;
        let name = record.info.name.clone();
        record.settings.register(desc, value).map_err(|e| {
            log::error!("{name}: {e}");
            match e {
                settings::SettingError::Duplicate(_) => Status::ALREADY_EXISTS,
                _ => Status::INVALID_ARGUMENT,
            }
        })
    }

    /// Resolve `name` (own key or `plugin.key`) relative to `caller`.
    fn resolve(&self, caller: Option<PluginHandle>, name: &str) -> Option<(PluginHandle, String)> {
        if let Some(handle) = caller {
            if self.plugin(handle)?.settings.desc(name).is_some() {
                return Some((handle, name.to_owned()));
            }
        }
        // Plugin names contain no dots, so the first dot always separates plugin and key.
        let (plugin, key) = names::split_qualified(name)?;
        let handle = self.find_plugin(plugin)?;
        self.plugin(handle)?.settings.desc(key)?;
        Some((handle, key.to_owned()))
    }

    /// Read a setting as text.
    ///
    /// # Errors
    /// [`Status::NOT_FOUND`] for an unknown setting.
    pub fn get_setting(&self, caller: Option<PluginHandle>, name: &str) -> Result<String, Status> {
        let (handle, key) = self.resolve(caller, name).ok_or(Status::NOT_FOUND)?;
        let record = self.plugin(handle).ok_or(Status::NOT_FOUND)?;
        record
            .settings
            .get(&key)
            .map(str::to_owned)
            .ok_or(Status::NOT_FOUND)
    }

    /// Validate, store and persist a setting. Returns the notification to deliver, if any.
    ///
    /// # Errors
    /// The status and the validation message.
    pub fn set_setting(
        &mut self,
        caller: Option<PluginHandle>,
        name: &str,
        value: &str,
    ) -> Result<Option<Notify>, (Status, String)> {
        let (handle, key) = self
            .resolve(caller, name)
            .ok_or((Status::NOT_FOUND, format!("unknown setting {name}")))?;
        let owner = self
            .plugin(handle)
            .map(|p| p.name().to_owned())
            .unwrap_or_default();
        let config_path = self.paths.plugin_config(&owner);
        let record = self
            .plugin_mut(handle)
            .ok_or((Status::NOT_FOUND, String::new()))?;
        let changed = record
            .settings
            .set(&key, value)
            .map_err(|e| (Status::INVALID_ARGUMENT, e.to_string()))?;
        if changed.persist && changed.differs {
            if let Err(e) = store::write(&config_path, &record.settings.persistent_values()) {
                log::error!("could not save settings: {e}");
            }
        }
        if !changed.differs {
            return Ok(None);
        }
        Ok(Some(Notify::SettingChanged {
            plugin: handle,
            key,
            value: changed.value,
        }))
    }

    /// Register a hotkey for the plugin currently starting. Returns the qualified name.
    ///
    /// # Errors
    /// Outside the plugin's start, a bad name, a duplicate or an unparsable default.
    pub fn register_hotkey(
        &mut self,
        plugin: PluginHandle,
        action: &str,
        title: &str,
        default: &str,
    ) -> Result<String, Status> {
        self.require_starting(plugin)?;
        names::action_name(action).map_err(|_| Status::INVALID_ARGUMENT)?;
        let name = format!(
            "{}.{action}",
            self.plugin(plugin).ok_or(Status::NOT_FOUND)?.name()
        );
        match self
            .hotkeys
            .register(&name, title, default, &self.hotkey_overrides)
        {
            Ok(None) => Ok(name),
            Ok(Some(bad_override)) => {
                log::warn!("hotkeys.toml: {name}: {bad_override}; using default {default:?}");
                Ok(name)
            }
            Err(hotkeys::HotkeyError::Duplicate(_)) => Err(Status::ALREADY_EXISTS),
            Err(e) => {
                log::error!("{e}");
                Err(Status::INVALID_ARGUMENT)
            }
        }
    }

    /// Register a console command for the plugin currently starting.
    ///
    /// # Errors
    /// Outside the plugin's start, a bad name or a duplicate.
    pub fn register_command(
        &mut self,
        plugin: PluginHandle,
        name: &str,
        info: CommandInfo,
    ) -> Result<(), Status> {
        self.require_starting(plugin)?;
        names::action_name(name).map_err(|_| Status::INVALID_ARGUMENT)?;
        let record = self.plugin_mut(plugin).ok_or(Status::NOT_FOUND)?;
        if record.commands.contains_key(name) {
            return Err(Status::ALREADY_EXISTS);
        }
        record.commands.insert(name.to_owned(), info);
        Ok(())
    }

    /// Subscribe the plugin currently starting to a topic.
    ///
    /// # Errors
    /// Outside the plugin's start, or an empty topic.
    pub fn subscribe(&mut self, plugin: PluginHandle, topic: &str) -> Result<(), Status> {
        self.require_starting(plugin)?;
        if topic.is_empty() {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.plugin_mut(plugin)
            .ok_or(Status::NOT_FOUND)?
            .subscriptions
            .insert(topic.to_owned());
        Ok(())
    }

    /// Enabled plugins subscribed to `topic`, excluding `from`.
    #[must_use]
    pub fn subscribers(&self, from: PluginHandle, topic: &str) -> Vec<PluginHandle> {
        self.plugins
            .iter()
            .enumerate()
            .filter(|(_, p)| p.enabled && p.subscriptions.contains(topic))
            .filter_map(|(i, _)| u32::try_from(i + 1).ok().map(PluginHandle))
            .filter(|h| *h != from)
            .collect()
    }

    /// Register a service for a running or starting plugin.
    ///
    /// # Errors
    /// See [`services::Registry::register`]; [`Status::NOT_FOUND`] for an unknown plugin.
    pub fn register_service(
        &mut self,
        plugin: PluginHandle,
        name: &str,
        version: u32,
        table: usize,
    ) -> Result<(), Status> {
        self.plugin(plugin).ok_or(Status::NOT_FOUND)?;
        self.services.register(
            name,
            services::Service {
                provider: plugin,
                version,
                table,
            },
        )
    }

    /// The service registered under `name`, if its provider is running.
    #[must_use]
    pub fn service(&self, name: &str) -> Option<services::Service> {
        self.services
            .get(name)
            .filter(|s| self.plugin(s.provider).is_some_and(|p| p.enabled))
    }

    /// Bind a hotkey action to `binding` (key grammar, chords separated by commas; empty
    /// unbinds) and save it to `hotkeys.toml`. A binding equal to the default removes the
    /// override, so a changed default still reaches the user later.
    ///
    /// # Errors
    /// [`Status::NOT_FOUND`] for an unknown action, [`Status::INVALID_ARGUMENT`] for a
    /// binding that does not parse.
    pub fn rebind_hotkey(&mut self, name: &str, binding: &str) -> Result<(), Status> {
        let chords = keys::parse_list(binding).map_err(|_| Status::INVALID_ARGUMENT)?;
        let default = self
            .hotkeys
            .iter()
            .find(|e| e.name == name)
            .map(hotkeys::Entry::default_binding)
            .ok_or(Status::NOT_FOUND)?;
        let canonical = keys::format_list(&chords);
        if !self.hotkeys.rebind_all(name, chords) {
            return Err(Status::NOT_FOUND);
        }
        if canonical == default {
            self.hotkey_overrides.remove(name);
        } else {
            self.hotkey_overrides.insert(name.to_owned(), canonical);
        }
        if let Err(e) = store::write(&self.paths.hotkeys_file(), &self.hotkey_overrides) {
            log::error!("could not save hotkeys: {e}");
        }
        Ok(())
    }

    /// Note that a plugin is waiting for a dependency, replacing any earlier note.
    pub fn set_pending(&mut self, plugin: &str, reason: &str) {
        self.pending.retain(|(name, _)| name != plugin);
        self.pending.push((plugin.to_owned(), reason.to_owned()));
    }

    /// Forget that a plugin was waiting, because it started or will never start.
    pub fn clear_pending(&mut self, plugin: &str) {
        self.pending.retain(|(name, _)| name != plugin);
    }

    /// Record the dependencies a plugin declared, for the console to show.
    pub fn set_dependencies(
        &mut self,
        plugin: PluginHandle,
        lines: impl IntoIterator<Item = String>,
    ) {
        if let Some(record) = self.plugin_mut(plugin) {
            record.dependencies = lines.into_iter().collect();
        }
    }

    /// Mark a plugin started or stopped/faulted.
    pub fn set_enabled(&mut self, plugin: PluginHandle, enabled: bool) {
        if let Some(record) = self.plugin_mut(plugin) {
            record.enabled = enabled;
        }
    }

    /// Append a console line, trimming history.
    ///
    /// Queued log records are taken first, so a command's output lands after whatever the
    /// loader logged on the way to producing it rather than in front of it.
    pub fn console_print(&mut self, line: String) {
        // Under a target the scrollback drops, so the log file carries the console session
        // without the console showing every line twice. See `scrollback::ECHO_TARGET`.
        log::debug!(target: scrollback::ECHO_TARGET, "{line}");
        self.console_drain();
        let kind = if line.starts_with("> ") {
            scrollback::Kind::Echo
        } else {
            scrollback::Kind::Output
        };
        self.console_push(kind, String::new(), line);
    }

    /// Move everything the logger has queued into the buffer.
    pub fn console_drain(&mut self) {
        for (level, target, message) in scrollback::take() {
            self.console_push(scrollback::Kind::Log(level), target, message);
        }
    }

    fn console_push(&mut self, kind: scrollback::Kind, target: String, text: String) {
        if self.console.len() >= CONSOLE_HISTORY {
            self.console.pop_front();
        }
        self.console_printed += 1;
        self.console.push_back(scrollback::Line {
            seq: self.console_printed,
            kind,
            clock: scrollback::clock(),
            target,
            text,
        });
    }

    /// Remember where the console is, for [`State::console_since`].
    #[must_use]
    pub fn console_mark(&self) -> u64 {
        self.console_printed
    }

    /// Every line a command printed since `mark`, oldest first.
    ///
    /// Log records are left out: this is what a plugin calling `console_exec` gets back as
    /// the answer to its own line. Lines the bounded buffer has already dropped cannot be
    /// returned, so a command that printed more than the whole history is reported from
    /// where the history starts.
    #[must_use]
    pub fn console_since(&self, mark: u64) -> Vec<String> {
        self.console
            .iter()
            .filter(|line| line.seq > mark && !matches!(line.kind, scrollback::Kind::Log(_)))
            .map(scrollback::Line::flat)
            .collect()
    }

    /// Every line since `mark`, log records included, oldest first: the whole scrollback a
    /// console view shows.
    #[must_use]
    pub fn console_all_since(&self, mark: u64) -> Vec<scrollback::Line> {
        self.console
            .iter()
            .filter(|line| line.seq > mark)
            .cloned()
            .collect()
    }

    /// Every name the console completes: built-ins, plugin commands, qualified settings,
    /// hotkey actions and plugin names, sorted and without repeats.
    #[must_use]
    pub fn completion_names(&self) -> Vec<String> {
        let mut names: BTreeSet<String> = uniloader_core::console::BUILTIN_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        for plugin in &self.plugins {
            let owner = plugin.name();
            names.insert(owner.to_owned());
            names.extend(plugin.commands.keys().map(|c| format!("{owner}.{c}")));
            names.extend(
                plugin
                    .settings
                    .iter()
                    .map(|(desc, _)| format!("{owner}.{}", desc.key)),
            );
        }
        names.extend(self.hotkeys.iter().map(|entry| entry.name.clone()));
        names.into_iter().collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn state() -> State {
        let dir = std::env::temp_dir().join(format!(
            "uniloader-state-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        State::new(Paths::from_game_dir(&dir), LoaderConfig::default())
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn described(name: &str) -> Described {
        Described {
            name: name.to_owned(),
            version: "1".to_owned(),
            ..Described::default()
        }
    }

    pub(crate) fn desc(key: &str) -> settings::Desc {
        settings::Desc {
            key: key.into(),
            title: String::new(),
            description: String::new(),
            kind: settings::Kind::Int,
            default: "1".into(),
            min: 0.0,
            max: 10.0,
            choices: Vec::new(),
            restart_required: false,
            advanced: false,
            transient: false,
        }
    }

    pub(crate) fn started(s: &mut State, name: &str) -> PluginHandle {
        let h = s
            .add_plugin(described(name), &format!("{name}.dll"))
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.phase = Phase::Starting(h);
        h
    }

    #[test]
    fn registrations_require_start_phase() {
        let mut s = state();
        let h = s
            .add_plugin(described("a"), "a.dll")
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(s.register_setting(h, desc("x")), Err(Status::WRONG_PHASE));
        s.phase = Phase::Starting(h);
        assert_eq!(s.register_setting(h, desc("x")), Ok(()));
        assert_eq!(
            s.register_setting(h, desc("x")),
            Err(Status::ALREADY_EXISTS)
        );
        assert_eq!(
            s.register_setting(h, desc("Bad Key")),
            Err(Status::INVALID_ARGUMENT)
        );
        assert_eq!(
            s.register_hotkey(h, "go", "Go", "f5"),
            Ok("a.go".to_owned())
        );
        assert_eq!(
            s.register_hotkey(h, "go", "Go", "f5"),
            Err(Status::ALREADY_EXISTS)
        );
        assert_eq!(
            s.register_hotkey(h, "bad", "Bad", "nokey"),
            Err(Status::INVALID_ARGUMENT)
        );
        s.phase = Phase::Running;
        assert_eq!(s.subscribe(h, "late"), Err(Status::WRONG_PHASE));
        assert_eq!(
            s.add_plugin(described("a"), "a2.dll"),
            Err(Status::INVALID_ARGUMENT)
        );
    }

    #[test]
    fn settings_resolve_own_and_qualified_names() {
        let mut s = state();
        let a = started(&mut s, "a");
        s.register_setting(a, desc("x"))
            .unwrap_or_else(|e| panic!("{e:?}"));
        let b = started(&mut s, "b");
        s.register_setting(b, desc("x"))
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.phase = Phase::Running;
        s.set_enabled(a, true);
        s.set_enabled(b, true);

        assert_eq!(s.get_setting(Some(a), "x"), Ok("1".into()));
        assert_eq!(s.get_setting(None, "x"), Err(Status::NOT_FOUND));
        assert_eq!(s.get_setting(None, "b.x"), Ok("1".into()));
        let notify = s
            .set_setting(Some(a), "b.x", "7")
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            notify,
            Some(Notify::SettingChanged {
                plugin: b,
                key: "x".into(),
                value: "7".into()
            })
        );
        assert_eq!(
            s.set_setting(None, "b.x", "7"),
            Ok(None),
            "unchanged value notifies nobody"
        );
        assert!(matches!(
            s.set_setting(None, "b.x", "99"),
            Err((Status::INVALID_ARGUMENT, _))
        ));
        assert!(matches!(
            s.set_setting(None, "c.x", "1"),
            Err((Status::NOT_FOUND, _))
        ));
        assert!(s.paths.plugin_config("b").exists(), "persisted to disk");
        assert_eq!(crate::config::load_plugin_values(&s.paths, "b")["x"], "7");
        let _ = std::fs::remove_dir_all(&s.paths.game_dir);
    }

    #[test]
    fn subscribers_exclude_sender_and_disabled() {
        let mut s = state();
        let a = started(&mut s, "a");
        s.subscribe(a, "pose").unwrap_or_else(|e| panic!("{e:?}"));
        let b = started(&mut s, "b");
        s.subscribe(b, "pose").unwrap_or_else(|e| panic!("{e:?}"));
        let c = started(&mut s, "c");
        s.subscribe(c, "pose").unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(s.subscribe(c, ""), Err(Status::INVALID_ARGUMENT));
        for h in [a, b] {
            s.set_enabled(h, true);
        }
        assert_eq!(s.subscribers(a, "pose"), vec![b]);
        assert_eq!(s.subscribers(a, "other"), Vec::<PluginHandle>::new());
    }

    #[test]
    fn services_need_a_running_provider() {
        let mut s = state();
        let a = started(&mut s, "a");
        assert_eq!(s.register_service(a, "overlay", 1, 0x10), Ok(()));
        assert_eq!(
            s.register_service(PluginHandle(9), "x", 1, 0x10),
            Err(Status::NOT_FOUND)
        );
        assert_eq!(s.service("overlay"), None, "not running yet");
        s.set_enabled(a, true);
        assert_eq!(s.service("overlay").map(|x| x.table), Some(0x10));
        s.set_enabled(a, false);
        assert_eq!(s.service("overlay"), None, "paused provider");
    }

    #[test]
    fn rebinding_persists_only_differences_from_the_default() {
        let mut s = state();
        let a = started(&mut s, "a");
        s.register_hotkey(a, "go", "Go", "f5")
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(s.rebind_hotkey("a.go", "ctrl+f6, f7"), Ok(()));
        assert_eq!(s.hotkey_overrides["a.go"], "ctrl+f6, f7");
        assert_eq!(
            store::read(&s.paths.hotkeys_file()).unwrap_or_default()["a.go"],
            "ctrl+f6, f7"
        );
        assert_eq!(s.rebind_hotkey("a.go", "f5"), Ok(()));
        assert!(!s.hotkey_overrides.contains_key("a.go"));
        assert_eq!(s.rebind_hotkey("a.nope", "f5"), Err(Status::NOT_FOUND));
        assert_eq!(
            s.rebind_hotkey("a.go", "nokey"),
            Err(Status::INVALID_ARGUMENT)
        );
        let _ = std::fs::remove_dir_all(&s.paths.game_dir);
    }

    #[test]
    fn captured_output_survives_a_full_history() {
        let mut s = state();
        // Fill the buffer first: a position in it is then meaningless, which is the bug this
        // guards against — capture used to skip by index and returned nothing.
        for i in 0..(CONSOLE_HISTORY + 5) {
            s.console_print(format!("old {i}"));
        }
        let mark = s.console_mark();
        s.console_print("new 1".to_owned());
        s.console_print("new 2".to_owned());
        assert_eq!(
            s.console_since(mark),
            vec!["new 1".to_owned(), "new 2".to_owned()]
        );
    }

    #[test]
    fn capture_cannot_return_evicted_lines() {
        let mut s = state();
        let mark = s.console_mark();
        for i in 0..(CONSOLE_HISTORY + 5) {
            s.console_print(i.to_string());
        }
        let captured = s.console_since(mark);
        assert_eq!(captured.len(), CONSOLE_HISTORY);
        assert_eq!(captured.first().map(String::as_str), Some("5"));
    }

    #[test]
    fn console_view_includes_log_records() {
        let mut s = state();
        let mark = s.console_mark();
        s.console_push(
            scrollback::Kind::Log(log::Level::Warn),
            "t".to_owned(),
            "careful".to_owned(),
        );
        s.console_print("out".to_owned());
        assert_eq!(s.console_since(mark), vec!["out".to_owned()]);
        let all = s.console_all_since(mark);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].kind, scrollback::Kind::Log(log::Level::Warn));
        assert_eq!(
            (all[0].target.as_str(), all[0].text.as_str()),
            ("t", "careful")
        );
        assert_eq!(all[0].clock.len(), "HH:MM:SS".len());
    }

    #[test]
    fn completion_names_include_builtins() {
        let s = state();
        let names = s.completion_names();
        assert!(names.iter().any(|n| n == "plugin load"));
        assert!(names.windows(2).all(|w| w[0] < w[1]), "sorted, no repeats");
    }

    #[test]
    fn console_history_is_bounded() {
        let mut s = state();
        for i in 0..(CONSOLE_HISTORY + 5) {
            s.console_print(i.to_string());
        }
        assert_eq!(s.console.len(), CONSOLE_HISTORY);
        assert_eq!(s.console.front().map(|line| line.text.as_str()), Some("5"));
    }
}
