//! The update check's fetching: one background thread per check, HTTPS through
//! [`super::http`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use uniloader_api::{topics, PluginHandle};
use uniloader_core::update::{Cache, Entry, Source};

use super::dispatch::deliver_event;
use super::state;

/// A check is running; a second request while it does is dropped.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Biggest answer read. A release's JSON with a long changelog is tens of kilobytes.
const MAX_BODY: usize = 1 << 20;

/// Run a check in the background when the setting is on. Never blocks the caller.
pub(super) fn start() {
    if !crate::update::plan(&state()).0 || RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("uniloader-update".to_owned())
        .spawn(|| {
            check();
            RUNNING.store(false, Ordering::Release);
        });
    if let Err(e) = spawned {
        log::warn!("update check: no thread: {e}");
        RUNNING.store(false, Ordering::Release);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn check() {
    let ((enabled, interval, plugins), file) = {
        let guard = state();
        (crate::update::plan(&guard), guard.paths.update_cache())
    };
    if !enabled {
        return;
    }
    let mut cache = Cache::parse(&std::fs::read_to_string(&file).unwrap_or_default());
    let mut sources: Vec<&Source> = plugins.iter().map(|(_, _, s)| s).collect();
    sources.sort();
    sources.dedup();
    let mut asked = false;
    for source in sources {
        if !cache.due(source, now(), interval) {
            continue;
        }
        asked = true;
        let key = source.key();
        let previous = cache.sources.get(&key).cloned().unwrap_or_default();
        let entry = match get(&source.url(), &previous.etag) {
            Ok(Answer::NotModified) => Entry {
                checked: now(),
                ..previous
            },
            Ok(Answer::Body { body, etag }) => {
                if let Some(release) = source.parse(&body) {
                    Entry {
                        checked: now(),
                        release: Some(release),
                        etag,
                    }
                } else {
                    log::info!("update check: {} gave no version", source.url());
                    Entry {
                        checked: now(),
                        ..previous
                    }
                }
            }
            Err(why) => {
                // Not cached as checked: the next start tries again.
                log::info!("update check: {}: {why}", source.url());
                continue;
            }
        };
        cache.sources.insert(key, entry);
    }
    if asked {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&file, cache.render()) {
            log::warn!("update check: cannot write {}: {e}", file.display());
        }
    }
    let (targets, events) = {
        let mut guard = state();
        let events = crate::update::apply(&mut guard, &plugins, &cache);
        (
            guard.subscribers(PluginHandle::NONE, topics::UPDATE_AVAILABLE),
            events,
        )
    };
    for payload in events {
        deliver_event(
            PluginHandle::NONE,
            &targets,
            topics::UPDATE_AVAILABLE,
            &payload,
        );
    }
}

enum Answer {
    NotModified,
    Body { body: String, etag: String },
}

/// Milliseconds for each of resolve, connect, send and receive.
const TIMEOUT_MS: i32 = 10_000;

/// GET `url` over HTTPS, conditional on `etag` when there is one.
fn get(url: &str, etag: &str) -> Result<Answer, String> {
    let mut headers = String::from(
        "Accept: application/vnd.github+json, application/json, text/plain\r\n\
         X-GitHub-Api-Version: 2022-11-28\r\n",
    );
    if !etag.is_empty() {
        headers.push_str("If-None-Match: ");
        headers.push_str(etag);
        headers.push_str("\r\n");
    }
    let answer = super::http::request("GET", url, &headers, &[], (TIMEOUT_MS, MAX_BODY))?;
    match answer.status {
        304 => Ok(Answer::NotModified),
        200 => Ok(Answer::Body {
            body: String::from_utf8_lossy(&answer.body).into_owned(),
            etag: answer.etag,
        }),
        other => Err(format!("HTTP {other}")),
    }
}
