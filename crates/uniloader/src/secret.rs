//! Secret settings: what the console shows instead of their value, the redacted echo of a
//! command that carries one, and the Argon2id hashing of passwords the loader only verifies.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use password_hash::rand_core::OsRng;
use uniloader_api::Status;
use uniloader_core::console::{self, Line};
use uniloader_core::script;

use crate::console::Ctx;
use crate::state::State;

/// What the console prints for a secret's value.
#[must_use]
pub fn shown(value: &str) -> &'static str {
    if value.is_empty() {
        "<unset>"
    } else {
        "<set>"
    }
}

/// Whether `value` for the setting `name`, kept as a hash when `hashed`, is a password to
/// hash before it is stored: neither empty nor a hash already, and long enough. Cheap, so a
/// caller can ask under the state lock and [`hash`] (Argon2, tens of ms) without it.
///
/// # Errors
/// A password shorter than [`crate::rcon::MIN_PASSWORD`].
pub fn needs_hash(hashed: bool, name: &str, value: &str) -> Result<bool, (Status, String)> {
    if !hashed || value.is_empty() || is_hash(value) {
        return Ok(false);
    }
    let min = crate::rcon::MIN_PASSWORD;
    if value.chars().count() < min {
        let message = format!("{name}: use at least {min} characters");
        return Err((Status::INVALID_ARGUMENT, message));
    }
    Ok(true)
}

/// Hash a password for storage. Returns the PHC string (`$argon2id$...`).
///
/// # Errors
/// The hasher refused, which only happens for absurd input lengths.
pub fn hash(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Whether `stored` is already a hash, so setting it again does not hash a hash.
#[must_use]
pub fn is_hash(stored: &str) -> bool {
    stored.starts_with("$argon2")
}

/// Whether `password` matches the stored hash. Compares in constant time.
#[must_use]
pub fn verify(stored: &str, password: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// Whether running `text` would put a secret setting's value on screen or into a file: it
/// sets one, or binds or aliases a script that does.
#[must_use]
pub fn carries_secret(state: &State, ctx: &Ctx, text: &str) -> bool {
    let is_secret = |name: &str| {
        state
            .setting_desc(ctx.caller, name)
            .is_some_and(|d| d.secret)
    };
    let Ok(commands) = script::commands(text) else {
        return false;
    };
    commands
        .iter()
        .any(|command| match console::parse(command) {
            Ok(Line::Set(name, _) | Line::Variable(name, Some(_))) => is_secret(&name),
            Ok(Line::Command(name, args)) => !args.is_empty() && is_secret(&name),
            Ok(Line::Bind(_, Some(body)) | Line::Alias(_, Some(body))) => {
                carries_secret(state, ctx, &body)
            }
            _ => false,
        })
}

/// The command as the console echoes and logs it: as typed, or with everything after the
/// setting name (or the bound key) replaced when it carries a secret.
#[must_use]
pub fn redact(state: &State, ctx: &Ctx, command: &str) -> String {
    let command = command.trim();
    if !carries_secret(state, ctx, command) {
        return command.to_owned();
    }
    let words = script::words(command).unwrap_or_default();
    let keep = match words.first().map(String::as_str) {
        Some(w) if w.eq_ignore_ascii_case("set") || w.eq_ignore_ascii_case("bind") => 2,
        Some(w) if w.eq_ignore_ascii_case("alias") => 2,
        _ => 1,
    };
    let mut shown: Vec<String> = words.into_iter().take(keep).collect();
    shown.push("<redacted>".to_owned());
    shown.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_verify_and_are_salted() {
        let a = hash("correct horse").unwrap_or_else(|e| panic!("{e}"));
        let b = hash("correct horse").unwrap_or_else(|e| panic!("{e}"));
        assert!(is_hash(&a));
        assert_ne!(a, b, "salted");
        assert!(verify(&a, "correct horse"));
        assert!(!verify(&a, "wrong"));
        assert!(!verify("not a hash", "x"));
        assert_eq!(shown(""), "<unset>");
        assert_eq!(shown(&a), "<set>");
    }
}
