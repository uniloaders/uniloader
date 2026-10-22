//! The bind table: which key chord runs what, merged from three layers.
//!
//! 1. **Suggestions.** Plugins register entries `<plugin>.<name>` with default chords: a
//!    hotkey action (delivered to the plugin's `on_hotkey`) or a script. They come back from
//!    the plugins at every start and are never written to disk.
//! 2. **Moves**, keyed by entry name: the chords the user moved an entry to (`rebind`), or
//!    none. Keyed by the entry rather than the chord, so an entry the user removed stays
//!    removed when its plugin later changes its default.
//! 3. **User binds**: `bind <chord> "<script>"`. A user bind takes its chord; a suggestion on
//!    it is moved off (layer 2).
//!
//! Two suggestions on one chord: the first registered keeps it and the later one is shadowed.
//! Layers 2 and 3 and the aliases are saved as a script (`binds.cfg`); see [`Table::save`].

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::keys::{self, Chord, KeyError, Modifiers};
use crate::script;

/// What an entry runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The owning plugin's hotkey callback, with the entry's unqualified name.
    Hotkey,
    /// A console script.
    Script(String),
}

/// What a key event runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fire {
    /// Call `on_hotkey` (or its release counterpart) for this qualified action.
    Hotkey(String),
    /// Run this script.
    Script(String),
}

/// Whether a key went down or up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Pressed with the chord's exact modifiers.
    Press,
    /// The main key of a chord that fired went up, whatever the modifiers do.
    Release,
}

/// One thing to run because of a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The chord that fired.
    pub chord: Chord,
    /// Press or release.
    pub edge: Edge,
    /// What to run.
    pub fire: Fire,
    /// Plugin that suggested the bind, `None` for a user bind.
    pub owner: Option<String>,
}

/// A suggestion a plugin registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Qualified name, `<plugin>.<name>`.
    pub name: String,
    /// Short label.
    pub title: String,
    /// What it runs.
    pub target: Target,
    defaults: Vec<Chord>,
    chords: Vec<Chord>,
}

impl Entry {
    /// The chords it holds now, the user's move or the defaults.
    pub fn chords(&self) -> impl Iterator<Item = Chord> + '_ {
        self.chords.iter().copied()
    }

    /// The chords it holds now, in config form (`none` when it holds none).
    #[must_use]
    pub fn binding(&self) -> String {
        keys::format_list(&self.chords)
    }

    /// The plugin's suggestion in config form.
    #[must_use]
    pub fn default_binding(&self) -> String {
        keys::format_list(&self.defaults)
    }

    /// The plugin that registered it.
    #[must_use]
    pub fn owner(&self) -> &str {
        self.name.split_once('.').map_or("", |(owner, _)| owner)
    }

    fn fire(&self) -> Fire {
        match &self.target {
            Target::Hotkey => Fire::Hotkey(self.name.clone()),
            Target::Script(script) => Fire::Script(script.clone()),
        }
    }
}

/// Why a table operation was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BindError {
    /// An entry of that name exists already.
    #[error("{0} is already registered")]
    Duplicate(String),
    /// A chord did not parse.
    #[error("{0}")]
    Key(#[from] KeyError),
    /// `none` where one chord is needed.
    #[error("name one key")]
    NoKey,
    /// No entry of that name.
    #[error("no bind entry named {0}")]
    Unknown(String),
    /// An alias name with a dot or another character aliases cannot have.
    #[error("alias names are letters, digits, _ and an optional leading + or -, no dots")]
    AliasName,
}

/// Snapshot of key state the table matches against. The loader fills it from the OS.
pub trait KeyState {
    /// Whether virtual key `vk` is currently held.
    fn is_down(&self, vk: u16) -> bool;

    /// The virtual key the current layout puts at scan code `scancode`, if the platform can
    /// say. Only used for log lines; `sc..` binds fire through [`Table::press`].
    fn vk_for_scancode(&self, _scancode: u16) -> Option<u16> {
        None
    }

    /// Which modifiers are currently held.
    fn modifiers(&self) -> Modifiers;
}

/// Where a row of [`Table::rows`] comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// `bind` by the user.
    User,
    /// A plugin's suggestion, in effect; the entry's name.
    Default(String),
    /// A plugin's suggestion that an earlier one or a user bind holds the chord of.
    Shadowed(String),
    /// A plugin's suggestion the user moved off this chord.
    Removed(String),
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::User => f.write_str("user"),
            Origin::Default(name) => write!(f, "default {name}"),
            Origin::Shadowed(name) => write!(f, "shadowed {name}"),
            Origin::Removed(name) => write!(f, "removed {name}"),
        }
    }
}

/// One line of the `binds` listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The chord.
    pub chord: Chord,
    /// What it runs, as script text.
    pub script: String,
    /// Where it comes from.
    pub origin: Origin,
}

/// Every bind, alias and key state.
#[derive(Debug, Default)]
pub struct Table {
    entries: Vec<Entry>,
    moves: BTreeMap<String, Vec<Chord>>,
    user: BTreeMap<Chord, String>,
    aliases: BTreeMap<String, String>,
    /// Virtual keys down at the last poll.
    down: BTreeSet<u16>,
    /// Chords that fired a press with something to run on release.
    held: Vec<(Chord, Fire, Option<String>)>,
}

/// What a script bind runs on release: `-name` for a script that is a single `+name`.
#[must_use]
pub fn release_of(script: &str) -> Option<String> {
    let commands = script::commands(script).ok()?;
    let [command] = commands.as_slice() else {
        return None;
    };
    let (word, _) = script::split_first(command).ok()??;
    let name = word.strip_prefix('+').filter(|n| !n.is_empty())?;
    Some(format!("-{name}"))
}

/// Whether `name` is a valid alias name (lower case already).
#[must_use]
pub fn alias_name(name: &str) -> bool {
    let body = name.strip_prefix(['+', '-']).unwrap_or(name);
    !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Parse exactly one chord.
///
/// # Errors
/// A chord that does not parse, or `none`.
pub fn chord(text: &str) -> Result<Chord, BindError> {
    keys::parse(text)?.ok_or(BindError::NoKey)
}

impl Table {
    /// Register a plugin's suggestion. Returns warnings to log: chords another entry or a user
    /// bind already holds, so this one is shadowed there.
    ///
    /// # Errors
    /// Duplicate name or an unparsable default.
    pub fn register(
        &mut self,
        name: &str,
        title: &str,
        defaults: &str,
        target: Target,
    ) -> Result<Vec<String>, BindError> {
        if self.entries.iter().any(|e| e.name == name) {
            return Err(BindError::Duplicate(name.to_owned()));
        }
        let defaults = keys::parse_list(defaults)?;
        let chords = self
            .moves
            .get(name)
            .cloned()
            .unwrap_or_else(|| defaults.clone());
        if self.moves.get(name) == Some(&defaults) {
            self.moves.remove(name);
        }
        let mut warnings = Vec::new();
        for chord in &chords {
            if self.user.contains_key(chord) {
                warnings.push(format!(
                    "{name}: {chord} is bound by the user; {name} is shadowed there"
                ));
            } else if let Some(first) = self.entries.iter().find(|e| e.chords.contains(chord)) {
                warnings.push(format!(
                    "{name}: {chord} is already suggested by {}; the first one keeps it",
                    first.name
                ));
            }
        }
        self.entries.push(Entry {
            name: name.to_owned(),
            title: title.to_owned(),
            target,
            defaults,
            chords,
        });
        Ok(warnings)
    }

    /// Registered suggestions, in registration order.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// The entry of that name.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Move an entry to `chords` (empty: off every key). Kept for an entry not registered yet,
    /// so a saved move applies when its plugin starts. Returns whether the entry is known.
    pub fn rebind(&mut self, name: &str, chords: Vec<Chord>) -> bool {
        self.set_move(name, chords)
    }

    fn set_move(&mut self, name: &str, chords: Vec<Chord>) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|e| e.name == name) else {
            self.moves.insert(name.to_owned(), chords);
            return false;
        };
        if chords == entry.defaults {
            self.moves.remove(name);
        } else {
            self.moves.insert(name.to_owned(), chords.clone());
        }
        entry.chords = chords;
        true
    }

    /// Take `chord` away from every entry holding it, recording the move.
    fn move_off(&mut self, chord: Chord) -> bool {
        let holders: Vec<(String, Vec<Chord>)> = self
            .entries
            .iter()
            .filter(|e| e.chords.contains(&chord))
            .map(|e| {
                let rest = e.chords.iter().copied().filter(|c| *c != chord).collect();
                (e.name.clone(), rest)
            })
            .collect();
        let any = !holders.is_empty();
        for (name, rest) in holders {
            self.set_move(&name, rest);
        }
        any
    }

    /// `bind <chord> <script>`: the user's bind takes the chord.
    pub fn bind(&mut self, chord: Chord, script: &str) {
        self.move_off(chord);
        self.user.insert(chord, script.to_owned());
    }

    /// `unbind <chord>`: nothing runs on it any more, suggestions included. Returns whether
    /// anything was bound.
    pub fn unbind(&mut self, chord: Chord) -> bool {
        let user = self.user.remove(&chord).is_some();
        self.move_off(chord) || user
    }

    /// `unbindall`: no user binds, and every suggestion moved off every key.
    pub fn unbind_all(&mut self) {
        self.user.clear();
        let names: Vec<String> = self.entries.iter().map(|e| e.name.clone()).collect();
        for name in names {
            self.set_move(&name, Vec::new());
        }
    }

    /// `binds reset all`: every suggestion back on its defaults. User binds stay, and still
    /// shadow a suggestion on their chord.
    pub fn reset_all(&mut self) {
        self.moves.clear();
        for entry in &mut self.entries {
            entry.chords.clone_from(&entry.defaults);
        }
    }

    /// `binds reset <plugin.name>`: that suggestion back on its defaults.
    ///
    /// # Errors
    /// No entry of that name, and no saved move for it either.
    pub fn reset_entry(&mut self, name: &str) -> Result<(), BindError> {
        let had = self.moves.remove(name).is_some();
        match self.entries.iter_mut().find(|e| e.name == name) {
            Some(entry) => {
                entry.chords.clone_from(&entry.defaults);
                Ok(())
            }
            None if had => Ok(()),
            None => Err(BindError::Unknown(name.to_owned())),
        }
    }

    /// `binds reset <chord>`: drop the user's bind on it and put back every suggestion that
    /// wants it.
    pub fn reset_chord(&mut self, chord: Chord) {
        self.user.remove(&chord);
        let wanting: Vec<(String, Vec<Chord>)> = self
            .entries
            .iter()
            .filter(|e| e.defaults.contains(&chord) && !e.chords.contains(&chord))
            .map(|e| {
                let mut chords = e.chords.clone();
                chords.push(chord);
                (e.name.clone(), chords)
            })
            .collect();
        for (name, chords) in wanting {
            self.set_move(&name, chords);
        }
    }

    /// What `chord` runs and who suggested it, if anything.
    #[must_use]
    pub fn lookup(&self, chord: Chord) -> Option<(Fire, Option<String>)> {
        if let Some(script) = self.user.get(&chord) {
            return Some((Fire::Script(script.clone()), None));
        }
        self.entries
            .iter()
            .find(|e| e.chords.contains(&chord))
            .map(|e| (e.fire(), Some(e.owner().to_owned())))
    }

    /// Define an alias, or delete it with an empty script.
    ///
    /// # Errors
    /// A name aliases cannot have.
    pub fn alias(&mut self, name: &str, script: &str) -> Result<(), BindError> {
        if !alias_name(name) {
            return Err(BindError::AliasName);
        }
        if script.trim().is_empty() {
            self.aliases.remove(name);
        } else {
            self.aliases.insert(name.to_owned(), script.to_owned());
        }
        Ok(())
    }

    /// The script of an alias.
    #[must_use]
    pub fn alias_script(&self, name: &str) -> Option<&str> {
        self.aliases.get(name).map(String::as_str)
    }

    /// Every alias, sorted by name.
    pub fn aliases(&self) -> impl Iterator<Item = (&str, &str)> {
        self.aliases.iter().map(|(n, s)| (n.as_str(), s.as_str()))
    }

    /// Everything bound, for the `binds` listing: user binds, suggestions in effect, shadowed
    /// ones and the ones the user moved off a chord, sorted by chord.
    #[must_use]
    pub fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .user
            .iter()
            .map(|(chord, script)| Row {
                chord: *chord,
                script: script.clone(),
                origin: Origin::User,
            })
            .collect();
        let mut taken: BTreeSet<Chord> = self.user.keys().copied().collect();
        for entry in &self.entries {
            let script = match &entry.target {
                Target::Hotkey => format!("action {}", entry.name),
                Target::Script(script) => script.clone(),
            };
            for chord in &entry.chords {
                let origin = if taken.insert(*chord) {
                    Origin::Default(entry.name.clone())
                } else {
                    Origin::Shadowed(entry.name.clone())
                };
                rows.push(Row {
                    chord: *chord,
                    script: script.clone(),
                    origin,
                });
            }
            for chord in entry.defaults.iter().filter(|c| !entry.chords.contains(c)) {
                rows.push(Row {
                    chord: *chord,
                    script: script.clone(),
                    origin: Origin::Removed(entry.name.clone()),
                });
            }
        }
        rows.sort_by_key(|row| row.chord.to_string());
        rows
    }

    /// Compare against the current key state: presses of chords whose main key went down with
    /// exactly their modifiers, and releases of chords that fired and whose main key went up.
    /// Call once per tick while the game has focus. `sc..` chords come through
    /// [`Table::press`] instead.
    pub fn poll(&mut self, keys: &impl KeyState) -> Vec<Event> {
        let watched: BTreeSet<u16> = self
            .user
            .keys()
            .copied()
            .chain(self.entries.iter().flat_map(|e| e.chords.iter().copied()))
            .chain(self.held.iter().map(|(c, _, _)| *c))
            .filter(|c| c.scancode.is_none())
            .map(|c| c.vk)
            .collect();
        let now: BTreeSet<u16> = watched.into_iter().filter(|vk| keys.is_down(*vk)).collect();
        let mut events =
            self.release_where(|chord| chord.scancode.is_none() && !now.contains(&chord.vk));
        let modifiers = keys.modifiers();
        let pressed: Vec<u16> = now.difference(&self.down).copied().collect();
        for vk in &pressed {
            let chord = Chord {
                vk: *vk,
                scancode: None,
                modifiers,
            };
            events.extend(self.fire_press(chord));
        }
        self.down = now;
        events
    }

    /// A key press the platform observed by scan code (a window message). Fires `sc..`
    /// chords; their release is not reported, so nothing waits for it.
    pub fn press(&mut self, scancode: u16, modifiers: Modifiers) -> Vec<Event> {
        let chord = Chord {
            vk: 0,
            scancode: Some(scancode),
            modifiers,
        };
        let events = self.fire_press(chord);
        self.held.retain(|(c, _, _)| *c != chord);
        events
    }

    fn fire_press(&mut self, chord: Chord) -> Vec<Event> {
        let Some((fire, owner)) = self.lookup(chord) else {
            return Vec::new();
        };
        let release = match &fire {
            Fire::Hotkey(name) => Some(Fire::Hotkey(name.clone())),
            Fire::Script(script) => release_of(script).map(Fire::Script),
        };
        if let Some(release) = release {
            self.held.push((chord, release, owner.clone()));
        }
        vec![Event {
            chord,
            edge: Edge::Press,
            fire,
            owner,
        }]
    }

    fn release_where(&mut self, released: impl Fn(&Chord) -> bool) -> Vec<Event> {
        let (gone, kept) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|(chord, _, _)| released(chord));
        self.held = kept;
        gone.into_iter()
            .map(|(chord, fire, owner)| Event {
                chord,
                edge: Edge::Release,
                fire,
                owner,
            })
            .collect()
    }

    /// Forget the key state, for example when the game loses focus. Returns the releases of
    /// everything still held, so a `+` bind does not stay on.
    pub fn reset(&mut self) -> Vec<Event> {
        self.down.clear();
        self.release_where(|_| true)
    }

    /// The user's layers as a script, for `binds.cfg`: the moves as `rebind` lines, the
    /// aliases, then the user binds, each sorted.
    #[must_use]
    pub fn save(&self) -> String {
        let mut lines = vec![
            "// Written by uniloader after every change. Edit it while the game is closed, or"
                .to_owned(),
            "// use the console: bind, unbind, rebind, alias, binds reset.".to_owned(),
        ];
        lines.extend(self.moves.iter().map(|(name, chords)| {
            format!(
                "rebind {name} {}",
                script::quote(&keys::format_list(chords))
            )
        }));
        lines.extend(
            self.aliases
                .iter()
                .map(|(name, body)| format!("alias {name} {}", script::quote(body))),
        );
        let mut user: Vec<(String, &String)> =
            self.user.iter().map(|(c, s)| (c.to_string(), s)).collect();
        user.sort();
        lines.extend(
            user.into_iter()
                .map(|(chord, body)| format!("bind {chord} {}", script::quote(body))),
        );
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }

    /// Read a saved `binds.cfg` into the user's layers. Only `bind`, `unbind`, `rebind` and
    /// `alias` belong in it; anything else is skipped and reported, with its line, in the
    /// returned warnings (commands to run at start belong in `autoexec.cfg`).
    pub fn load(&mut self, text: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let commands = match script::commands(line) {
                Ok(c) => c,
                Err(e) => {
                    warnings.push(format!("line {}: {e}", number + 1));
                    continue;
                }
            };
            for command in commands {
                if let Err(e) = self.load_command(&command) {
                    warnings.push(format!("line {}: {e}", number + 1));
                }
            }
        }
        warnings
    }

    fn load_command(&mut self, command: &str) -> Result<(), String> {
        let words = script::words(command).map_err(|e| e.to_string())?;
        let word = |i: usize| words.get(i).map(String::as_str);
        match (word(0).map(str::to_ascii_lowercase).as_deref(), words.len()) {
            (Some("bind"), 3) => {
                let chord = chord(word(1).unwrap_or_default()).map_err(|e| e.to_string())?;
                self.bind(chord, word(2).unwrap_or_default());
            }
            (Some("unbind"), 2) => {
                let chord = chord(word(1).unwrap_or_default()).map_err(|e| e.to_string())?;
                self.unbind(chord);
            }
            (Some("rebind"), 3) => {
                let chords =
                    keys::parse_list(word(2).unwrap_or_default()).map_err(|e| e.to_string())?;
                self.rebind(&word(1).unwrap_or_default().to_ascii_lowercase(), chords);
            }
            (Some("alias"), 3) => {
                let name = word(1).unwrap_or_default().to_ascii_lowercase();
                self.alias(&name, word(2).unwrap_or_default())
                    .map_err(|e| e.to_string())?;
            }
            _ => return Err(format!("{command:?} does not belong in binds.cfg")),
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "binds_tests.rs"]
mod tests;
