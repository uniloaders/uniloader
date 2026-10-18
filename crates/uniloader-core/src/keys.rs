//! Key binding grammar: `f12`, `ctrl+shift+r`, `numpad5`, `caret`, `0x7b`, `sc29`, `none`.
//!
//! Virtual key codes follow the Win32 `VK_*` numbering so the loader can use them directly;
//! the table itself has no Windows dependency.
//!
//! A name or a `0x..` code names a key *as the layout labels it*, which is what a user means
//! by "bind it to Q". `sc<hex>` names a key by **where it is**: the scan code the keyboard
//! sends, which is the same on every layout. `sc29` is the key under Escape — grave on a US
//! layout, `^` on a German one, `²` on a French one — and is the only sane way to bind that
//! key, because its virtual key code is a different one in each of those layouts.

use std::fmt;

use thiserror::Error;

/// Modifier keys that must be held together with the main key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Either Control key.
    pub ctrl: bool,
    /// Either Alt key.
    pub alt: bool,
    /// Either Shift key.
    pub shift: bool,
}

/// A parsed binding: one virtual key plus modifiers. `None` means unbound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    /// Win32 virtual key code of the main key. Ignored while [`Chord::scancode`] is set,
    /// because the scan code only becomes a virtual key under a particular layout.
    pub vk: u16,
    /// Physical key position, when the binding was written as `sc<hex>`.
    ///
    /// Resolved to a virtual key by the platform at poll time, so the same binding follows
    /// the key's position rather than the character printed on it.
    pub scancode: Option<u16>,
    /// Modifiers that must be held.
    pub modifiers: Modifiers,
}

/// Why a binding string was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeyError {
    /// The main key name is not in the table.
    #[error("unknown key name {0:?}")]
    UnknownKey(String),
    /// A modifier was given without a main key.
    #[error("binding {0:?} has modifiers but no key")]
    MissingKey(String),
    /// The `0x..` form did not parse or exceeds the virtual key range.
    #[error("bad virtual key code {0:?}")]
    BadCode(String),
    /// The `sc..` form did not parse as a scan code.
    #[error("bad scan code {0:?}")]
    BadScanCode(String),
}

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_PAUSE: u16 = 0x13;
const VK_CAPITAL: u16 = 0x14;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_SNAPSHOT: u16 = 0x2C;
const VK_INSERT: u16 = 0x2D;
const VK_DELETE: u16 = 0x2E;
const VK_NUMPAD0: u16 = 0x60;
const VK_MULTIPLY: u16 = 0x6A;
const VK_ADD: u16 = 0x6B;
const VK_SUBTRACT: u16 = 0x6D;
const VK_DECIMAL: u16 = 0x6E;
const VK_DIVIDE: u16 = 0x6F;
const VK_F1: u16 = 0x70;
const VK_NUMLOCK: u16 = 0x90;
const VK_SCROLL: u16 = 0x91;
const VK_OEM_1: u16 = 0xBA;
const VK_OEM_PLUS: u16 = 0xBB;
const VK_OEM_COMMA: u16 = 0xBC;
const VK_OEM_MINUS: u16 = 0xBD;
const VK_OEM_PERIOD: u16 = 0xBE;
const VK_OEM_2: u16 = 0xBF;
const VK_OEM_3: u16 = 0xC0;
const VK_OEM_4: u16 = 0xDB;
const VK_OEM_5: u16 = 0xDC;
const VK_OEM_6: u16 = 0xDD;
const VK_OEM_7: u16 = 0xDE;
const VK_OEM_102: u16 = 0xE2;

/// Named keys. `caret` is the key left of `1` on a German QWERTZ layout (`VK_OEM_5`), the
/// conventional console key there; `grave` is the same physical key on US layouts.
const NAMED: &[(&str, u16)] = &[
    ("backspace", VK_BACK),
    ("tab", VK_TAB),
    ("enter", VK_RETURN),
    ("return", VK_RETURN),
    ("pause", VK_PAUSE),
    ("capslock", VK_CAPITAL),
    ("escape", VK_ESCAPE),
    ("esc", VK_ESCAPE),
    ("space", VK_SPACE),
    ("pageup", VK_PRIOR),
    ("pagedown", VK_NEXT),
    ("end", VK_END),
    ("home", VK_HOME),
    ("left", VK_LEFT),
    ("up", VK_UP),
    ("right", VK_RIGHT),
    ("down", VK_DOWN),
    ("printscreen", VK_SNAPSHOT),
    ("insert", VK_INSERT),
    ("delete", VK_DELETE),
    ("numlock", VK_NUMLOCK),
    ("scrolllock", VK_SCROLL),
    ("multiply", VK_MULTIPLY),
    ("add", VK_ADD),
    ("subtract", VK_SUBTRACT),
    ("decimal", VK_DECIMAL),
    ("divide", VK_DIVIDE),
    ("semicolon", VK_OEM_1),
    ("plus", VK_OEM_PLUS),
    ("comma", VK_OEM_COMMA),
    ("minus", VK_OEM_MINUS),
    ("period", VK_OEM_PERIOD),
    ("slash", VK_OEM_2),
    ("grave", VK_OEM_3),
    ("tilde", VK_OEM_3),
    ("lbracket", VK_OEM_4),
    ("backslash", VK_OEM_5),
    ("caret", VK_OEM_5),
    ("rbracket", VK_OEM_6),
    ("quote", VK_OEM_7),
    ("oem102", VK_OEM_102),
];

fn lookup(name: &str) -> Option<u16> {
    if let Some(digits) = name.strip_prefix('f') {
        if let Ok(n) = digits.parse::<u16>() {
            if (1..=24).contains(&n) {
                return Some(VK_F1 + n - 1);
            }
        }
    }
    if let Some(digit) = name.strip_prefix("numpad") {
        if let Ok(n) = digit.parse::<u16>() {
            if n <= 9 {
                return Some(VK_NUMPAD0 + n);
            }
        }
    }
    let bytes = name.as_bytes();
    if bytes.len() == 1 && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return Some(u16::from(bytes[0].to_ascii_uppercase()));
    }
    NAMED.iter().find(|(n, _)| *n == name).map(|(_, vk)| *vk)
}

/// Parse a binding. Case insensitive; `none` or an empty string means unbound.
///
/// # Errors
/// See [`KeyError`].
pub fn parse(binding: &str) -> Result<Option<Chord>, KeyError> {
    let text = binding.trim().to_ascii_lowercase();
    if text.is_empty() || text == "none" {
        return Ok(None);
    }
    let mut modifiers = Modifiers::default();
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        match part {
            "ctrl" | "control" => modifiers.ctrl = true,
            "alt" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            // An empty part comes from a trailing or doubled "+"; the key is still missing.
            "" => {}
            other => key = Some(other),
        }
    }
    let Some(name) = key else {
        return Err(KeyError::MissingKey(binding.to_owned()));
    };
    if let Some(code) = name.strip_prefix("sc") {
        let scancode = u16::from_str_radix(code.strip_prefix("0x").unwrap_or(code), 16)
            .ok()
            .filter(|sc| (1..=0x1FF).contains(sc))
            .ok_or_else(|| KeyError::BadScanCode(name.to_owned()))?;
        return Ok(Some(Chord {
            vk: 0,
            scancode: Some(scancode),
            modifiers,
        }));
    }
    let vk = if let Some(hex) = name.strip_prefix("0x") {
        u16::from_str_radix(hex, 16)
            .ok()
            .filter(|vk| (1..=0xFE).contains(vk))
            .ok_or_else(|| KeyError::BadCode(name.to_owned()))?
    } else {
        lookup(name).ok_or_else(|| KeyError::UnknownKey(name.to_owned()))?
    };
    Ok(Some(Chord {
        vk,
        scancode: None,
        modifiers,
    }))
}

/// Whether [`Chord`]'s own grammar has a name for this virtual key.
///
/// The hotkey recorder asks before it writes a binding: a key the grammar can name is worth
/// recording by name, and anything else is better recorded by position, because a code the
/// grammar cannot name is one no layout agrees on either.
#[must_use]
pub fn has_name(vk: u16) -> bool {
    (VK_F1..VK_F1 + 24).contains(&vk)
        || (VK_NUMPAD0..=VK_NUMPAD0 + 9).contains(&vk)
        || (0x30..=0x39).contains(&vk)
        || (0x41..=0x5A).contains(&vk)
        || NAMED.iter().any(|(_, code)| *code == vk)
}

/// Parse a binding list: chords separated by commas, any one of which fires the action.
///
/// Two bindings for one action is the normal case rather than a luxury: the key under Escape
/// is the console key people reach for, and it is also the key some layouts put a dead
/// diacritic on, so an action that only has that one binding is unreachable for them.
///
/// `none`, an empty string and a list of nothing but those all mean unbound.
///
/// # Errors
/// The first chord that does not parse, as [`parse`] would report it.
pub fn parse_list(binding: &str) -> Result<Vec<Chord>, KeyError> {
    let mut chords = Vec::new();
    for part in binding.split(',') {
        if let Some(chord) = parse(part)? {
            if !chords.contains(&chord) {
                chords.push(chord);
            }
        }
    }
    Ok(chords)
}

/// A binding list in config form, `none` when there is nothing bound.
#[must_use]
pub fn format_list(chords: &[Chord]) -> String {
    if chords.is_empty() {
        return "none".to_owned();
    }
    chords
        .iter()
        .map(Chord::to_string)
        .collect::<Vec<String>>()
        .join(", ")
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers.ctrl {
            f.write_str("ctrl+")?;
        }
        if self.modifiers.alt {
            f.write_str("alt+")?;
        }
        if self.modifiers.shift {
            f.write_str("shift+")?;
        }
        if let Some(scancode) = self.scancode {
            return write!(f, "sc{scancode:x}");
        }
        match self.vk {
            vk if (VK_F1..VK_F1 + 24).contains(&vk) => write!(f, "f{}", vk - VK_F1 + 1),
            vk if (VK_NUMPAD0..=VK_NUMPAD0 + 9).contains(&vk) => {
                write!(f, "numpad{}", vk - VK_NUMPAD0)
            }
            vk if (0x30..=0x39).contains(&vk) || (0x41..=0x5A).contains(&vk) => {
                write!(
                    f,
                    "{}",
                    char::from(u8::try_from(vk).unwrap_or(b'?')).to_ascii_lowercase()
                )
            }
            vk => match NAMED.iter().find(|(_, code)| *code == vk) {
                Some((name, _)) => f.write_str(name),
                None => write!(f, "0x{vk:02x}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_function_and_letter_keys() {
        assert_eq!(
            parse("F12"),
            Ok(Some(Chord {
                scancode: None,
                vk: 0x7B,
                modifiers: Modifiers::default()
            }))
        );
        assert_eq!(
            parse("a"),
            Ok(Some(Chord {
                scancode: None,
                vk: 0x41,
                modifiers: Modifiers::default()
            }))
        );
        assert_eq!(
            parse("7"),
            Ok(Some(Chord {
                scancode: None,
                vk: 0x37,
                modifiers: Modifiers::default()
            }))
        );
        assert_eq!(
            parse("numpad5"),
            Ok(Some(Chord {
                scancode: None,
                vk: 0x65,
                modifiers: Modifiers::default()
            }))
        );
    }

    #[test]
    fn parses_modifiers_in_any_order() {
        let chord = Chord {
            scancode: None,
            vk: 0x52,
            modifiers: Modifiers {
                ctrl: true,
                alt: false,
                shift: true,
            },
        };
        assert_eq!(parse("ctrl+shift+r"), Ok(Some(chord)));
        assert_eq!(parse("Shift + Control + R"), Ok(Some(chord)));
    }

    #[test]
    fn only_keys_the_grammar_can_spell_have_a_name() {
        assert!(has_name(0x7B), "f12");
        assert!(has_name(0x41), "a");
        assert!(has_name(VK_ESCAPE), "escape");
        // What Wine reports for the key under Escape on a German layout. Nothing in the
        // table claims it, so a recorder must fall back to the key's position.
        assert!(!has_name(0xFC));
    }

    #[test]
    fn a_scan_code_binding_names_a_position_not_a_character() {
        let chord = parse("sc29").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            chord,
            Some(Chord {
                vk: 0,
                scancode: Some(0x29),
                modifiers: Modifiers::default(),
            })
        );
        // It round-trips through the config file.
        assert_eq!(chord.map(|c| c.to_string()), Some("sc29".to_owned()));
        // With modifiers, and with an explicit 0x.
        let chord = parse("ctrl+sc0x3b").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(chord.and_then(|c| c.scancode), Some(0x3b));
        assert!(chord.is_some_and(|c| c.modifiers.ctrl));
        assert!(matches!(parse("sczz"), Err(KeyError::BadScanCode(_))));
    }

    #[test]
    fn console_key_names_map_to_oem_codes() {
        assert_eq!(parse("caret").map(|c| c.map(|c| c.vk)), Ok(Some(VK_OEM_5)));
        assert_eq!(parse("grave").map(|c| c.map(|c| c.vk)), Ok(Some(VK_OEM_3)));
    }

    #[test]
    fn unbound_and_hex_forms() {
        assert_eq!(parse("none"), Ok(None));
        assert_eq!(parse("  "), Ok(None));
        assert_eq!(parse("0x7b").map(|c| c.map(|c| c.vk)), Ok(Some(0x7B)));
        assert_eq!(parse("0xzz"), Err(KeyError::BadCode("0xzz".into())));
        assert_eq!(parse("ctrl+"), Err(KeyError::MissingKey("ctrl+".into())));
        assert_eq!(parse("bogus"), Err(KeyError::UnknownKey("bogus".into())));
    }

    #[test]
    fn display_round_trips() {
        // "caret" shares its code with "backslash", which Display prefers; the parse test above
        // covers the alias.
        for text in [
            "f12",
            "ctrl+alt+shift+f1",
            "numpad9",
            "backslash",
            "x",
            "0xfe",
        ] {
            let chord = parse(text)
                .ok()
                .flatten()
                .unwrap_or_else(|| panic!("{text} parses"));
            assert_eq!(chord.to_string(), text);
        }
    }

    #[test]
    fn a_list_round_trips_and_drops_the_unbound_parts() {
        let chords = parse_list("sc29, ctrl+shift+c").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(chords.len(), 2);
        assert_eq!(format_list(&chords), "sc29, ctrl+shift+c");
        assert_eq!(parse_list("none"), Ok(Vec::new()));
        assert_eq!(parse_list(""), Ok(Vec::new()));
        assert_eq!(format_list(&[]), "none");
        // A duplicate would fire the action twice on one press.
        assert_eq!(parse_list("f12, f12").map(|c| c.len()), Ok(1));
        assert!(parse_list("f12, nokey").is_err());
    }
}
