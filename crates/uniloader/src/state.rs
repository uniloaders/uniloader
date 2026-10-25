//! The loader's registries. Pure data plus the rules for mutating it; the Windows side
//! holds this behind a mutex and performs the plugin calls that these methods *request*
//! (see [`Notify`]) after releasing the lock, so plugins can re-enter the host freely.
//!
//! From the DayZ loader's `state.rs` (`eaebd9b`) without panels, the window layout, the
//! backbuffer request and the DayZ console hooks.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use uniloader_api::{PluginHandle, PluginState, Status};
use uniloader_core::{binds, keys, names, settings, store};

use crate::config::{LoaderConfig, Paths};
use crate::described::{Described, Latest};
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
    /// [`PluginState::STOPPED`] or [`PluginState::FAULTED`] once it ended: for good this
    /// session, unlike a pause (`plugin disable`), so it is never enabled again.
    pub ended: Option<PluginState>,
    /// Why the running game does not match its target filters; empty when it does.
    pub mismatch: Vec<String>,
    /// What the update check found: the latest version and where to download it.
    pub latest: Option<Latest>,
}

impl PluginRecord {
    /// Running, paused (disabled), stopped or faulted.
    #[must_use]
    pub fn state(&self) -> PluginState {
        match (self.ended, self.enabled) {
            (Some(ended), _) => ended,
            (None, true) => PluginState::RUNNING,
            (None, false) => PluginState::DISABLED,
        }
    }

    /// Unique name from the describe export.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.info.name
    }
}

/// What a plugin entry with no DLL behind it registers: the loader's own (see `speed`).
#[derive(Debug, Clone, PartialEq)]
pub struct Builtin {
    /// File name shown in the plugin list.
    pub file: String,
    /// Its settings.
    pub settings: Vec<settings::Desc>,
    /// Its hotkey actions: name, title and default binding.
    pub hotkeys: Vec<(String, String, String)>,
    /// Its console commands.
    pub commands: Vec<(String, CommandInfo)>,
    /// Keys of its settings stored as a hash (see [`State::hashed`]).
    pub hashed: Vec<String>,
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

/// What [`State::set_setting`] changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    /// The notification to deliver; `None` when the value stayed the same.
    pub notify: Option<Notify>,
    /// The settings file to write; `None` for a transient or unchanged setting.
    pub persist: Option<Persist>,
}

/// A plugin's settings file to write once the state lock is released: a file write must
/// not hold up every other plugin and the hotkey thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persist {
    path: std::path::PathBuf,
    body: Body,
    /// Handed out under the state lock, so it orders writes of one file as the sets were.
    seq: u64,
}

/// What a [`Persist`] writes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Body {
    /// A plugin's settings, as a flat TOML table.
    Values(BTreeMap<String, String>),
    /// A whole text file (`binds.cfg`), replaced atomically.
    Text(String),
}

/// Write `text` to `path` through a temporary file and a rename, so a crash mid-write leaves
/// the old file rather than half of the new one.
pub(crate) fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

impl Persist {
    /// Hands the file to the writer thread ([`crate::persist`]), which writes the newest
    /// version of each file a moment later: the caller may be the game's or the render
    /// thread, and a slider drag sets a value every frame.
    pub fn queue(self) {
        crate::persist::queue(self);
    }

    /// The file this writes.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Its place in the order of sets; a higher one is newer.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Writes the file now, unless a later set of it was written already (two sets can race
    /// between the lock and here). Writes of one file wait for each other; writes of
    /// different files do not.
    pub fn write(self) {
        type Last = std::sync::Arc<std::sync::Mutex<u64>>;
        static FILES: std::sync::Mutex<BTreeMap<std::path::PathBuf, Last>> =
            std::sync::Mutex::new(BTreeMap::new());
        let file = std::sync::Arc::clone(
            FILES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(self.path.clone())
                .or_default(),
        );
        let mut last = file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *last > self.seq {
            return;
        }
        match &self.body {
            Body::Values(values) => {
                if let Err(e) = store::write(&self.path, values) {
                    log::error!("could not save settings: {e}");
                }
            }
            Body::Text(text) => {
                if let Err(e) = write_atomic(&self.path, text) {
                    log::error!("could not save {}: {e}", self.path.display());
                }
            }
        }
        *last = self.seq;
    }
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
    /// Every bind, plugin suggestion and alias, and the key state.
    pub binds: binds::Table,
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
    /// Plugins claiming the keyboard, oldest first; the newest running one holds it, and
    /// its hotkeys are then the only ones that fire. See [`State::claim_input`].
    pub input_claims: Vec<PluginHandle>,
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
    /// The stored values of the plugin starting now, read before its start so that each
    /// [`State::register_setting`] does not read the file again under the lock.
    pub start_values: Option<(PluginHandle, BTreeMap<String, String>)>,
    /// Keys the game uses, with who said so (`game` from `loader.toml`, or an adapter
    /// plugin's label), so a bind on one can be pointed out.
    pub reserved: Vec<(keys::Chord, String)>,
    /// The running game's properties, for target filters and the `facts` command.
    pub facts: uniloader_core::target::Facts,
    /// Settings (`plugin.key`) stored as an Argon2id hash of what is set, because the loader
    /// only ever verifies them: the RCON password.
    pub hashed: BTreeSet<String>,
    /// The last [`Persist::seq`] handed out.
    persist_seq: u64,
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
        let mut binds = binds::Table::default();
        let file = paths.binds_file();
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                for warning in binds.load(&text) {
                    log::warn!("{}: {warning}", file.display());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("{}: {e}", file.display()),
        }
        let game_keys = config.game_keys.join(",");
        let mut state = State {
            paths,
            config,
            plugins: Vec::new(),
            binds,
            phase: Phase::Idle,
            pending: Vec::new(),
            foreign: Vec::new(),
            services: services::Registry::default(),
            input_claims: Vec::new(),
            console: VecDeque::new(),
            console_printed: 0,
            hook_lines: no_hooks,
            quit: no_quit,
            start_values: None,
            hashed: BTreeSet::new(),
            reserved: Vec::new(),
            facts: uniloader_core::target::Facts::new(),
            persist_seq: 0,
        };
        if state.reserve_keys("used by the game", &game_keys).is_err() {
            log::warn!("loader.toml: game_keys {game_keys:?} has a key that does not parse");
        }
        state
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
            ended: None,
            mismatch: Vec::new(),
            latest: None,
        });
        Ok(PluginHandle(
            u32::try_from(self.plugins.len()).map_err(|_| Status::ERROR)?,
        ))
    }

    /// Record a plugin entry that has no DLL and register everything it declares, as if it
    /// had done so in its start. It is running from then on; nothing ever stops it.
    ///
    /// # Errors
    /// What [`State::add_plugin`] and the registrations refuse.
    pub fn add_builtin(
        &mut self,
        info: Described,
        builtin: Builtin,
    ) -> Result<PluginHandle, Status> {
        let handle = self.add_plugin(info, &builtin.file)?;
        let phase = std::mem::replace(&mut self.phase, Phase::Starting(handle));
        let registered = self.register_builtin(handle, builtin);
        self.phase = phase;
        registered?;
        self.set_enabled(handle, true);
        Ok(handle)
    }

    fn register_builtin(&mut self, handle: PluginHandle, builtin: Builtin) -> Result<(), Status> {
        for desc in builtin.settings {
            self.register_setting(handle, desc)?;
        }
        for (action, title, default) in &builtin.hotkeys {
            self.register_hotkey(handle, action, title, default)?;
        }
        for (name, info) in builtin.commands {
            self.register_command(handle, &name, info)?;
        }
        let owner = self
            .plugin(handle)
            .map(|p| p.name().to_owned())
            .unwrap_or_default();
        self.hashed
            .extend(builtin.hashed.iter().map(|key| format!("{owner}.{key}")));
        Ok(())
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
        let read;
        let stored = match &self.start_values {
            Some((handle, values)) if *handle == plugin => values,
            _ => {
                read = crate::config::load_plugin_values(&self.paths, record.name());
                &read
            }
        };
        let value = stored.get(&desc.key).cloned();
        let record = self.plugin_mut(plugin).ok_or(Status::NOT_FOUND)?;
        let name = record.info.name.clone();
        record
            .settings
            .register(desc, value.as_deref())
            .map_err(|e| {
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

    /// The descriptor of a setting, resolved like [`State::get_setting`] resolves its name.
    #[must_use]
    pub fn setting_desc(
        &self,
        caller: Option<PluginHandle>,
        name: &str,
    ) -> Option<&settings::Desc> {
        let (handle, key) = self.resolve(caller, name)?;
        self.plugin(handle)?.settings.desc(&key)
    }

    /// A setting as the console shows it: its value, or `<set>`/`<unset>` for a secret.
    ///
    /// # Errors
    /// [`Status::NOT_FOUND`] for an unknown setting.
    pub fn shown_setting(
        &self,
        caller: Option<PluginHandle>,
        name: &str,
    ) -> Result<String, Status> {
        let value = self.get_setting(caller, name)?;
        if self.setting_desc(caller, name).is_some_and(|d| d.secret) {
            return Ok(crate::secret::shown(&value).to_owned());
        }
        Ok(value)
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

    /// Validate and store a setting. Returns the notification to deliver and the file to
    /// write, once the lock is released.
    ///
    /// # Errors
    /// The status and the validation message.
    pub fn set_setting(
        &mut self,
        caller: Option<PluginHandle>,
        name: &str,
        value: &str,
    ) -> Result<Changed, (Status, String)> {
        let (handle, key) = self
            .resolve(caller, name)
            .ok_or((Status::NOT_FOUND, format!("unknown setting {name}")))?;
        let owner = self
            .plugin(handle)
            .map(|p| p.name().to_owned())
            .unwrap_or_default();
        let config_path = self.paths.plugin_config(&owner);
        let hashed;
        let value = if self.hashed.contains(&format!("{owner}.{key}"))
            && !value.is_empty()
            && !crate::secret::is_hash(value)
        {
            if value.chars().count() < crate::rcon::MIN_PASSWORD {
                return Err((
                    Status::INVALID_ARGUMENT,
                    format!(
                        "{name}: use at least {} characters",
                        crate::rcon::MIN_PASSWORD
                    ),
                ));
            }
            hashed = crate::secret::hash(value).map_err(|e| (Status::ERROR, e))?;
            hashed.as_str()
        } else {
            value
        };
        let record = self
            .plugin_mut(handle)
            .ok_or((Status::NOT_FOUND, String::new()))?;
        let changed = record
            .settings
            .set(&key, value)
            .map_err(|e| (Status::INVALID_ARGUMENT, e.to_string()))?;
        if !changed.differs {
            return Ok(Changed {
                notify: None,
                persist: None,
            });
        }
        let values = changed.persist.then(|| record.settings.persistent_values());
        let persist = values.map(|values| {
            self.persist_seq += 1;
            Persist {
                path: config_path,
                body: Body::Values(values),
                seq: self.persist_seq,
            }
        });
        Ok(Changed {
            notify: Some(Notify::SettingChanged {
                plugin: handle,
                key,
                value: changed.value,
            }),
            persist,
        })
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
        self.register_bind(&name, title, default, binds::Target::Hotkey)?;
        Ok(name)
    }

    /// Register a bind the plugin currently starting suggests: `name` runs `script` on the
    /// keys in `default` until the user moves or removes it. Returns the qualified name.
    ///
    /// # Errors
    /// Outside the plugin's start, a bad name, a duplicate or an unparsable default.
    pub fn register_suggestion(
        &mut self,
        plugin: PluginHandle,
        name: &str,
        title: &str,
        default: &str,
        script: &str,
    ) -> Result<String, Status> {
        self.require_starting(plugin)?;
        names::action_name(name).map_err(|_| Status::INVALID_ARGUMENT)?;
        uniloader_core::script::commands(script).map_err(|_| Status::INVALID_ARGUMENT)?;
        let qualified = format!(
            "{}.{name}",
            self.plugin(plugin).ok_or(Status::NOT_FOUND)?.name()
        );
        let target = binds::Target::Script(script.to_owned());
        self.register_bind(&qualified, title, default, target)?;
        Ok(qualified)
    }

    fn register_bind(
        &mut self,
        name: &str,
        title: &str,
        default: &str,
        target: binds::Target,
    ) -> Result<(), Status> {
        match self.binds.register(name, title, default, target) {
            Ok(warnings) => {
                for warning in warnings {
                    log::warn!("{warning}");
                }
                let chords: Vec<keys::Chord> = self
                    .binds
                    .entry(name)
                    .map(|e| e.chords().collect())
                    .unwrap_or_default();
                for chord in chords {
                    if let Some(label) = self.reserved_by(chord) {
                        log::warn!("{name}: {chord} is also {label}; both will run");
                    }
                }
                Ok(())
            }
            Err(binds::BindError::Duplicate(_)) => Err(Status::ALREADY_EXISTS),
            Err(e) => {
                log::error!("{name}: {e}");
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

    /// Move a registered bind entry to `binding` (key grammar, chords separated by commas;
    /// empty or `none` unbinds). Returns `binds.cfg` to write once the lock is released. A
    /// binding equal to the default drops the move, so a changed default still reaches the
    /// user later.
    ///
    /// # Errors
    /// [`Status::NOT_FOUND`] for an unknown entry, [`Status::INVALID_ARGUMENT`] for a
    /// binding that does not parse.
    pub fn rebind_hotkey(&mut self, name: &str, binding: &str) -> Result<Persist, Status> {
        let chords = keys::parse_list(binding).map_err(|_| Status::INVALID_ARGUMENT)?;
        if self.binds.entry(name).is_none() {
            return Err(Status::NOT_FOUND);
        }
        self.binds.rebind(name, chords);
        Ok(self.save_binds())
    }

    /// Note keys the game uses. `label` says what, for example `game: Destination`.
    ///
    /// # Errors
    /// A key list that does not parse.
    pub fn reserve_keys(&mut self, label: &str, chords: &str) -> Result<(), Status> {
        let chords = keys::parse_list(chords).map_err(|_| Status::INVALID_ARGUMENT)?;
        for chord in chords {
            if let Some((fire, _)) = self.binds.lookup(chord) {
                log::warn!("{chord} is {label} and also bound to {fire:?}; both will run");
            }
            self.reserved.push((chord, label.to_owned()));
        }
        Ok(())
    }

    /// What reserved `chord`, if anything.
    #[must_use]
    pub fn reserved_by(&self, chord: keys::Chord) -> Option<&str> {
        self.reserved
            .iter()
            .find(|(c, _)| *c == chord)
            .map(|(_, label)| label.as_str())
    }

    /// `binds.cfg` with the table as it is now, to write once the lock is released.
    #[must_use]
    pub fn save_binds(&mut self) -> Persist {
        self.persist_seq += 1;
        Persist {
            path: self.paths.binds_file(),
            body: Body::Text(self.binds.save()),
            seq: self.persist_seq,
        }
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

    /// Record why the running game does not match a plugin's target filters.
    pub fn set_mismatch(&mut self, plugin: PluginHandle, why: Vec<String>) {
        if let Some(record) = self.plugin_mut(plugin) {
            record.mismatch = why;
        }
    }

    /// Which of the plugin's target filters the running game fails, one line each; empty when
    /// it declared none or all match.
    #[must_use]
    pub fn target_mismatch(&self, info: &Described) -> Vec<String> {
        uniloader_core::target::mismatches(&info.targets, &self.facts)
    }

    /// Mark a plugin started or stopped/faulted.
    pub fn set_enabled(&mut self, plugin: PluginHandle, enabled: bool) {
        if let Some(record) = self.plugin_mut(plugin) {
            record.enabled = enabled;
        }
    }

    /// Mark a plugin ended for the session: stopped or faulted. It also stops being enabled.
    pub fn set_ended(&mut self, plugin: PluginHandle, ended: PluginState) {
        if let Some(record) = self.plugin_mut(plugin) {
            record.enabled = false;
            record.ended = Some(ended);
        }
    }

    /// Whether a plugin ended for the session (stopped or faulted).
    #[must_use]
    pub fn has_ended(&self, plugin: PluginHandle) -> bool {
        self.plugin(plugin).is_some_and(|p| p.ended.is_some())
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
    /// Log records and the echoed commands are left out: this is what a plugin calling
    /// `console_capture` gets back as the answer to its own script. Lines the bounded buffer
    /// has already dropped cannot be returned, so a command that printed more than the whole
    /// history is reported from where the history starts.
    #[must_use]
    pub fn console_since(&self, mark: u64) -> Vec<String> {
        self.console
            .iter()
            .filter(|line| line.seq > mark && line.kind == scrollback::Kind::Output)
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

    /// Claim the keyboard for `plugin`, or give its claim up. Claims stack: the newest holds
    /// the keyboard, and giving one up hands it back to the one before, so two plugins that
    /// each capture for a while cannot take it from each other for good.
    pub fn claim_input(&mut self, plugin: PluginHandle, capture: bool) {
        self.input_claims.retain(|h| *h != plugin);
        if capture {
            self.input_claims.push(plugin);
        }
    }

    /// The key events out of `fired` that may run now: all of them, or only the presses of
    /// the plugin holding the keyboard (its own suggestions) and every release, so nothing
    /// held stays on. A claim by a plugin that is no longer running is void.
    #[must_use]
    pub fn allowed_binds(&self, fired: Vec<binds::Event>) -> Vec<binds::Event> {
        let Some(owner) = self
            .input_claims
            .iter()
            .rev()
            .filter_map(|handle| self.plugin(*handle))
            .find(|p| p.enabled)
        else {
            return fired;
        };
        fired
            .into_iter()
            .filter(|event| {
                event.edge == binds::Edge::Release || event.owner.as_deref() == Some(owner.name())
            })
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
        names.extend(self.binds.entries().map(|entry| entry.name.clone()));
        names.extend(self.binds.aliases().map(|(name, _)| name.to_owned()));
        names.into_iter().collect()
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
pub(crate) mod tests;
