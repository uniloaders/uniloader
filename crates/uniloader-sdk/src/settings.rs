//! Builder for setting descriptors.

/// Value type of a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

/// A setting a plugin registers. Build with the constructors, tweak with the `with_*` methods.
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
