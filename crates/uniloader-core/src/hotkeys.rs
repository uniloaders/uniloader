//! Hotkey registry: actions, their effective chords and edge-triggered matching.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::keys::{self, Chord, KeyError, Modifiers};

/// Stable identifier of a registered action: `<plugin>.<action>`.
pub type ActionName = String;

/// One effective binding of an action, with the state edge detection needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bound {
    chord: Chord,
    /// Whether the main key was down at the last poll.
    down: bool,
}

/// One registered action.
///
/// An action can carry several bindings and fires on whichever is pressed. The list is
/// private because each binding owns its own edge state: handing out a `&mut Vec<Chord>`
/// would let a caller change a binding and leave the "was down" flag of the old one behind,
/// which is an action that fires once on the release of a key it is no longer bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Qualified name.
    pub name: ActionName,
    /// Short label.
    pub title: String,
    /// Bindings from the descriptor.
    defaults: Vec<Chord>,
    /// Effective bindings (user override or defaults).
    chords: Vec<Bound>,
}

impl Entry {
    /// The effective bindings, in the order they were written.
    pub fn chords(&self) -> impl Iterator<Item = Chord> + '_ {
        self.chords.iter().map(|bound| bound.chord)
    }

    /// The effective bindings in config form, for example `sc29, ctrl+shift+c`.
    #[must_use]
    pub fn binding(&self) -> String {
        keys::format_list(&self.chords.iter().map(|b| b.chord).collect::<Vec<Chord>>())
    }

    /// What the bindings would be without a user override.
    #[must_use]
    pub fn default_binding(&self) -> String {
        keys::format_list(&self.defaults)
    }
}

/// Why registration failed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HotkeyError {
    /// Action already registered.
    #[error("hotkey {0:?} is already registered")]
    Duplicate(String),
    /// The default binding did not parse.
    #[error("hotkey {0:?} default binding: {1}")]
    BadDefault(String, KeyError),
}

/// All actions of all plugins plus user overrides.
#[derive(Debug, Default)]
pub struct Registry {
    entries: Vec<Entry>,
}

/// Snapshot of key state the registry matches against. The loader fills it from the OS.
pub trait KeyState {
    /// Whether virtual key `vk` is currently held.
    fn is_down(&self, vk: u16) -> bool;

    /// The virtual key the current layout puts at scan code `scancode`, if the platform can
    /// say. The default answers "cannot", which makes a `sc..` binding simply never fire on
    /// a platform that does not implement it, rather than firing on the wrong key.
    fn vk_for_scancode(&self, _scancode: u16) -> Option<u16> {
        None
    }
    /// Which modifiers are currently held.
    fn modifiers(&self) -> Modifiers;
}

impl Registry {
    /// Register an action. `overrides` holds user bindings by qualified name; an override
    /// that fails to parse is reported back so the loader can log it, and the default applies.
    ///
    /// # Errors
    /// Duplicate name or an unparsable default.
    pub fn register(
        &mut self,
        name: &str,
        title: &str,
        default_binding: &str,
        overrides: &BTreeMap<String, String>,
    ) -> Result<Option<KeyError>, HotkeyError> {
        if self.entries.iter().any(|e| e.name == name) {
            return Err(HotkeyError::Duplicate(name.to_owned()));
        }
        let defaults = keys::parse_list(default_binding)
            .map_err(|e| HotkeyError::BadDefault(name.to_owned(), e))?;
        let (chords, override_error) = match overrides.get(name).map(|b| keys::parse_list(b)) {
            Some(Ok(chords)) => (chords, None),
            Some(Err(e)) => (defaults.clone(), Some(e)),
            None => (defaults.clone(), None),
        };
        self.entries.push(Entry {
            name: name.to_owned(),
            title: title.to_owned(),
            defaults,
            chords: bind(chords),
        });
        Ok(override_error)
    }

    /// Change the effective binding of an action at runtime, to one chord or to nothing.
    #[must_use]
    pub fn rebind(&mut self, name: &str, chord: Option<Chord>) -> bool {
        self.rebind_all(name, chord.into_iter().collect())
    }

    /// Replace every binding of an action at runtime.
    #[must_use]
    pub fn rebind_all(&mut self, name: &str, chords: Vec<Chord>) -> bool {
        match self.entries.iter_mut().find(|e| e.name == name) {
            Some(e) => {
                e.chords = bind(chords);
                true
            }
            None => false,
        }
    }

    /// Registered actions in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// Actions bound to this physical key, for a key press the platform observed.
    ///
    /// Scan code bindings are delivered this way rather than polled. Polling needs a virtual
    /// key, and asking the platform which virtual key sits at a scan code is not reliable:
    /// under Wine, `MapVirtualKeyW` answers one code for the key under Escape while the key
    /// itself arrives carrying another. A press carries both, so there is nothing to guess.
    pub fn press(&mut self, scancode: u16, modifiers: Modifiers) -> Vec<ActionName> {
        self.entries
            .iter()
            .filter(|entry| {
                entry
                    .chords()
                    .any(|chord| chord.scancode == Some(scancode) && chord.modifiers == modifiers)
            })
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// Compare against the current key state and return the actions whose key went from
    /// up to down with matching modifiers. Call once per frame while the game has focus.
    ///
    /// Only the bindings that name a key; the ones that name a position come through
    /// [`Registry::press`].
    pub fn poll(&mut self, state: &impl KeyState) -> Vec<ActionName> {
        let held = state.modifiers();
        let mut fired = Vec::new();
        for entry in &mut self.entries {
            // One action fires at most once per poll even when two of its bindings go down
            // together, which is what Ctrl held over a binding that does not want it does.
            let mut already = false;
            for bound in &mut entry.chords {
                if bound.chord.scancode.is_some() {
                    continue;
                }
                let now = state.is_down(bound.chord.vk);
                if now && !bound.down && bound.chord.modifiers == held && !already {
                    fired.push(entry.name.clone());
                    already = true;
                }
                bound.down = now;
            }
        }
        fired
    }

    /// Forget all key-down state, for example when the window loses focus.
    pub fn reset(&mut self) {
        for entry in &mut self.entries {
            for bound in &mut entry.chords {
                bound.down = false;
            }
        }
    }

    /// Bindings in config file form, for writing back after an in-game rebind.
    #[must_use]
    pub fn bindings(&self) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .map(|e| (e.name.clone(), e.binding()))
            .collect()
    }
}

/// Wrap chords as bindings that have not been seen down yet.
fn bind(chords: Vec<Chord>) -> Vec<Bound> {
    chords
        .into_iter()
        .map(|chord| Bound { chord, down: false })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Keys {
        down: Vec<u16>,
        mods: Modifiers,
    }

    impl KeyState for Keys {
        fn is_down(&self, vk: u16) -> bool {
            self.down.contains(&vk)
        }
        fn modifiers(&self) -> Modifiers {
            self.mods
        }
    }

    fn registry() -> Registry {
        let mut overrides = BTreeMap::new();
        overrides.insert("vr.recenter".to_owned(), "ctrl+f11".to_owned());
        overrides.insert("vr.broken".to_owned(), "nokey".to_owned());
        let mut r = Registry::default();
        assert_eq!(
            r.register("vr.toggle", "Toggle", "f12, f9", &overrides),
            Ok(None)
        );
        assert_eq!(
            r.register("vr.recenter", "Recenter", "f11", &overrides),
            Ok(None)
        );
        assert!(matches!(
            r.register("vr.broken", "Broken", "f10", &overrides),
            Ok(Some(KeyError::UnknownKey(_)))
        ));
        r
    }

    #[test]
    fn overrides_apply_and_bad_ones_fall_back() {
        let r = registry();
        let b = r.bindings();
        assert_eq!(b["vr.toggle"], "f12, f9");
        assert_eq!(b["vr.recenter"], "ctrl+f11");
        assert_eq!(b["vr.broken"], "f10");
    }

    #[test]
    fn either_binding_fires_the_action_and_both_together_fire_it_once() {
        let mut r = registry();
        let none = Modifiers::default();
        let press = |r: &mut Registry, keys: Vec<u16>| {
            r.poll(&Keys {
                down: keys,
                mods: none,
            })
        };
        // 0x78 is f9, the action's second binding.
        assert_eq!(press(&mut r, vec![0x78]), vec!["vr.toggle"]);
        assert_eq!(press(&mut r, vec![]), Vec::<String>::new());
        assert_eq!(press(&mut r, vec![0x7B]), vec!["vr.toggle"], "f12 too");
        assert_eq!(press(&mut r, vec![]), Vec::<String>::new());
        assert_eq!(
            press(&mut r, vec![0x7B, 0x78]),
            vec!["vr.toggle"],
            "one action, one firing"
        );
    }

    #[test]
    fn a_rebind_replaces_every_binding() {
        let mut r = registry();
        let chord = keys::parse("f5").unwrap_or(None);
        assert!(r.rebind("vr.toggle", chord));
        assert_eq!(r.bindings()["vr.toggle"], "f5");
        let none = Modifiers::default();
        assert!(
            r.poll(&Keys {
                down: vec![0x7B],
                mods: none
            })
            .is_empty(),
            "the old binding is gone"
        );
    }

    #[test]
    fn fires_on_edge_with_exact_modifiers() {
        let mut r = registry();
        let none = Modifiers::default();
        assert_eq!(
            r.poll(&Keys {
                down: vec![0x7B],
                mods: none
            }),
            vec!["vr.toggle"]
        );
        assert!(
            r.poll(&Keys {
                down: vec![0x7B],
                mods: none
            })
            .is_empty(),
            "held key fires once"
        );
        assert_eq!(
            r.poll(&Keys {
                down: vec![],
                mods: none
            }),
            Vec::<String>::new()
        );
        assert!(
            r.poll(&Keys {
                down: vec![0x7A],
                mods: none
            })
            .is_empty(),
            "f11 needs ctrl"
        );
        r.reset();
        let ctrl = Modifiers { ctrl: true, ..none };
        assert_eq!(
            r.poll(&Keys {
                down: vec![0x7A],
                mods: ctrl
            }),
            vec!["vr.recenter"]
        );
    }

    #[test]
    fn duplicate_and_rebind() {
        let mut r = registry();
        assert_eq!(
            r.register("vr.toggle", "x", "f1", &BTreeMap::new()),
            Err(HotkeyError::Duplicate("vr.toggle".into()))
        );
        assert!(r.rebind("vr.toggle", None));
        assert!(!r.rebind("missing", None));
        assert_eq!(r.bindings()["vr.toggle"], "none");
    }
}
