//! What a plugin says about itself (its describe export) and what the loader found out
//! about it since (the update check).

/// What a plugin's describe export said about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Described {
    /// Unique name.
    pub name: String,
    /// Human readable name.
    pub title: String,
    /// Version text.
    pub version: String,
    /// One-line description.
    pub description: String,
    /// Authors: name and URL (or empty).
    pub authors: Vec<(String, String)>,
    /// Home page, issue tracker and download page, each possibly empty.
    pub links: Links,
    /// GitHub `owner/name` for the update check, or empty.
    pub update_github: String,
    /// URL answering with the latest version, or empty.
    pub update_url: String,
    /// Which game the plugin is meant for; empty for any.
    pub targets: Vec<uniloader_core::target::Filter>,
    /// Refuse to start it when the targets do not match, instead of warning.
    pub refuse_mismatch: bool,
}

/// Where to read about, report problems with, and download a plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    /// Home page.
    pub homepage: String,
    /// Issue tracker.
    pub issues: String,
    /// Latest release download.
    pub download: String,
}

/// A plugin's latest release, as the update check found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Latest {
    /// Its version.
    pub version: String,
    /// Where to download it, or empty.
    pub download: String,
    /// Whether it is newer than the running one.
    pub newer: bool,
}
