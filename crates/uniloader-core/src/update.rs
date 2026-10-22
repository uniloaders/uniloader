//! The update check's logic: where to ask, what the answer means, whether it is newer, and
//! the cache that keeps the loader from asking more than once per interval.
//!
//! Fetching is the platform layer's job; everything here is pure so it tests on any host.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::target::{compare, version};

/// Where a plugin's latest version is published.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// A GitHub repository, `owner/name`: its latest release (not a draft or prerelease).
    GitHub(String),
    /// A URL answering with the version as plain text, or JSON with `version` or `tag_name`
    /// and optionally `url`, `download` or `html_url`.
    Url(String),
}

impl Source {
    /// The URL to fetch.
    #[must_use]
    pub fn url(&self) -> String {
        match self {
            Source::GitHub(repo) => format!("https://api.github.com/repos/{repo}/releases/latest"),
            Source::Url(url) => url.clone(),
        }
    }

    /// The cache key.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Source::GitHub(repo) => format!("github:{repo}"),
            Source::Url(url) => url.clone(),
        }
    }

    /// Read an answer: the latest version and where to download it (possibly empty).
    #[must_use]
    pub fn parse(&self, body: &str) -> Option<Release> {
        let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
        let field = |names: &[&str]| -> Option<String> {
            let json = json.as_ref()?;
            names
                .iter()
                .find_map(|n| json.get(n)?.as_str().map(str::to_owned))
        };
        let version = match (self, &json) {
            (_, Some(_)) => field(&["version", "tag_name"])?,
            (Source::Url(_), None) => body.lines().next()?.trim().to_owned(),
            (Source::GitHub(_), None) => return None,
        };
        if version.is_empty() || version.len() > 64 {
            return None;
        }
        Some(Release {
            version,
            download: field(&["url", "download", "html_url"]).unwrap_or_default(),
        })
    }
}

/// What an update source answered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The latest version, as published (a tag may carry a leading `v`).
    pub version: String,
    /// Where to download it, or empty.
    pub download: String,
}

/// Whether `latest` is a newer version than `current`, semantic versioning style: numbers
/// compare part by part, and on equal numbers a prerelease (`1.2.0-beta`) is older than the
/// release. `false` when either does not start with a number.
#[must_use]
pub fn newer(latest: &str, current: &str) -> bool {
    let (Some(l), Some(c)) = (version(latest), version(current)) else {
        return false;
    };
    match compare(&l, &c) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => is_prerelease(current) && !is_prerelease(latest),
    }
}

fn is_prerelease(text: &str) -> bool {
    let text = text.trim().trim_start_matches(['v', 'V']);
    // The suffix after the numbers, before any build metadata.
    let suffix = text.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    let suffix = suffix.split('+').next().unwrap_or_default();
    !suffix.is_empty()
}

/// One cached answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix seconds of the last request.
    pub checked: u64,
    /// The answer, when there was a usable one.
    #[serde(default)]
    pub release: Option<Release>,
    /// The `ETag` of that answer, for a conditional request next time.
    #[serde(default)]
    pub etag: String,
}

/// Every source's last answer, by [`Source::key`]. Stored as TOML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cache {
    /// The entries.
    #[serde(default)]
    pub sources: BTreeMap<String, Entry>,
}

impl Cache {
    /// Parse the cache file; anything unreadable starts empty.
    #[must_use]
    pub fn parse(text: &str) -> Cache {
        toml::from_str(text).unwrap_or_default()
    }

    /// The file's text.
    #[must_use]
    pub fn render(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Whether `source` should be asked again at `now`: never asked, or longer ago than
    /// `interval` seconds. A clock that went backwards counts as due.
    #[must_use]
    pub fn due(&self, source: &Source, now: u64, interval: u64) -> bool {
        self.sources
            .get(&source.key())
            .is_none_or(|e| now < e.checked || now - e.checked >= interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_like_semver() {
        assert!(newer("v1.2.0", "1.1.9"));
        assert!(newer("1.10", "1.9.3"));
        assert!(!newer("1.2.0", "1.2"));
        assert!(newer("1.2.0", "1.2.0-dev"));
        assert!(!newer("1.2.0-rc1", "1.2.0"));
        assert!(!newer("1.2.0+build5", "1.2.0"));
        assert!(!newer("nightly", "1.0"));
        assert!(!newer("0.9", "1.0"));
    }

    #[test]
    fn answers_parse() {
        let gh = Source::GitHub("tsw-plugins/tsw-jump".into());
        assert_eq!(
            gh.url(),
            "https://api.github.com/repos/tsw-plugins/tsw-jump/releases/latest"
        );
        let body = r#"{"tag_name":"v0.4.0","html_url":"https://github.com/x/y/releases/tag/v0.4.0","draft":false}"#;
        assert_eq!(
            gh.parse(body),
            Some(Release {
                version: "v0.4.0".into(),
                download: "https://github.com/x/y/releases/tag/v0.4.0".into()
            })
        );
        assert_eq!(gh.parse("Not Found"), None);
        assert_eq!(gh.parse(r#"{"message":"Not Found"}"#), None);
        let url = Source::Url("https://example.org/latest".into());
        assert_eq!(
            url.parse("1.3.0\n"),
            Some(Release {
                version: "1.3.0".into(),
                download: String::new()
            })
        );
        assert_eq!(
            url.parse(r#"{"version":"2.0","url":"https://example.org/2.0.zip"}"#)
                .map(|r| r.download),
            Some("https://example.org/2.0.zip".into())
        );
        assert_eq!(url.parse(""), None);
    }

    #[test]
    fn cache_round_trips_and_knows_when_to_ask() {
        let source = Source::GitHub("a/b".into());
        let mut cache = Cache::default();
        assert!(cache.due(&source, 1000, 3600));
        cache.sources.insert(
            source.key(),
            Entry {
                checked: 1000,
                release: Some(Release {
                    version: "1.0".into(),
                    download: String::new(),
                }),
                etag: "\"abc\"".into(),
            },
        );
        assert!(!cache.due(&source, 1000 + 3599, 3600));
        assert!(cache.due(&source, 1000 + 3600, 3600));
        assert!(cache.due(&source, 10, 3600), "clock went backwards");
        assert_eq!(Cache::parse(&cache.render()), cache);
        assert_eq!(Cache::parse("garbage ="), Cache::default());
    }
}
