//! A plugin's `config/<plugin>.toml`, edited in place, and the rules a value must pass
//! before it goes in.
//!
//! The loader stores every value as a TOML string and reads other TOML types as their text
//! (`uniloader_core::store`); the descriptor, not the file, says what type a value is. A
//! stored value the descriptor refuses is replaced by the default when the plugin starts.

use argon2::password_hash::{PasswordHasher, SaltString};
use argon2::Argon2;
use password_hash::rand_core::OsRng;
use toml_edit::{DocumentMut, Item, Value};
use uniloader_core::schema::Setting;
use uniloader_core::settings;

/// A parsed settings file.
#[derive(Debug, Clone)]
pub struct SettingsFile {
    doc: DocumentMut,
}

impl SettingsFile {
    /// Parse the file's text (empty for a missing file).
    ///
    /// # Errors
    /// The text is not TOML.
    pub fn parse(text: &str) -> Result<Self, toml_edit::TomlError> {
        Ok(SettingsFile { doc: text.parse()? })
    }

    /// The text to write.
    #[must_use]
    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// The stored value of `key` as the loader reads it, `None` when it is not stored.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<String> {
        match self.doc.get(key)? {
            Item::Value(Value::String(s)) => Some(s.value().clone()),
            Item::Value(other) => {
                let mut bare = other.clone();
                bare.decor_mut().clear();
                Some(bare.to_string())
            }
            _ => None,
        }
    }

    /// Store `value` for `key` as a string, keeping a comment that follows an old value.
    pub fn set(&mut self, key: &str, value: &str) {
        if let Some(old) = self.doc.get_mut(key).and_then(Item::as_value_mut) {
            let decor = old.decor().clone();
            *old = Value::from(value);
            *old.decor_mut() = decor;
            return;
        }
        self.doc[key] = toml_edit::value(value);
    }

    /// Forget `key`, so the plugin starts with its default.
    pub fn remove(&mut self, key: &str) {
        self.doc.remove(key);
    }

    /// Every stored key, in file order.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.doc.iter().map(|(k, _)| k.to_owned()).collect()
    }
}

/// What the editor shows for a setting's value: the value, or only whether a secret is set.
#[must_use]
pub fn shown(setting: &Setting, value: &str) -> String {
    if setting.desc.secret {
        if value.is_empty() { "<unset>" } else { "<set>" }.to_owned()
    } else {
        value.to_owned()
    }
}

/// The text to store for what the user entered: the canonical form the loader would store,
/// or an Argon2id hash for a hashed setting (empty clears it).
///
/// # Errors
/// Why the loader would refuse it, in a sentence that never repeats a secret.
pub fn prepare(setting: &Setting, input: &str) -> Result<String, String> {
    if let Some(hashed) = &setting.hashed {
        if input.is_empty() {
            return Ok(String::new());
        }
        if input.chars().count() < hashed.min_chars {
            return Err(format!("Use at least {} characters.", hashed.min_chars));
        }
        return hash(input);
    }
    settings::validate(&setting.desc, input).map_err(|e| {
        if setting.desc.secret {
            "That is not a valid value.".to_owned()
        } else {
            format!("{e}.")
        }
    })
}

/// What is wrong with a stored value, from the loader's point of view; `None` when it is
/// fine.
#[must_use]
pub fn stored_problem(setting: &Setting, stored: &str) -> Option<String> {
    if setting.hashed.is_some() {
        return (!stored.is_empty() && !stored.starts_with("$argon2")).then(|| {
            "The stored value is not a hash, so no password will match it. Set it again.".to_owned()
        });
    }
    let error = settings::validate(&setting.desc, stored).err()?;
    Some(if setting.desc.secret {
        "The stored value is not valid; the plugin uses its default.".to_owned()
    } else {
        format!("{error}; the plugin uses its default.")
    })
}

/// Hash a password the way the loader does (`crates/uniloader/src/secret.rs`): Argon2id with
/// default parameters and a random salt, as a PHC string.
fn hash(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("Could not hash it: {e}."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    use uniloader_core::schema::Hashed;
    use uniloader_core::settings::{Desc, Kind};

    fn setting(key: &str, kind: Kind, default: &str) -> Setting {
        Setting {
            desc: Desc {
                key: key.into(),
                title: key.into(),
                description: String::new(),
                kind,
                default: default.into(),
                min: 0.0,
                max: 0.0,
                choices: Vec::new(),
                labels: Vec::new(),
                restart_required: false,
                advanced: false,
                transient: false,
                secret: false,
            },
            hashed: None,
        }
    }

    #[test]
    fn values_are_read_as_the_loader_reads_them_and_written_as_strings() {
        let mut f = SettingsFile::parse(
            "# my notes\nspeed = \"2\" # fast\nn = 42\nb = true\n\"stereo.ipd\" = \"0.064\"\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(f.get("speed").as_deref(), Some("2"));
        assert_eq!(f.get("n").as_deref(), Some("42"));
        assert_eq!(f.get("b").as_deref(), Some("true"));
        assert_eq!(f.get("stereo.ipd").as_deref(), Some("0.064"));
        assert_eq!(f.get("missing"), None);
        f.set("speed", "1.5");
        f.set("n", "7");
        f.set("new", "x \" y");
        f.remove("b");
        let text = f.text();
        assert!(
            text.starts_with("# my notes\nspeed = \"1.5\" # fast\n"),
            "{text}"
        );
        assert!(text.contains("n = \"7\""), "{text}");
        assert!(!text.contains("b = true"), "{text}");
        // What the loader's own reader makes of it.
        let dir = crate::test_dir::TestDir::new("settings-file");
        let path = dir.path().join("p.toml");
        crate::test_dir::TestDir::write(&path, &text);
        let read = uniloader_core::store::read(&path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read["new"], "x \" y");
        assert_eq!(read["speed"], "1.5");
        assert_eq!(f.keys(), ["speed", "n", "stereo.ipd", "new"]);
    }

    #[test]
    fn input_is_validated_and_made_canonical() {
        let mut s = setting("speed", Kind::Float, "1");
        s.desc.min = 0.1;
        s.desc.max = 4.0;
        assert_eq!(prepare(&s, " 2.50 ").as_deref(), Ok("2.5"));
        let error = prepare(&s, "9").err().unwrap_or_default();
        assert!(error.contains("outside"), "{error}");
        let flag = setting("on", Kind::Bool, "false");
        assert_eq!(prepare(&flag, "yes").as_deref(), Ok("true"));
        assert_eq!(
            stored_problem(&flag, "maybe").as_deref(),
            Some("on: \"maybe\" is not a boolean (use true/false); the plugin uses its default.")
        );
        assert_eq!(stored_problem(&flag, "false"), None);
    }

    #[test]
    fn secrets_are_never_repeated() {
        let mut s = setting("token", Kind::Int, "0");
        s.desc.secret = true;
        let error = prepare(&s, "hunter2").err().unwrap_or_default();
        assert!(error.contains("not a valid value"), "{error}");
        assert!(!error.contains("hunter2"), "{error}");
        let problem = stored_problem(&s, "hunter2").unwrap_or_default();
        assert!(!problem.contains("hunter2"), "{problem}");
        assert_eq!(shown(&s, "123"), "<set>");
        assert_eq!(shown(&s, ""), "<unset>");
    }

    #[test]
    fn a_hashed_setting_stores_a_hash_the_loader_verifies() {
        let mut s = setting("rcon_password", Kind::String, "");
        s.desc.secret = true;
        s.hashed = Some(Hashed { min_chars: 12 });
        assert!(prepare(&s, "short").is_err());
        assert_eq!(prepare(&s, "").as_deref(), Ok(""));
        let stored = prepare(&s, "a long enough password").unwrap_or_else(|e| panic!("{e}"));
        assert!(stored.starts_with("$argon2id$"), "{stored}");
        let parsed = PasswordHash::new(&stored).unwrap_or_else(|e| panic!("{e}"));
        assert!(Argon2::default()
            .verify_password(b"a long enough password", &parsed)
            .is_ok());
        assert_eq!(stored_problem(&s, &stored), None);
        assert!(stored_problem(&s, "plain text").is_some());
    }
}
