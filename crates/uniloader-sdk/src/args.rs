//! Splitting a command's argument text the way the console does.
//!
//! A plugin command receives the raw text after its name, quotes included, so a plugin that
//! only wants "the rest of the line" keeps it verbatim. A plugin that wants words uses
//! [`split`], which follows the console's own grammar: whitespace separates, `"..."` groups,
//! and inside quotes `\"` is a quote and `\\` a backslash.

use uniloader_core::script;

/// The words of `args`. An unterminated quote keeps the text after it as one last word, so a
/// typo never loses input.
#[must_use]
pub fn split(args: &str) -> Vec<String> {
    script::words(args).unwrap_or_else(|_| {
        let closed = format!("{args}\"");
        script::words(&closed).unwrap_or_else(|_| vec![args.to_owned()])
    })
}

/// Quote one word so [`split`] (and the console) read it back unchanged.
#[must_use]
pub fn quote(word: &str) -> String {
    script::quote(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_the_console() {
        assert_eq!(split(r#"Bob "two words" 3"#), ["Bob", "two words", "3"]);
        assert_eq!(split(r#"say "unterminated"#), ["say", "unterminated"]);
        assert_eq!(split(&quote(r#"a "b""#)), [r#"a "b""#]);
    }
}
