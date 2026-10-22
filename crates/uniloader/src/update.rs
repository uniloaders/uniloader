//! The opt-in update check: its settings on the loader's entry, which plugins it asks about,
//! and what an answer changes. Off by default; the loader opens no connection until the user
//! turns `uniloader.update_check` on. Fetching runs on its own thread (`win::update`).

use uniloader_core::settings::{Desc, Kind};
use uniloader_core::update::{newer, Cache, Source};

use crate::described::Latest;
use crate::state::State;

/// Setting keys on the loader's entry.
pub mod keys {
    /// Bool: look for newer versions.
    pub const ENABLED: &str = "update_check";
    /// Int: hours between two requests to the same source.
    pub const INTERVAL: &str = "update_interval";
}

/// Hours between requests to one source unless the user sets otherwise. GitHub allows 60
/// unauthenticated requests per hour per address; a day keeps far away from that.
pub const DEFAULT_INTERVAL_HOURS: u64 = 24;

/// The update check's settings.
#[must_use]
pub fn settings() -> Vec<Desc> {
    let desc = |key: &str, title: &str, kind, default: &str, description: &str| Desc {
        key: key.to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        kind,
        default: default.to_owned(),
        min: 0.0,
        max: 0.0,
        choices: Vec::new(),
        restart_required: false,
        advanced: false,
        transient: false,
        secret: false,
    };
    let mut interval = desc(
        keys::INTERVAL,
        "Update check interval",
        Kind::Int,
        &DEFAULT_INTERVAL_HOURS.to_string(),
        "Hours between two checks of the same plugin. Answers are cached in between.",
    );
    (interval.min, interval.max) = (1.0, 720.0);
    interval.advanced = true;
    vec![
        desc(
            keys::ENABLED,
            "Check for updates",
            Kind::Bool,
            "false",
            "Ask each plugin's update source (GitHub releases or its own URL) whether a newer \
             version exists, at most once per interval, in the background. Off: the loader \
             never connects anywhere for this.",
        ),
        interval,
    ]
}

/// What the check needs from the state: whether it is on, the interval in seconds, and each
/// plugin's (name, version, source).
#[must_use]
pub fn plan(state: &State) -> (bool, u64, Vec<(String, String, Source)>) {
    let get = |key: &str| {
        state
            .get_setting(None, &format!("{}.{key}", crate::speed::NAME))
            .unwrap_or_default()
    };
    let enabled = get(keys::ENABLED) == "true";
    let hours = get(keys::INTERVAL)
        .parse::<u64>()
        .unwrap_or(DEFAULT_INTERVAL_HOURS)
        .max(1);
    let sources = state
        .plugins
        .iter()
        .filter_map(|p| {
            let source = if !p.info.update_github.is_empty() {
                Source::GitHub(p.info.update_github.clone())
            } else if !p.info.update_url.is_empty() {
                Source::Url(p.info.update_url.clone())
            } else {
                return None;
            };
            Some((p.name().to_owned(), p.info.version.clone(), source))
        })
        .collect();
    (enabled, hours * 3600, sources)
}

/// Record what the cache knows about each plugin's source in the state. Returns the event
/// payloads for plugins with a newer version: name, version, latest, download, NUL separated.
pub fn apply(
    state: &mut State,
    plugins: &[(String, String, Source)],
    cache: &Cache,
) -> Vec<Vec<u8>> {
    let mut events = Vec::new();
    for (name, current, source) in plugins {
        let Some(release) = cache
            .sources
            .get(&source.key())
            .and_then(|e| e.release.as_ref())
        else {
            continue;
        };
        let is_newer = newer(&release.version, current);
        let Some(record) = state.plugins.iter_mut().find(|p| p.name() == name) else {
            continue;
        };
        let download = if release.download.is_empty() {
            record.info.links.download.clone()
        } else {
            release.download.clone()
        };
        let latest = Latest {
            version: release.version.clone(),
            download,
            newer: is_newer,
        };
        if is_newer && record.latest.as_ref() != Some(&latest) {
            log::info!(
                "[{name}] update available: {} (running {current}){}",
                latest.version,
                if latest.download.is_empty() {
                    String::new()
                } else {
                    format!(", {}", latest.download)
                }
            );
            events.push(
                [name.as_str(), current, &latest.version, &latest.download]
                    .join("\0")
                    .into_bytes(),
            );
        }
        record.latest = Some(latest);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{started, state};
    use uniloader_core::update::{Entry, Release};

    #[test]
    fn plan_and_apply() {
        let mut s = state();
        let h = started(&mut s, "jump");
        let _ = started(&mut s, "plain");
        if let Some(p) = s.plugins.iter_mut().find(|p| p.name() == "jump") {
            p.info.update_github = "tsw-plugins/tsw-jump".into();
            p.info.links.download = "https://example.org/dl".into();
        }
        let (enabled, interval, sources) = plan(&s);
        assert!(
            !enabled,
            "off by default... when the builtin is not even registered"
        );
        assert_eq!(interval, DEFAULT_INTERVAL_HOURS * 3600);
        assert_eq!(sources.len(), 1);
        let mut cache = Cache::default();
        assert_eq!(apply(&mut s, &sources, &cache), Vec::<Vec<u8>>::new());
        cache.sources.insert(
            sources[0].2.key(),
            Entry {
                checked: 1,
                release: Some(Release {
                    version: "v2.0.0".into(),
                    download: String::new(),
                }),
                etag: String::new(),
            },
        );
        let events = apply(&mut s, &sources, &cache);
        assert_eq!(
            events,
            vec![b"jump\x001\x00v2.0.0\x00https://example.org/dl".to_vec()]
        );
        let latest = s.plugin(h).and_then(|p| p.latest.clone());
        assert_eq!(latest.map(|l| l.newer), Some(true));
        assert_eq!(
            apply(&mut s, &sources, &cache),
            Vec::<Vec<u8>>::new(),
            "said once"
        );
    }
}
