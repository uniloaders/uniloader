//! Recording a key: the next key or extra mouse button pressed becomes a binding in the
//! loader's key grammar. It behaves like the in-game overlay's recorder: Escape cancels,
//! Delete clears, a key the grammar can name is recorded by name and a punctuation key it
//! cannot (the layout put something else there) by position, `sc<hex>`.

use eframe::egui::{self, Event, Key, PointerButton, RichText, Ui};

/// The hint shown while recording.
pub const HINT: &str = "Press a key or an extra mouse button. Escape cancels, Delete clears.";

/// What a recording produced this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// Still waiting.
    Waiting,
    /// Escape: stop, change nothing.
    Cancelled,
    /// A binding (`none` for Delete).
    Bound(String),
}

/// Look at this frame's input for a recording that is under way.
pub fn poll(ui: &Ui) -> Recorded {
    ui.input(|input| {
        for event in &input.events {
            match event {
                Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if *key == Key::Escape {
                        return Recorded::Cancelled;
                    }
                    if *key == Key::Delete && modifiers.is_none() {
                        return Recorded::Bound("none".to_owned());
                    }
                    if let Some(chord) = chord(*key, *physical_key, *modifiers) {
                        return Recorded::Bound(chord);
                    }
                }
                Event::PointerButton {
                    button,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(name) = mouse_name(*button) {
                        return Recorded::Bound(with_modifiers(name, *modifiers));
                    }
                }
                _ => {}
            }
        }
        Recorded::Waiting
    })
}

/// The binding for a key press, or `None` for a key the grammar has no way to write.
#[must_use]
pub fn chord(key: Key, physical: Option<Key>, modifiers: egui::Modifiers) -> Option<String> {
    let main = key_name(key)
        .map(str::to_owned)
        .or_else(|| physical.and_then(scan_code).map(|sc| format!("sc{sc:x}")))?;
    Some(with_modifiers(&main, modifiers))
}

fn with_modifiers(main: &str, modifiers: egui::Modifiers) -> String {
    let mut text = String::new();
    // `ctrl` is the Control key on every platform, which is what the loader polls; on macOS
    // egui reports Command as `mac_cmd`, which the game never sees.
    if modifiers.ctrl {
        text.push_str("ctrl+");
    }
    if modifiers.alt {
        text.push_str("alt+");
    }
    if modifiers.shift {
        text.push_str("shift+");
    }
    text.push_str(main);
    text
}

fn mouse_name(button: PointerButton) -> Option<&'static str> {
    match button {
        PointerButton::Middle => Some("mouse3"),
        PointerButton::Extra1 => Some("mouse4"),
        PointerButton::Extra2 => Some("mouse5"),
        PointerButton::Primary | PointerButton::Secondary => None,
    }
}

/// The grammar's name for a key as the layout labels it (`uniloader_core::keys`).
#[must_use]
pub fn key_name(key: Key) -> Option<&'static str> {
    use Key as K;
    Some(match key {
        K::ArrowDown => "down",
        K::ArrowLeft => "left",
        K::ArrowRight => "right",
        K::ArrowUp => "up",
        K::Escape => "escape",
        K::Tab => "tab",
        K::Backspace => "backspace",
        K::Enter => "enter",
        K::Space => "space",
        K::Insert => "insert",
        K::Delete => "delete",
        K::Home => "home",
        K::End => "end",
        K::PageUp => "pageup",
        K::PageDown => "pagedown",
        K::Comma => "comma",
        K::Backslash => "backslash",
        K::Slash => "slash",
        K::OpenBracket => "lbracket",
        K::CloseBracket => "rbracket",
        K::Backtick => "grave",
        K::Minus => "minus",
        K::Period => "period",
        // The key right of `0`/`-` on US layouts is VK_OEM_PLUS, whatever it prints.
        K::Plus | K::Equals => "plus",
        K::Semicolon => "semicolon",
        K::Quote => "quote",
        K::IntlBackslash => "oem102",
        _ => return digit_letter_or_function(key),
    })
}

fn digit_letter_or_function(key: Key) -> Option<&'static str> {
    const NAMES: &[&str] = &[
        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "a", "b", "c", "d", "e", "f", "g", "h",
        "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
        "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13", "f14",
        "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24",
    ];
    // egui names these keys "0".."9", "A".."Z" and "F1".."F35"; F25 and up have no virtual
    // key, so they are left out.
    let name = key.name().to_ascii_lowercase();
    NAMES.iter().copied().find(|n| *n == name)
}

/// The set-1 scan code of a punctuation key by position, for layouts whose character there
/// egui has no key for (`^` under Escape on a German layout).
fn scan_code(physical: Key) -> Option<u16> {
    Some(match physical {
        Key::Backtick => 0x29,
        Key::Minus => 0x0C,
        Key::Equals => 0x0D,
        Key::OpenBracket => 0x1A,
        Key::CloseBracket => 0x1B,
        Key::Backslash => 0x2B,
        Key::Semicolon => 0x27,
        Key::Quote => 0x28,
        Key::Comma => 0x33,
        Key::Period => 0x34,
        Key::Slash => 0x35,
        Key::IntlBackslash => 0x56,
        _ => return None,
    })
}

/// A binding as a button that starts recording; `recording` says whether this one is. Returns
/// whether it was clicked.
pub fn button(ui: &mut Ui, shown: &str, recording: bool) -> bool {
    let text = if recording {
        RichText::new("press a key…").strong()
    } else if shown.is_empty() || shown == "none" {
        RichText::new("none").monospace().weak()
    } else {
        RichText::new(shown).monospace()
    };
    let hint = if recording {
        HINT
    } else {
        "Click, then press the key."
    };
    ui.button(text).on_hover_text(hint).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniloader_core::keys;

    #[test]
    fn every_recorded_name_parses_as_the_same_key() {
        for key in Key::ALL {
            let Some(name) = key_name(*key) else {
                continue;
            };
            let parsed = keys::parse(name)
                .unwrap_or_else(|e| panic!("{key:?} -> {name}: {e}"))
                .unwrap_or_else(|| panic!("{key:?} -> {name}: unbound"));
            if !["plus", "escape", "enter", "delete", "insert"].contains(&name) {
                assert_eq!(parsed.to_string(), name, "{key:?}");
            }
        }
        assert_eq!(key_name(Key::F12), Some("f12"));
        assert_eq!(key_name(Key::Num7), Some("7"));
        assert_eq!(key_name(Key::Q), Some("q"));
        assert_eq!(key_name(Key::F30), None);
        assert_eq!(key_name(Key::Copy), None);
    }

    #[test]
    fn modifiers_come_first_in_the_loader_order() {
        let mods = egui::Modifiers {
            alt: true,
            ctrl: true,
            shift: true,
            mac_cmd: false,
            command: true,
        };
        assert_eq!(
            chord(Key::R, None, mods).as_deref(),
            Some("ctrl+alt+shift+r")
        );
        let parsed = keys::parse("ctrl+alt+shift+r").ok().flatten();
        assert_eq!(
            parsed.map(|c| c.to_string()).as_deref(),
            Some("ctrl+alt+shift+r")
        );
    }

    #[test]
    fn a_key_egui_cannot_name_is_recorded_by_position() {
        // `^` under Escape on a German layout: no logical key, physical Backtick.
        assert_eq!(
            chord(Key::Copy, Some(Key::Backtick), egui::Modifiers::NONE).as_deref(),
            Some("sc29")
        );
        assert_eq!(chord(Key::Copy, None, egui::Modifiers::NONE), None);
        assert!(keys::parse("sc29").is_ok());
        assert_eq!(mouse_name(PointerButton::Extra2), Some("mouse5"));
        assert_eq!(mouse_name(PointerButton::Primary), None);
    }
}
