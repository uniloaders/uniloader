//! `binds.cfg`: the user's binds, aliases and moves of plugin suggestions, read and written
//! by the loader's own bind table, with the plugins' suggestions taken from their schemas.

use std::collections::{BTreeMap, BTreeSet};

use uniloader_core::binds::{self, Origin, Table, Target};
use uniloader_core::console::{self, Line};
use uniloader_core::keys::{self, Chord};
use uniloader_core::schema;
use uniloader_core::script;

/// The bind table of an install.
#[derive(Debug)]
pub struct BindTable {
    table: Table,
    /// Lines of `binds.cfg` the loader skips, and suggestions that lost their key to an
    /// earlier one, one sentence each.
    pub warnings: Vec<String>,
}

impl BindTable {
    /// Read `binds.cfg`'s text, then register what the plugins suggest, in the order the
    /// loader does: the file first, so a saved move applies to the suggestion.
    #[must_use]
    pub fn load<'a>(text: &str, plugins: impl IntoIterator<Item = &'a schema::Plugin>) -> Self {
        let mut table = Table::default();
        let mut warnings: Vec<String> = table
            .load(text)
            .into_iter()
            .map(|w| format!("binds.cfg {w}"))
            .collect();
        for bind in plugins.into_iter().flat_map(|p| &p.binds) {
            let target = bind
                .script
                .as_ref()
                .map_or(Target::Hotkey, |s| Target::Script(s.clone()));
            match table.register(&bind.name, &bind.title, &bind.default, target) {
                Ok(shadowed) => warnings.extend(shadowed),
                Err(e) => warnings.push(format!("{}: {e}", bind.name)),
            }
        }
        BindTable { table, warnings }
    }

    /// `binds.cfg` as the loader would write it.
    #[must_use]
    pub fn text(&self) -> String {
        self.table.save()
    }

    /// The plugins' suggestions, in registration order.
    pub fn entries(&self) -> impl Iterator<Item = &binds::Entry> {
        self.table.entries()
    }

    /// The user's binds: key and script, by key.
    #[must_use]
    pub fn user_binds(&self) -> Vec<(Chord, String)> {
        self.table
            .rows()
            .into_iter()
            .filter(|row| row.origin == Origin::User)
            .map(|row| (row.chord, row.script))
            .collect()
    }

    /// Every alias, by name.
    pub fn aliases(&self) -> impl Iterator<Item = (&str, &str)> {
        self.table.aliases()
    }

    /// The keys of suggestion `name` that an earlier suggestion or a user bind holds, so
    /// they do nothing for it.
    #[must_use]
    pub fn shadowed(&self, name: &str) -> Vec<Chord> {
        let origin = Origin::Shadowed(name.to_owned());
        self.table
            .rows()
            .into_iter()
            .filter(|row| row.origin == origin)
            .map(|row| row.chord)
            .collect()
    }

    /// Move a suggestion to `binding` (keys separated by commas; empty or `none` for none).
    ///
    /// # Errors
    /// A key that does not parse.
    pub fn rebind(&mut self, name: &str, binding: &str) -> Result<(), String> {
        let chords = keys::parse_list(binding).map_err(|e| format!("{e}."))?;
        self.table.rebind(name, chords);
        Ok(())
    }

    /// Put a suggestion back on the keys its plugin suggests.
    pub fn reset_entry(&mut self, name: &str) {
        // Only fails for a name neither registered nor moved, which the editor never passes.
        let _ = self.table.reset_entry(name);
    }

    /// Bind `chord` to `script`, replacing the user's bind on `replacing` (the bind being
    /// edited) if given. Returns warnings that do not stop it (unknown commands).
    ///
    /// # Errors
    /// A key that does not parse, or a script the loader would refuse.
    pub fn bind(
        &mut self,
        replacing: Option<Chord>,
        chord: &str,
        script: &str,
        known: &Known,
    ) -> Result<Vec<String>, String> {
        let chord = binds::chord(chord).map_err(|e| format!("{e}."))?;
        let warnings = check_script(script, known, &|name| {
            self.table.alias_script(name).is_some()
        })?;
        if let Some(old) = replacing {
            self.table.reset_chord(old);
        }
        self.table.bind(chord, script.trim());
        Ok(warnings)
    }

    /// Remove the user's bind on `chord`; a suggestion it had pushed off gets the key back.
    pub fn remove_bind(&mut self, chord: Chord) {
        self.table.reset_chord(chord);
    }

    /// Define an alias.
    ///
    /// # Errors
    /// A name aliases cannot have, or a script the loader would refuse.
    pub fn set_alias(
        &mut self,
        name: &str,
        script: &str,
        known: &Known,
    ) -> Result<Vec<String>, String> {
        let name = name.trim().to_ascii_lowercase();
        if script.trim().is_empty() {
            return Err("The alias needs a script.".to_owned());
        }
        let warnings = check_script(script, known, &|n| {
            n == name || self.table.alias_script(n).is_some()
        })?;
        self.table
            .alias(&name, script.trim())
            .map_err(|e| format!("{e}."))?;
        Ok(warnings)
    }

    /// Delete an alias.
    pub fn remove_alias(&mut self, name: &str) {
        let _ = self.table.alias(name, "");
    }
}

/// The names plugins registered, for checking and completing scripts.
#[derive(Debug, Default, Clone)]
pub struct Known {
    /// Qualified setting names, and whether each is a secret.
    settings: BTreeMap<String, bool>,
    /// Qualified command names.
    commands: BTreeSet<String>,
    /// Qualified bind entry names (`action <name>`).
    actions: BTreeSet<String>,
}

impl Known {
    /// Everything `plugins` registered.
    #[must_use]
    pub fn from_schemas<'a>(plugins: impl IntoIterator<Item = &'a schema::Plugin>) -> Self {
        let mut known = Known::default();
        for plugin in plugins {
            for setting in &plugin.settings {
                known.settings.insert(
                    format!("{}.{}", plugin.name, setting.desc.key),
                    setting.desc.secret,
                );
            }
            known.commands.extend(
                plugin
                    .commands
                    .iter()
                    .map(|c| format!("{}.{}", plugin.name, c.name)),
            );
            known
                .actions
                .extend(plugin.binds.iter().map(|b| b.name.clone()));
        }
        known
    }

    /// Every name a script can start with, for a picker: built-ins, plugin commands,
    /// settings that are not secret, and `action <entry>`.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = console::BUILTIN_NAMES
            .iter()
            .map(|n| (*n).to_owned())
            .collect();
        names.extend(self.commands.iter().cloned());
        names.extend(
            self.settings
                .iter()
                .filter(|(_, secret)| !**secret)
                .map(|(name, _)| name.clone()),
        );
        names.extend(self.actions.iter().map(|a| format!("action {a}")));
        names
    }

    fn secret(&self, name: &str) -> bool {
        self.settings.get(name).copied().unwrap_or(false)
    }
}

/// Check a bind or alias script the way the loader treats it: every command must parse, and
/// none may set a secret (the loader refuses that, since `binds.cfg` is plain text). Names no
/// plugin registered are warnings, because the plugin may simply not have run yet.
///
/// # Errors
/// The first command the loader would refuse.
pub fn check_script(
    text: &str,
    known: &Known,
    is_alias: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, String> {
    let commands = script::commands(text).map_err(|e| format!("{e}."))?;
    if commands.is_empty() {
        return Err("The script is empty.".to_owned());
    }
    let mut warnings = Vec::new();
    for command in &commands {
        let line = console::parse(command).map_err(|e| format!("{command}: {e}."))?;
        match line {
            Line::Set(name, _) if known.secret(&name) => {
                return Err(format!(
                    "{name} is a secret; a bind or alias may not set it."
                ));
            }
            Line::Command(name, args) => {
                if known.secret(&name) && !args.trim().is_empty() {
                    return Err(format!(
                        "{name} is a secret; a bind or alias may not set it."
                    ));
                }
                if !known.settings.contains_key(&name) && !known.commands.contains(&name) {
                    warnings.push(format!(
                        "{name} is not a command or setting any plugin registered."
                    ));
                }
            }
            Line::Variable(name, _) if !is_alias(&name) => {
                warnings.push(format!("{name} is not a command or an alias."));
            }
            Line::Action(name) if !known.actions.contains(&name) => {
                warnings.push(format!("No plugin registered the action {name}."));
            }
            Line::Bind(_, Some(body)) | Line::Alias(_, Some(body)) => {
                warnings.extend(check_script(&body, known, is_alias)?);
            }
            _ => {}
        }
    }
    Ok(warnings)
}

#[cfg(test)]
#[path = "bind_table_tests.rs"]
mod tests;
