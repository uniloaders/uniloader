//! Builder for setting descriptors.

use uniloader_api::SettingKind as Abi;

/// Value type of a setting. More may come: a kind a UI does not know is edited as text,
/// which every value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SettingKind {
    /// `true` / `false`.
    Bool,
    /// Integer in range.
    Int,
    /// Float in range.
    Float,
    /// One of the choices.
    Enum,
    /// Free text.
    String,
    /// A file path; the choices are the extensions a file picker offers.
    File,
    /// A folder path.
    Directory,
    /// `#rrggbb` or `#rrggbbaa`.
    Colour,
    /// `YYYY-MM-DD`.
    Date,
    /// `HH:MM` or `HH:MM:SS`.
    Time,
    /// `YYYY-MM-DD HH:MM[:SS]`.
    DateTime,
    /// Any set of the choices, comma separated.
    Flags,
    /// A key binding list in the hotkey grammar, `none` when unbound.
    Key,
}

const KINDS: [(SettingKind, Abi); 13] = [
    (SettingKind::Bool, Abi::BOOL),
    (SettingKind::Int, Abi::INT),
    (SettingKind::Float, Abi::FLOAT),
    (SettingKind::Enum, Abi::ENUM),
    (SettingKind::String, Abi::STRING),
    (SettingKind::File, Abi::FILE),
    (SettingKind::Directory, Abi::DIRECTORY),
    (SettingKind::Colour, Abi::COLOUR),
    (SettingKind::Date, Abi::DATE),
    (SettingKind::Time, Abi::TIME),
    (SettingKind::DateTime, Abi::DATETIME),
    (SettingKind::Flags, Abi::FLAGS),
    (SettingKind::Key, Abi::KEY),
];

impl SettingKind {
    /// The ABI value.
    pub(crate) fn to_abi(self) -> Abi {
        KINDS
            .iter()
            .find(|(k, _)| *k == self)
            .map_or(Abi::STRING, |(_, a)| *a)
    }

    /// From the ABI value; text for a kind a newer loader adds, as every value is text.
    pub(crate) fn from_abi(abi: Abi) -> Self {
        KINDS
            .iter()
            .find(|(_, a)| *a == abi)
            .map_or(SettingKind::String, |(k, _)| *k)
    }
}

/// A setting a plugin registers. Build with the constructors, tweak with the `with_*` methods.
// Four independent yes/no properties mirroring the ABI's `SettingFlags` bits; each is read
// by name all over the loader and the editors, which a bit set would only make noisier.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) kind: SettingKind,
    pub(crate) default: String,
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) choices: Vec<String>,
    pub(crate) restart_required: bool,
    pub(crate) advanced: bool,
    pub(crate) transient: bool,
    pub(crate) secret: bool,
}

impl Setting {
    fn new(key: &str, title: &str, kind: SettingKind, default: String) -> Self {
        Setting {
            key: key.to_owned(),
            title: title.to_owned(),
            description: String::new(),
            kind,
            default,
            min: 0.0,
            max: 0.0,
            choices: Vec::new(),
            restart_required: false,
            advanced: false,
            transient: false,
            secret: false,
        }
    }

    /// A boolean setting.
    #[must_use]
    pub fn bool(key: &str, title: &str, default: bool) -> Self {
        Self::new(key, title, SettingKind::Bool, default.to_string())
    }

    /// An integer setting; pass `min == max` for unbounded.
    #[must_use]
    pub fn int(key: &str, title: &str, default: i64, min: i64, max: i64) -> Self {
        // Bounds cross the ABI as f64; game-scale integers are far below 2^53.
        #[allow(clippy::cast_precision_loss)]
        let (min, max) = (min as f64, max as f64);
        Self {
            min,
            max,
            ..Self::new(key, title, SettingKind::Int, default.to_string())
        }
    }

    /// A float setting; pass `min == max` for unbounded.
    #[must_use]
    pub fn float(key: &str, title: &str, default: f64, min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            ..Self::new(key, title, SettingKind::Float, default.to_string())
        }
    }

    /// A choice setting; `default` must be one of `choices`.
    #[must_use]
    pub fn choice(key: &str, title: &str, default: &str, choices: &[&str]) -> Self {
        Self {
            choices: choices.iter().map(|&c| c.to_owned()).collect(),
            ..Self::new(key, title, SettingKind::Enum, default.to_owned())
        }
    }

    /// A free text setting.
    #[must_use]
    pub fn text(key: &str, title: &str, default: &str) -> Self {
        Self::new(key, title, SettingKind::String, default.to_owned())
    }

    /// A file path; a picker offers files with one of `extensions` (without the dot), or
    /// any file when it is empty.
    #[must_use]
    pub fn file(key: &str, title: &str, default: &str, extensions: &[&str]) -> Self {
        Self {
            choices: extensions.iter().map(|&e| e.to_owned()).collect(),
            ..Self::new(key, title, SettingKind::File, default.to_owned())
        }
    }

    /// A folder path.
    #[must_use]
    pub fn directory(key: &str, title: &str, default: &str) -> Self {
        Self::new(key, title, SettingKind::Directory, default.to_owned())
    }

    /// A colour, `#rrggbb` or `#rrggbbaa`.
    #[must_use]
    pub fn colour(key: &str, title: &str, default: &str) -> Self {
        Self::new(key, title, SettingKind::Colour, default.to_owned())
    }

    /// A date, `YYYY-MM-DD`.
    #[must_use]
    pub fn date(key: &str, title: &str, default: &str) -> Self {
        Self::new(key, title, SettingKind::Date, default.to_owned())
    }

    /// A time of day, `HH:MM` or `HH:MM:SS`.
    #[must_use]
    pub fn time(key: &str, title: &str, default: &str) -> Self {
        Self::new(key, title, SettingKind::Time, default.to_owned())
    }

    /// A date and time, `YYYY-MM-DD HH:MM[:SS]`.
    #[must_use]
    pub fn date_time(key: &str, title: &str, default: &str) -> Self {
        Self::new(key, title, SettingKind::DateTime, default.to_owned())
    }

    /// Any set of `choices`; `default` lists the ones on at first.
    #[must_use]
    pub fn flags(key: &str, title: &str, default: &[&str], choices: &[&str]) -> Self {
        Self {
            choices: choices.iter().map(|&c| c.to_owned()).collect(),
            ..Self::new(key, title, SettingKind::Flags, default.join(","))
        }
    }

    /// A key binding in the hotkey grammar (`f8`, `ctrl+shift+c`); `none` for unbound.
    ///
    /// For a key the plugin reads itself. An action that should simply fire on a key is a
    /// hotkey (`Host::hotkey`), which the loader watches and the settings UIs rebind too.
    #[must_use]
    pub fn key(key: &str, title: &str, default: &str) -> Self {
        Self::new(key, title, SettingKind::Key, default.to_owned())
    }

    /// Longer help text shown in UI and `help`.
    #[must_use]
    pub fn with_description(mut self, description: &str) -> Self {
        description.clone_into(&mut self.description);
        self
    }

    /// Mark the setting as taking effect only after the game restarts.
    #[must_use]
    pub fn restart_required(mut self) -> Self {
        self.restart_required = true;
        self
    }

    /// Mark the setting as a runtime variable that is never written to disk.
    #[must_use]
    pub fn transient(mut self) -> Self {
        self.transient = true;
        self
    }

    /// Mark the setting as a password or token: the console, settings lists and the log show
    /// `<set>` or `<unset>` instead of its value. [`crate::Host::get`] still returns it.
    #[must_use]
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// Hide the setting from the settings editor unless the user asks for advanced settings.
    ///
    /// For the ones that exist in case something ever needs changing — a port, a timeout, a
    /// diagnostic switch — so the editor shows what a user came for. The setting is otherwise
    /// completely normal: still listed by `list`, still readable and writable.
    #[must_use]
    pub fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }
}
