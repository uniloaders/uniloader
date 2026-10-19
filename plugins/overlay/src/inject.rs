//! `overlay.input`: typed text, key presses and clicks for the overlay, from code.
//!
//! A test, or someone at a console without the game focused, drives the overlay's windows
//! with it. The input is posted to the game window as ordinary window messages, so it goes
//! through the same window procedure, translation and keyboard capture as real input, and
//! no other window loses the focus to it.

/// One `overlay.input` request.
#[derive(Debug, PartialEq)]
pub(crate) enum Input {
    /// Characters, as `WM_CHAR`.
    Type(String),
    /// A press and release of a virtual key.
    Key(u16),
    /// A left click at a point in the backbuffer.
    Click(f32, f32),
}

/// The keys `overlay.input key` names, with their virtual-key codes.
const KEYS: &[(&str, u16)] = &[
    ("enter", 0x0D),
    ("escape", 0x1B),
    ("tab", 0x09),
    ("backspace", 0x08),
    ("delete", 0x2E),
    ("up", 0x26),
    ("down", 0x28),
    ("left", 0x25),
    ("right", 0x27),
    ("home", 0x24),
    ("end", 0x23),
];

/// The usage line `overlay.input` registers.
pub(crate) const USAGE: &str = "type <text> | key <enter|escape|tab|backspace|delete|up|down|left|right|home|end> | click <x> <y>";

/// Parse the arguments of `overlay.input`.
pub(crate) fn parse(args: &str) -> Result<Input, String> {
    let args = args.trim_start();
    let (verb, rest) = args.split_once(' ').unwrap_or((args, ""));
    match verb {
        "type" if !rest.is_empty() => Ok(Input::Type(rest.to_owned())),
        "key" => KEYS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(rest.trim()))
            .map(|&(_, vk)| Input::Key(vk))
            .ok_or_else(|| format!("which key? {:?} is not one of {USAGE}", rest.trim())),
        "click" => {
            let mut numbers = rest.split_whitespace().map(str::parse::<f32>);
            match (numbers.next(), numbers.next(), numbers.next()) {
                (Some(Ok(x)), Some(Ok(y)), None) => Ok(Input::Click(x, y)),
                _ => Err(format!("click where? expected two numbers, got {rest:?}")),
            }
        }
        _ => Err(format!("expected {USAGE}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_keeps_its_spaces() {
        assert_eq!(parse("type help me"), Ok(Input::Type("help me".into())));
    }

    #[test]
    fn keys_are_named() {
        assert_eq!(parse("key Enter"), Ok(Input::Key(0x0D)));
        assert!(parse("key f13").is_err());
    }

    #[test]
    fn a_click_needs_two_numbers() {
        assert_eq!(parse("click 10 20.5"), Ok(Input::Click(10.0, 20.5)));
        assert!(parse("click 10").is_err());
        assert!(parse("click 1 2 3").is_err());
        assert!(parse("type").is_err());
        assert!(parse("wave").is_err());
    }
}
