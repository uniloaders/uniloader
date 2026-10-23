//! Typed setting descriptors, validation and the per-plugin value registry.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::values;

/// Value type of a setting. Mirrors `uniloader_api::SettingKind`.
///
/// Serialised as the lower case word (`date_time` for [`Kind::DateTime`]) in schema files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// `true` / `false`.
    Bool,
    /// Integer in range.
    Int,
    /// Float in range.
    Float,
    /// One of `choices`.
    Enum,
    /// Free text.
    String,
    /// A file path; `choices` are the file name extensions a picker offers.
    File,
    /// A folder path.
    Directory,
    /// `#rrggbb` or `#rrggbbaa` ([`values::colour`]).
    Colour,
    /// `YYYY-MM-DD`.
    Date,
    /// `HH:MM` or `HH:MM:SS`.
    Time,
    /// `YYYY-MM-DD HH:MM[:SS]`.
    DateTime,
    /// Any set of `choices`, comma separated.
    Flags,
    /// A key binding list in the hotkey grammar, `none` when unbound.
    Key,
}

/// Everything the loader knows about one setting.
// Four independent yes/no properties mirroring the ABI's `SettingFlags` bits; each is read
// by name all over the loader and the editors, which a bit set would only make noisier.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Desc {
    /// Key within the plugin namespace.
    pub key: String,
    /// Short label.
    pub title: String,
    /// Help text.
    pub description: String,
    /// Value type.
    pub kind: Kind,
    /// Default value as text.
    pub default: String,
    /// Lower bound for numeric kinds.
    pub min: f64,
    /// Upper bound for numeric kinds.
    pub max: f64,
    /// Allowed values for [`Kind::Enum`].
    pub choices: Vec<String>,
    /// Takes effect after restart only.
    pub restart_required: bool,
    /// Hidden from the settings editor until the user asks for advanced settings.
    ///
    /// For the knobs that exist because something might need changing once — a port, a
    /// timeout, a diagnostic switch — rather than the ones a user came to the editor for.
    /// Nothing else treats them differently: they are listed, read and written as usual.
    pub advanced: bool,
    /// Never persisted.
    pub transient: bool,
    /// A password or token: never printed, listed or logged by the console, which shows
    /// `<set>` or `<unset>` instead.
    pub secret: bool,
}

/// Why a value or descriptor was rejected.
#[derive(Debug, Error, PartialEq)]
pub enum SettingError {
    /// Not a boolean word.
    #[error("{key}: {value:?} is not a boolean (use true/false)")]
    NotBool {
        /// Setting key.
        key: String,
        /// Offending text.
        value: String,
    },
    /// Not a number of the right kind.
    #[error("{key}: {value:?} is not a number")]
    NotNumber {
        /// Setting key.
        key: String,
        /// Offending text.
        value: String,
    },
    /// Outside `[min, max]`.
    #[error("{key}: {value} is outside {min}..={max}")]
    OutOfRange {
        /// Setting key.
        key: String,
        /// Parsed number.
        value: f64,
        /// Lower bound.
        min: f64,
        /// Upper bound.
        max: f64,
    },
    /// Not one of the choices.
    #[error("{key}: {value:?} is not one of {choices:?}")]
    NotAChoice {
        /// Setting key.
        key: String,
        /// Offending text.
        value: String,
        /// Allowed values.
        choices: Vec<String>,
    },
    /// Not in the kind's text form (a colour, a date, a key binding).
    #[error("{key}: {value:?} is not {expected}")]
    BadFormat {
        /// Setting key.
        key: String,
        /// Offending text.
        value: String,
        /// What was expected, for the message.
        expected: &'static str,
    },
    /// Key already registered.
    #[error("setting {0:?} is already registered")]
    Duplicate(String),
    /// Key not registered.
    #[error("setting {0:?} is not registered")]
    Unknown(String),
    /// The descriptor's own default does not validate.
    #[error("default of {0}: {1}")]
    BadDefault(String, Box<SettingError>),
}

/// Normalise and validate `value` against `desc`. Returns the canonical text form.
///
/// # Errors
/// See [`SettingError`].
pub fn validate(desc: &Desc, value: &str) -> Result<String, SettingError> {
    let text = value.trim();
    let key = &desc.key;
    match desc.kind {
        Kind::Bool => match text.to_ascii_lowercase().as_str() {
            "true" | "1" | "on" | "yes" => Ok("true".to_owned()),
            "false" | "0" | "off" | "no" => Ok("false".to_owned()),
            _ => Err(SettingError::NotBool {
                key: key.clone(),
                value: value.to_owned(),
            }),
        },
        Kind::Int => {
            let n: i64 = text.parse().map_err(|_| SettingError::NotNumber {
                key: key.clone(),
                value: value.to_owned(),
            })?;
            // Bounds are stored as f64 for one ABI field; i64 -> f64 is exact for any sane range.
            #[allow(clippy::cast_precision_loss)]
            let as_float = n as f64;
            check_range(desc, as_float)?;
            Ok(n.to_string())
        }
        Kind::Float => {
            let n: f64 = text.parse().map_err(|_| SettingError::NotNumber {
                key: key.clone(),
                value: value.to_owned(),
            })?;
            if !n.is_finite() {
                return Err(SettingError::NotNumber {
                    key: key.clone(),
                    value: value.to_owned(),
                });
            }
            check_range(desc, n)?;
            Ok(n.to_string())
        }
        Kind::Enum => desc
            .choices
            .iter()
            .find(|c| c.eq_ignore_ascii_case(text))
            .cloned()
            .ok_or_else(|| SettingError::NotAChoice {
                key: key.clone(),
                value: value.to_owned(),
                choices: desc.choices.clone(),
            }),
        Kind::String | Kind::File | Kind::Directory => Ok(text.to_owned()),
        Kind::Flags => values::flags(text, &desc.choices).map_err(|bad| SettingError::NotAChoice {
            key: key.clone(),
            value: bad,
            choices: desc.choices.clone(),
        }),
        Kind::Colour | Kind::Date | Kind::Time | Kind::DateTime | Kind::Key => {
            let (parsed, expected) = match desc.kind {
                Kind::Colour => (values::colour(text), "a colour like #ff8000"),
                Kind::Date => (values::date(text), "a date like 2026-10-19"),
                Kind::Time => (values::time(text), "a time like 08:30"),
                Kind::DateTime => (
                    values::date_time(text),
                    "a date and time like 2026-10-19 08:30",
                ),
                _ => (values::key(text), "a key binding like f8 or ctrl+shift+c"),
            };
            parsed.ok_or_else(|| SettingError::BadFormat {
                key: key.clone(),
                value: value.to_owned(),
                expected,
            })
        }
    }
}

fn check_range(desc: &Desc, n: f64) -> Result<(), SettingError> {
    let bounded = desc.min < desc.max;
    if bounded && (n < desc.min || n > desc.max) {
        return Err(SettingError::OutOfRange {
            key: desc.key.clone(),
            value: n,
            min: desc.min,
            max: desc.max,
        });
    }
    Ok(())
}

/// One plugin's settings: descriptors plus current values, in registration order.
#[derive(Debug, Default)]
pub struct Registry {
    entries: Vec<(Desc, String)>,
}

/// Outcome of a successful [`Registry::set`].
#[derive(Debug, PartialEq, Eq)]
pub struct Changed {
    /// Canonical new value.
    pub value: String,
    /// Whether the value differs from before (callers skip notifications otherwise).
    pub differs: bool,
    /// Whether the value should be written to disk.
    pub persist: bool,
}

impl Registry {
    /// Register a descriptor, taking the initial value from `stored` when present and valid.
    ///
    /// # Errors
    /// Duplicate key, or a default that fails its own validation.
    pub fn register(&mut self, desc: Desc, stored: Option<&str>) -> Result<(), SettingError> {
        if self.entries.iter().any(|(d, _)| d.key == desc.key) {
            return Err(SettingError::Duplicate(desc.key));
        }
        let default = validate(&desc, &desc.default)
            .map_err(|e| SettingError::BadDefault(desc.key.clone(), Box::new(e)))?;
        let value = stored
            .and_then(|s| validate(&desc, s).ok())
            .unwrap_or(default);
        self.entries.push((desc, value));
        Ok(())
    }

    /// Current canonical value.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(d, _)| d.key == key)
            .map(|(_, v)| v.as_str())
    }

    /// Descriptor of a key.
    #[must_use]
    pub fn desc(&self, key: &str) -> Option<&Desc> {
        self.entries
            .iter()
            .find(|(d, _)| d.key == key)
            .map(|(d, _)| d)
    }

    /// Validate and store a new value.
    ///
    /// # Errors
    /// Unknown key or invalid value.
    pub fn set(&mut self, key: &str, value: &str) -> Result<Changed, SettingError> {
        let Some((desc, current)) = self.entries.iter_mut().find(|(d, _)| d.key == key) else {
            return Err(SettingError::Unknown(key.to_owned()));
        };
        let canonical = validate(desc, value)?;
        let differs = *current != canonical;
        current.clone_from(&canonical);
        Ok(Changed {
            value: canonical,
            differs,
            persist: !desc.transient,
        })
    }

    /// All descriptors with their values, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = (&Desc, &str)> {
        self.entries.iter().map(|(d, v)| (d, v.as_str()))
    }

    /// Values that belong in the config file.
    #[must_use]
    pub fn persistent_values(&self) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .filter(|(d, _)| !d.transient)
            .map(|(d, v)| (d.key.clone(), v.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(key: &str, kind: Kind, default: &str) -> Desc {
        Desc {
            key: key.into(),
            title: String::new(),
            description: String::new(),
            kind,
            default: default.into(),
            min: 0.0,
            max: 0.0,
            choices: Vec::new(),
            restart_required: false,
            advanced: false,
            transient: false,
            secret: false,
        }
    }

    #[test]
    fn bool_accepts_common_spellings() {
        let d = desc("on", Kind::Bool, "false");
        for yes in ["true", "1", "ON", "yes"] {
            assert_eq!(validate(&d, yes).as_deref(), Ok("true"));
        }
        assert!(matches!(
            validate(&d, "maybe"),
            Err(SettingError::NotBool { .. })
        ));
    }

    #[test]
    fn numbers_respect_bounds_only_when_set() {
        let mut d = desc("ipd", Kind::Float, "0.064");
        assert_eq!(validate(&d, "9999").as_deref(), Ok("9999"));
        d.min = 0.0;
        d.max = 0.1;
        assert!(matches!(
            validate(&d, "0.5"),
            Err(SettingError::OutOfRange { .. })
        ));
        assert!(matches!(
            validate(&d, "nan"),
            Err(SettingError::NotNumber { .. })
        ));
        let i = Desc {
            kind: Kind::Int,
            ..d
        };
        assert!(matches!(
            validate(&i, "1.5"),
            Err(SettingError::NotNumber { .. })
        ));
    }

    #[test]
    fn enum_is_case_insensitive_and_canonical() {
        let d = Desc {
            choices: vec!["Left".into(), "Right".into()],
            ..desc("eye", Kind::Enum, "Left")
        };
        assert_eq!(validate(&d, "right").as_deref(), Ok("Right"));
        assert!(matches!(
            validate(&d, "up"),
            Err(SettingError::NotAChoice { .. })
        ));
    }

    #[test]
    fn rich_kinds_store_their_canonical_form() {
        assert_eq!(
            validate(&desc("tint", Kind::Colour, "#fff"), "F80").as_deref(),
            Ok("#ff8800")
        );
        assert!(matches!(
            validate(&desc("day", Kind::Date, "2026-01-01"), "2026-02-30"),
            Err(SettingError::BadFormat { .. })
        ));
        assert_eq!(
            validate(&desc("key", Kind::Key, "f8"), "Ctrl+F9").as_deref(),
            Ok("ctrl+f9")
        );
        let shown = Desc {
            choices: vec!["Map".into(), "Chat".into()],
            ..desc("shown", Kind::Flags, "")
        };
        assert_eq!(validate(&shown, "chat,map").as_deref(), Ok("Map,Chat"));
        assert!(matches!(
            validate(&shown, "radio"),
            Err(SettingError::NotAChoice { .. })
        ));
    }

    #[test]
    fn registry_prefers_valid_stored_values() {
        let mut r = Registry::default();
        r.register(desc("a", Kind::Int, "1"), Some("5"))
            .unwrap_or_else(|e| panic!("{e}"));
        r.register(desc("b", Kind::Int, "1"), Some("junk"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.get("a"), Some("5"));
        assert_eq!(r.get("b"), Some("1"));
        assert_eq!(
            r.register(desc("a", Kind::Int, "1"), None),
            Err(SettingError::Duplicate("a".into()))
        );
        assert!(matches!(
            r.register(desc("c", Kind::Bool, "x"), None),
            Err(SettingError::BadDefault(..))
        ));
    }

    #[test]
    fn set_reports_change_and_persistence() {
        let mut r = Registry::default();
        let mut t = desc("live", Kind::Bool, "true");
        t.transient = true;
        r.register(t, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            r.set("live", "0"),
            Ok(Changed {
                value: "false".into(),
                differs: true,
                persist: false
            })
        );
        assert_eq!(r.set("live", "off").map(|c| c.differs), Ok(false));
        assert_eq!(
            r.set("nope", "1"),
            Err(SettingError::Unknown("nope".into()))
        );
        assert!(r.persistent_values().is_empty());
    }
}
