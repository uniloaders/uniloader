//! The key recorder behind every "click to change" binding: the editor's hotkeys and the
//! key settings, wherever they are drawn. One binding records at a time; the window
//! procedure hands over key presses while one does (`Gui::recording`).
//!
//! While recording, Escape cancels, Delete clears the binding and a modifier alone waits for
//! the key it modifies. A chord with Delete (`ctrl+delete`) still records as itself.

use std::sync::{Mutex, MutexGuard, PoisonError};

use uniloader_core::keys::{self, Chord, Modifiers};

/// What a recorded key goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A hotkey action, `<plugin>.<action>`: written with `rebind`.
    Hotkey(String),
    /// A key setting, `<plugin>.<key>`: written with `set`.
    Setting(String),
}

/// What one key press did to the recording.
#[derive(Debug, PartialEq, Eq)]
enum Recorded {
    /// A modifier alone: still waiting.
    Waiting,
    /// Escape: nothing changes.
    Cancelled,
    /// Delete: unbound.
    Cleared,
    /// A chord in key grammar.
    Bound(String),
}

static RECORDING: Mutex<Option<Target>> = Mutex::new(None);

fn recording() -> MutexGuard<'static, Option<Target>> {
    RECORDING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether a binding is recording.
pub(crate) fn is_recording() -> bool {
    recording().is_some()
}

/// Whether `target` is the one recording.
pub(crate) fn is(target: &Target) -> bool {
    recording().as_ref() == Some(target)
}

/// Starts recording `target`, or stops when it already was (a second click).
pub(crate) fn toggle(target: Target) {
    let mut guard = recording();
    *guard = if guard.as_ref() == Some(&target) {
        None
    } else {
        Some(target)
    };
}

/// Stops recording whatever was.
pub(crate) fn stop() {
    *recording() = None;
}

/// Feeds the key the window procedure saw; the target and its new binding (`none` when
/// cleared) once the recording is over with a change.
pub(crate) fn press(vk: u16, scancode: u16, modifiers: Modifiers) -> Option<(Target, String)> {
    let mut guard = recording();
    let target = guard.clone()?;
    let binding = match recorded(vk, scancode, modifiers) {
        Recorded::Waiting => return None,
        Recorded::Cancelled => {
            *guard = None;
            return None;
        }
        Recorded::Cleared => "none".to_owned(),
        Recorded::Bound(chord) => chord,
    };
    *guard = None;
    Some((target, binding))
}

/// The hint under a recording binding.
pub(crate) const HINT: &str = "Press a key: Escape cancels, Delete clears.";

/// What a key press stands for.
///
/// A key the grammar can name is recorded by name, which reads well in `binds.cfg`;
/// anything else — the key under Escape reports `0xFC` under Wine — by position, so the
/// binding stays on that key whatever the layout calls it.
fn recorded(vk: u16, scancode: u16, modifiers: Modifiers) -> Recorded {
    const VK_ESCAPE: u16 = 0x1B;
    const VK_DELETE: u16 = 0x2E;
    const MODIFIER_KEYS: [u16; 9] = [0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5];
    if MODIFIER_KEYS.contains(&vk) {
        return Recorded::Waiting;
    }
    if vk == VK_ESCAPE {
        return Recorded::Cancelled;
    }
    if vk == VK_DELETE && modifiers == Modifiers::default() {
        return Recorded::Cleared;
    }
    let chord = Chord {
        vk,
        scancode: (!keys::has_name(vk)).then_some(scancode),
        modifiers,
    };
    Recorded::Bound(chord.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_waits_for_a_real_key_escape_cancels_delete_clears() {
        let none = Modifiers::default();
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(recorded(0x11, 0x1D, ctrl), Recorded::Waiting);
        assert_eq!(recorded(0x1B, 0x01, none), Recorded::Cancelled);
        assert_eq!(recorded(0x2E, 0x53, none), Recorded::Cleared);
        assert_eq!(
            recorded(0x2E, 0x53, ctrl),
            Recorded::Bound("ctrl+delete".to_owned())
        );
        assert_eq!(recorded(0x77, 0x42, none), Recorded::Bound("f8".to_owned()));
        let Recorded::Bound(by_position) = recorded(0xFC, 0x29, none) else {
            panic!("the key under Escape was not recorded");
        };
        assert!(by_position.starts_with("sc"), "{by_position}");
    }

    #[test]
    fn one_target_records_at_a_time() {
        let a = Target::Hotkey("p.a".to_owned());
        let b = Target::Setting("p.b".to_owned());
        toggle(a.clone());
        assert!(is(&a));
        toggle(b.clone());
        assert!(is(&b) && !is(&a));
        assert_eq!(
            press(0x77, 0x42, Modifiers::default()),
            Some((b, "f8".to_owned()))
        );
        assert!(!is_recording());
        toggle(a.clone());
        toggle(a);
        assert!(!is_recording());
    }
}
