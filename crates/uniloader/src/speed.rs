//! The loader's own entry in the plugin list, and the game speed setting it carries.
//!
//! The loader registers itself as the plugin `uniloader`, so its settings, hotkeys and
//! commands are read, set, rebound, listed, saved and shown in the overlay's settings editor
//! exactly as a plugin's are, and a plugin reads `uniloader.speed` with an ordinary `get`.
//! What the setting does is the platform layer's business (`win::speed`).

use uniloader_core::settings;

use crate::state::{Builtin, CommandInfo, Described};

/// The loader's name in the plugin list.
pub const NAME: &str = "uniloader";
/// The game speed setting's key.
pub const SPEED: &str = "speed";
/// Slowest speed the setting allows.
const MIN: f64 = 0.1;
/// Fastest speed the setting allows. Beyond it physics steps get long enough to misbehave.
const MAX: f64 = 4.0;
/// Steps per unit of speed: one `speed_up` or `speed_down` moves it by a tenth.
///
/// A count of steps rather than a step of 0.1, because 0.1 has no exact binary form and
/// eleven of them print as `1.1000000000000001`; eleven tenths print as `1.1`.
const STEPS: f64 = 10.0;

/// The hotkey and console actions that change the speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// One step faster.
    Up,
    /// One step slower.
    Down,
    /// Back to normal speed.
    Reset,
}

impl Action {
    /// Every action with its name and one-line help, in display order.
    const ALL: [(Action, &'static str, &'static str); 3] = [
        (Action::Up, "speed_up", "Run the game 0.1 faster."),
        (Action::Down, "speed_down", "Run the game 0.1 slower."),
        (
            Action::Reset,
            "speed_reset",
            "Run the game at normal speed.",
        ),
    ];

    /// The action of a hotkey or command name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Action> {
        Self::ALL
            .iter()
            .find(|(_, n, _)| *n == name)
            .map(|(a, _, _)| *a)
    }

    /// The speed after this action, starting from `current`, kept in range and on a step.
    #[must_use]
    pub fn apply(self, current: f64) -> f64 {
        let steps = (current * STEPS).round();
        let next = match self {
            Action::Up => steps + 1.0,
            Action::Down => steps - 1.0,
            Action::Reset => return 1.0,
        };
        (next / STEPS).clamp(MIN, MAX)
    }
}

/// The game speed setting.
fn speed_setting() -> settings::Desc {
    settings::Desc {
        key: SPEED.to_owned(),
        title: "Game speed".to_owned(),
        description: "How fast the game's clocks run: 1 is normal, 1.2 is a fifth faster. \
                      Everything the game times with them speeds up, physics and network \
                      timeouts included, so multiplayer plugins may hold it at 1."
            .to_owned(),
        kind: settings::Kind::Float,
        default: "1".to_owned(),
        min: MIN,
        max: MAX,
        choices: Vec::new(),
        restart_required: false,
        advanced: false,
        transient: false,
    }
}

/// The loader's plugin entry: what it says about itself and what it registers.
///
/// The speed hotkeys are unbound: games and plugins have already taken most of the function
/// keys, and a key that changes the game's speed by accident is worse than one to bind.
#[must_use]
pub fn builtin(file: &str) -> (Described, Builtin) {
    let info = Described {
        name: NAME.to_owned(),
        title: "uniloader".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        authors: String::new(),
        description: "The loader itself: game speed.".to_owned(),
    };
    let actions = Action::ALL.iter().map(|(_, name, help)| (*name, *help));
    let builtin = Builtin {
        file: file.to_owned(),
        settings: vec![speed_setting()],
        hotkeys: actions
            .clone()
            .map(|(name, help)| (name.to_owned(), help.trim_end_matches('.').to_owned()))
            .map(|(name, title)| (name, title, String::new()))
            .collect(),
        commands: actions
            .map(|(name, help)| {
                (
                    name.to_owned(),
                    CommandInfo {
                        help: help.to_owned(),
                        usage: String::new(),
                    },
                )
            })
            .collect(),
    };
    (info, builtin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::state;
    use uniloader_api::Status;

    #[test]
    fn steps_stay_in_range_and_print_short() {
        assert_eq!(Action::Up.apply(1.0).to_string(), "1.1");
        let mut speed = 1.0;
        for _ in 0..10 {
            speed = Action::Up.apply(speed);
        }
        assert_eq!(speed.to_string(), "2");
        for _ in 0..100 {
            speed = Action::Up.apply(speed);
        }
        assert_eq!(speed.to_string(), "4");
        for _ in 0..100 {
            speed = Action::Down.apply(speed);
        }
        assert_eq!(speed.to_string(), "0.1");
        assert_eq!(Action::Reset.apply(speed).to_string(), "1");
        // A typed value off the step snaps to it.
        assert_eq!(Action::Down.apply(1.234).to_string(), "1.1");
        assert_eq!(Action::parse("speed_up"), Some(Action::Up));
        assert_eq!(Action::parse("speed"), None);
    }

    #[test]
    fn the_loader_entry_is_a_plugin_with_settings_hotkeys_and_commands() {
        let mut s = state();
        let (info, builtin) = builtin("dwmapi.dll");
        let handle = s
            .add_builtin(info, builtin)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(s.get_setting(None, "uniloader.speed").as_deref(), Ok("1"));
        // A plugin reads it like any other plugin's setting, and may force it.
        let other = s
            .add_plugin(crate::state::tests::described("coop"), "coop.dll")
            .unwrap_or_else(|e| panic!("{e:?}"));
        let changed = s
            .set_setting(Some(other), "uniloader.speed", "1.5")
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(changed.notify.is_some());
        assert_eq!(
            s.get_setting(Some(other), "uniloader.speed").as_deref(),
            Ok("1.5")
        );
        assert!(s.set_setting(None, "uniloader.speed", "9").is_err());
        assert!(s.set_setting(None, "uniloader.speed", "0").is_err());
        let hotkeys: Vec<String> = s
            .binds
            .entries()
            .map(uniloader_core::binds::Entry::binding)
            .collect();
        assert_eq!(hotkeys, ["none", "none", "none"]);
        assert!(s.binds.entries().any(|e| e.name == "uniloader.speed_up"));
        let record = s.plugin(handle).unwrap_or_else(|| panic!("no record"));
        assert!(record.enabled);
        assert!(record.commands.contains_key("speed_reset"));
        // Registration is over: the phase is back where it was.
        assert_eq!(s.phase, crate::state::Phase::Idle);
        let (info, builtin) = super::builtin("dwmapi.dll");
        assert_eq!(s.add_builtin(info, builtin), Err(Status::INVALID_ARGUMENT));
        let _ = std::fs::remove_dir_all(&s.paths.game_dir);
    }
}
