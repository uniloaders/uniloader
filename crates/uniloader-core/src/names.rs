//! Validation of the identifiers plugins register: plugin names, setting keys, actions.

use thiserror::Error;

/// Why an identifier was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NameError {
    /// Empty identifier.
    #[error("identifier is empty")]
    Empty,
    /// Longer than [`MAX_LEN`].
    #[error("identifier {0:?} is longer than {MAX_LEN} bytes")]
    TooLong(String),
    /// Contains a character outside the allowed set.
    #[error("identifier {0:?} may only contain lowercase letters, digits, '_', '-' and '.'")]
    BadChar(String),
}

/// Longest identifier accepted.
pub const MAX_LEN: usize = 64;

fn check(name: &str, allow_dot: bool) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > MAX_LEN {
        return Err(NameError::TooLong(name.to_owned()));
    }
    let ok = name.bytes().all(|b| {
        b.is_ascii_lowercase()
            || b.is_ascii_digit()
            || b == b'_'
            || b == b'-'
            || (allow_dot && b == b'.')
    });
    if ok {
        Ok(())
    } else {
        Err(NameError::BadChar(name.to_owned()))
    }
}

/// Validate a plugin name: `[a-z0-9_-]+`, no dots (the dot separates plugin and key).
///
/// # Errors
/// Returns the first rule the name violates.
pub fn plugin_name(name: &str) -> Result<(), NameError> {
    check(name, false)
}

/// Validate a setting key: `[a-z0-9_.-]+`, dots allowed for grouping (`stereo.ipd`).
///
/// # Errors
/// Returns the first rule the key violates.
pub fn setting_key(key: &str) -> Result<(), NameError> {
    check(key, true)
}

/// Validate a hotkey action or command name: `[a-z0-9_-]+`.
///
/// # Errors
/// Returns the first rule the name violates.
pub fn action_name(name: &str) -> Result<(), NameError> {
    check(name, false)
}

/// Split `plugin.key` into its parts, or return `None` when there is no dot.
#[must_use]
pub fn split_qualified(name: &str) -> Option<(&str, &str)> {
    name.split_once('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_names() {
        assert_eq!(plugin_name("jump"), Ok(()));
        assert_eq!(plugin_name("tsw-coop_2"), Ok(()));
        assert_eq!(setting_key("stereo.vr_enabled"), Ok(()));
    }

    #[test]
    fn rejects_bad_names() {
        assert_eq!(plugin_name(""), Err(NameError::Empty));
        assert_eq!(plugin_name("TSW"), Err(NameError::BadChar("TSW".into())));
        assert_eq!(plugin_name("a.b"), Err(NameError::BadChar("a.b".into())));
        assert!(matches!(
            plugin_name(&"x".repeat(65)),
            Err(NameError::TooLong(_))
        ));
    }

    #[test]
    fn splits_qualified_names() {
        assert_eq!(
            split_qualified("tsw-coop.relay.port"),
            Some(("tsw-coop", "relay.port"))
        );
        assert_eq!(split_qualified("plain"), None);
    }
}
