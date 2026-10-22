//! Metadata declarations for [`Plugin::AUTHORS`](crate::Plugin::AUTHORS) and
//! [`Plugin::TARGETS`](crate::Plugin::TARGETS).
//!
//! Everything here is `const`, like the dependency list, so the loader reads it from the
//! describe export before the plugin runs any code of its own.

/// One author of a plugin.
///
/// ```
/// use uniloader_sdk::Author;
///
/// const AUTHORS: &[Author] = &[
///     Author::new("Bluscream").url("https://github.com/Bluscream"),
///     Author::new("A Contributor"),
/// ];
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Author {
    /// Name as shown.
    pub name: &'static str,
    /// A profile or home page, or empty.
    pub link: &'static str,
}

impl Author {
    /// An author without a link.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Author { name, link: "" }
    }

    /// With a profile or home page.
    #[must_use]
    pub const fn url(self, link: &'static str) -> Self {
        Author { link, ..self }
    }
}

/// One target filter: the running game's property must have one of the values. Every filter
/// a plugin declares must match; property names and value syntax are listed in the loader's
/// README (`exe.name`, `pe.version.CompanyName`, `pe.fixed.file_version` with `>=4.26 <5`,
/// ...).
///
/// ```
/// use uniloader_sdk::Target;
///
/// const TARGETS: &[Target] = &[
///     Target::new("exe.name", &["TrainSimWorld.exe"]),
///     Target::new("pe.version.CompanyName", &["Dovetail Games"]),
/// ];
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// Property name.
    pub property: &'static str,
    /// Accepted values; any one matches.
    pub values: &'static [&'static str],
}

impl Target {
    /// A filter on `property`.
    #[must_use]
    pub const fn new(property: &'static str, values: &'static [&'static str]) -> Self {
        Target { property, values }
    }
}
