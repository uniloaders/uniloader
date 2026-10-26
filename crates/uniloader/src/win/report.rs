//! Crash, freeze and unexpected-exit reports on Windows: the incidents of a session, the zip
//! the next start makes of them, and what the player decides (send it, or save it to the
//! Desktop). What goes into a report and how it is scrubbed: [`crate::reports`]; the dumps
//! and the watchdog: [`super::crash`].
//!
//! - At start: the previous session's `running.txt`, if it did not close normally and left
//!   no incident of its own, becomes an `exit` incident. [`SETTLE`] later (plugins have
//!   attached theirs by then: `report attach <folder|file>`, a game's own crash reports, a
//!   plugin's log)
//!   every incident is bundled, and the newest report and every one the player left for
//!   later (`report later <zip>`, the overlay's prompt closed undecided) are offered with
//!   [`topics::REPORT_READY`] (the overlay asks the player); older ones never answered go to
//!   the Desktop. An `exit` incident that got attached files is a crash the game reported
//!   itself.
//! - Nobody listening, or the setting says Desktop: the report is saved to the Desktop.
//! - `report send <zip>` POSTs it to the URL a plugin set (`report url <https url>`; the
//!   loader has none of its own), `report desktop <zip>` copies it to the Desktop; both end
//!   in [`topics::REPORT_DONE`]. A failed upload saves it to the Desktop instead.
//! - `report loading on|off`: a plugin that knows the game is loading keeps the freeze
//!   watchdog quiet meanwhile ([`crash::LOADING_CAP`] at most).

// FFI module: the Desktop folder through shell32, the Windows version from the shared page.
#![allow(unsafe_code)]

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use uniloader_api::{topics, PluginHandle, Status};
use windows::core::{s, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

use super::crash::{self, Seen};
use super::dispatch::deliver_event;
use crate::config::Paths;
use crate::reports::{self, Kind, Marker};

/// Plugins get this long to attach their folders before the previous session is bundled.
const SETTLE: Duration = Duration::from_secs(20);
/// Lines of the previous session's log quoted in an `exit` incident.
const LAST_LINES: usize = 15;
/// Milliseconds for each step of an upload.
const UPLOAD_TIMEOUT_MS: i32 = 60_000;
/// Biggest report sent; a bigger one is only saved.
const MAX_UPLOAD: u64 = 32 << 20;
/// Biggest answer to an upload read.
const MAX_ANSWER: usize = 64 << 10;

/// This session.
struct Session {
    /// `<root>/reports`.
    dir: PathBuf,
    /// `<root>/logs`.
    logs: PathBuf,
    /// When it started, Unix seconds.
    started: u64,
    since: Instant,
    /// The previous session's marker.
    previous: Option<Marker>,
    /// The game's executable name without `.exe`.
    game: String,
}

static SESSION: OnceLock<Session> = OnceLock::new();
/// Where `report send` posts to; empty: nowhere.
static URL: Mutex<String> = Mutex::new(String::new());
/// Folders and files plugins attached.
static ATTACHED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// The marker's path (NUL terminated UTF-16) and its closing text, made at start so that
/// [`ending`] neither allocates nor locks.
static CLOSING: OnceLock<(Vec<u16>, Vec<u8>)> = OnceLock::new();
/// The freeze incident being written.
static FREEZE: Mutex<Option<PathBuf>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now() -> u64 {
    reports::unix(SystemTime::now())
}

/// The report setting's value.
fn setting() -> String {
    super::state()
        .get_setting(
            None,
            &format!("{}.{}", crate::speed::NAME, reports::SETTING),
        )
        .unwrap_or_else(|_| reports::ASK.to_owned())
}

/// Starts reporting for this session: the marker, the crash filter, the watchdog, and the
/// previous session's report. Call after the loader's entry is registered (the setting).
pub(super) fn start(paths: &Paths) {
    if setting() == reports::OFF {
        log::info!("reports: off");
        return;
    }
    let dir = paths.reports_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        log::warn!("reports: cannot create {}: {e}", dir.display());
        return;
    }
    let marker = dir.join(reports::MARKER);
    let previous = fs::read_to_string(&marker)
        .ok()
        .and_then(|t| Marker::parse(&t));
    super::suspects::note_own();
    super::suspects::start(&dir, previous.is_none_or(|m| m.closing));
    let started = now();
    let mine = Marker {
        pid: std::process::id(),
        started,
        closing: false,
    };
    if let Err(e) = fs::write(&marker, mine.render()) {
        log::warn!("reports: cannot write {}: {e}", marker.display());
    }
    // The product name tells editions apart that share an exe name (TrainSimWorld7 and
    // TrainSimWorld5 both run TrainSimWorld.exe).
    let product = super::try_state()
        .and_then(|g| {
            g.facts
                .get("pe.version.ProductName")
                .map(ToString::to_string)
        })
        // It goes into a header and a file name.
        .map(|p| {
            p.chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                .collect::<String>()
        })
        .filter(|p| !p.is_empty());
    let game = product.unwrap_or_else(|| {
        super::proxy::module_path(None)
            .file_stem()
            .map_or_else(|| "game".to_owned(), |s| s.to_string_lossy().into_owned())
    });
    let session = Session {
        dir: dir.clone(),
        logs: paths.logs_dir.clone(),
        started,
        since: Instant::now(),
        previous,
        game,
    };
    if SESSION.set(session).is_err() {
        return;
    }
    let closing = Marker {
        closing: true,
        ..mine
    };
    let wide: Vec<u16> = marker.as_os_str().encode_wide().chain([0]).collect();
    let _ = CLOSING.set((wide, closing.render().into_bytes()));
    crash::start(&dir);
    crash::watch(seen);
    let incidents_dir = dir.clone();
    let spawned = std::thread::Builder::new()
        .name("uniloader-reports".to_owned())
        .spawn(move || {
            let dir = incidents_dir;
            std::thread::sleep(SETTLE);
            if super::dumper::ready() {
                log::info!(
                    "reports: the dump helper runs; dumps are written from outside the game"
                );
            }
            // Overlays and recorders inject their hooks by now; a crash in a hook of theirs
            // looks like ours.
            let modules = super::crashinfo::modules();
            if modules.is_empty() {
                log::warn!("reports: cannot list the game's modules");
            }
            let others = super::crashinfo::other_programs(&modules);
            if !others.is_empty() {
                log::info!(
                    "modules of other programs in the game: {}",
                    others.join(", ")
                );
            }
            // After the settle time, so the incident's plugin list is this start's whole one
            // (the same plugins, as a rule, as the session that ended).
            if let Some(previous) = previous.filter(|m| !m.closing) {
                previous_ended(&dir, previous);
            }
            bundle_all();
            offer_pending();
        });
    if let Err(e) = spawned {
        log::warn!("reports: cannot bundle the previous session's report: {e}");
    }
    log::info!(
        "reports: on (crash dumps, freeze watchdog), in {}",
        dir.display()
    );
}

/// The incident folders in `dir`, oldest first.
fn incidents(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(name, _)| reports::kind_of(name).is_some())
        .collect();
    found.sort();
    found
}

/// The previous session ended without closing: an `exit` incident, unless it left a crash or
/// freeze incident of its own.
fn previous_ended(dir: &Path, previous: Marker) {
    let since = reports::stamp(previous.started);
    if incidents(dir).iter().any(|(name, _)| *name >= since) {
        return;
    }
    let incident = dir.join(reports::incident_name(now(), Kind::Exit));
    if fs::create_dir_all(&incident).is_err() {
        return;
    }
    let last = SESSION
        .get()
        .and_then(|s| fs::read_to_string(s.logs.join(crate::logging::PREV_LOG_FILE)).ok())
        .map(|log| reports::last_lines(&log, LAST_LINES))
        .filter(|l| !l.is_empty())
        .map_or_else(String::new, |l| {
            format!("\nThe last lines of its log:\n{l}\n")
        });
    let text = format!(
        "The session of process {} (started {} UTC) ended without closing normally. No crash \
         dump of the loader's own; the game's own crash report is attached when a plugin \
         found one.\n{last}",
        previous.pid, since
    );
    let _ = fs::write(incident.join(reports::INFO), info(Kind::Exit, &text));
    log::warn!("reports: the previous session ended without closing normally");
}

/// The game caught a crash itself (UE does on its main thread, writes its own report and
/// ends the process, which may look like a normal end): a `crash` incident when an attached
/// folder got a dump during the previous session and nothing else reported that session.
fn game_crashed(session: &Session, previous: Marker) {
    let since = reports::stamp(previous.started);
    if incidents(&session.dir)
        .iter()
        .any(|(name, _)| *name >= since)
    {
        return;
    }
    let dumped = lock(&ATTACHED).iter().any(|folder| {
        reports::parts_of(folder, "", Some(previous.started))
            .iter()
            .any(|p| reports::content_of(&p.name) == Some(reports::Content::Dump))
    });
    if !dumped {
        return;
    }
    let incident = session.dir.join(reports::incident_name(now(), Kind::Crash));
    if fs::create_dir_all(&incident).is_err() {
        return;
    }
    let text = format!(
        "The game wrote a crash report of its own during the session of process {} (started \
         {since} UTC); it is attached.\n",
        previous.pid
    );
    let _ = fs::write(incident.join(reports::INFO), info(Kind::Crash, &text));
    log::warn!("reports: the game reported a crash in the previous session");
}

/// The process ends through `ExitProcess` or the loader is unloaded: a normal end. Runs
/// in `DllMain`, where other threads may have died holding the heap's lock: nothing here
/// allocates.
pub(super) fn ending() {
    if let Some((path, text)) = CLOSING.get() {
        crash::write_raw(path, text);
    }
}

fn write_marker(session: &Session, closing: bool) {
    let marker = Marker {
        pid: std::process::id(),
        started: session.started,
        closing,
    };
    let _ = fs::write(session.dir.join(reports::MARKER), marker.render());
}

/// The incident's `info.txt`: what happened, then the loader, the game, Windows and the
/// plugins.
fn info(kind: Kind, what: &str) -> String {
    format!(
        "{} ({})\n{}\n\n{}",
        kind.headline(),
        kind.key(),
        what.trim_end(),
        details()
    )
}

/// The loader, Windows, the game and the plugins, for an incident's `info.txt`.
fn details() -> String {
    let mut lines = vec![
        format!("uniloader {}", env!("CARGO_PKG_VERSION")),
        windows_version(),
    ];
    if let Some(session) = SESSION.get() {
        lines.push(format!(
            "written by the session started {} UTC, {} s in",
            reports::stamp(session.started),
            session.since.elapsed().as_secs()
        ));
    }
    match super::try_state() {
        Some(guard) => {
            for name in [
                "exe.name",
                "pe.version.ProductName",
                "pe.version.ProductVersion",
                "pe.version.FileVersion",
                "unreal.version",
                "steam.build_id",
            ] {
                if let Some(value) = guard.facts.get(name) {
                    lines.push(format!("{name} = {value}"));
                }
            }
            lines.push("plugins:".to_owned());
            for p in &guard.plugins {
                lines.push(format!(
                    "  {} {} {}",
                    p.info.name,
                    p.info.version,
                    if p.enabled { "running" } else { "off" }
                ));
            }
        }
        None => lines.push("(the loader's state was busy: no plugin list)".to_owned()),
    }
    lines.join("\n") + "\n"
}

/// `windows 10.0 build 19045`, from the shared user page every process has.
fn windows_version() -> String {
    // KUSER_SHARED_DATA at 0x7FFE0000: NtBuildNumber (+0x260), NtMajorVersion (+0x26C),
    // NtMinorVersion (+0x270); mapped read-only into every process.
    let read = |offset: usize| {
        // SAFETY: a documented, always mapped read-only page; aligned u32 reads.
        unsafe { core::ptr::read_volatile((0x7FFE_0000usize + offset) as *const u32) }
    };
    format!(
        "windows {}.{} build {}",
        read(0x26C),
        read(0x270),
        read(0x260) & 0xFFFF
    )
}

/// What the watchdog saw.
fn seen(what: &Seen) {
    let Some(session) = SESSION.get() else { return };
    match what {
        Seen::Frozen { n, silent } => {
            let mut freeze = lock(&FREEZE);
            let dir = freeze.get_or_insert_with(|| {
                session
                    .dir
                    .join(reports::incident_name(now(), Kind::Freeze))
            });
            if fs::create_dir_all(&*dir).is_err() {
                return;
            }
            let (n, silent) = (*n, *silent);
            let written = crash::write_dump(&dir.join(format!("freeze-{n}.dmp")), None);
            let text = format!(
                "The game's window stopped answering; dump {n} after {} s (written: {written}).",
                silent.as_secs()
            );
            let _ = fs::write(dir.join(reports::INFO), info(Kind::Freeze, &text));
            log::warn!(
                "reports: the game has not answered for {} s; freeze dump {n}",
                silent.as_secs()
            );
        }
        Seen::Recovered(silent) => {
            let silent = silent.as_secs();
            if let Some(dir) = lock(&FREEZE).take() {
                let _ = fs::remove_dir_all(dir);
                log::info!(
                    "reports: the game answers again after {silent} s; the freeze dumps are \
                     discarded"
                );
            } else {
                log::info!("reports: the game's window was silent for {silent} s");
            }
        }
        Seen::Closing => {
            write_marker(session, true);
            log::info!("reports: the game's main window closed; the session ends normally");
        }
        Seen::Reopened => {
            write_marker(session, false);
            log::info!("reports: a new main window (that was a start-up window); still running");
        }
    }
}

/// What faulted, for [`crashed`].
pub(super) struct Crash<'a> {
    pub(super) code: u32,
    pub(super) address: usize,
    /// The address as `module+offset`.
    pub(super) place: String,
    /// The first loaded module on the stack ([`super::suspects`]).
    pub(super) suspect: Option<&'a str>,
    /// Registers, stack and modules in words ([`super::crashinfo`]).
    pub(super) words: &'a str,
}

/// From the exception filter: a crash incident with what faulted and the dump `dump`
/// writes. Takes no lock the crashing thread might hold.
pub(super) fn crashed(dir: &Path, crash: &Crash<'_>, dump: impl FnOnce(&Path) -> bool) {
    super::suspects::record(dir, crash.suspect);
    let incident = dir.join(reports::incident_name(now(), Kind::Crash));
    if fs::create_dir_all(&incident).is_err() {
        return;
    }
    let what = if crash.code == crash::STACK_OVERFLOW {
        " (stack overflow)"
    } else {
        ""
    };
    let stack = crash
        .suspect
        .map_or_else(String::new, |s| format!("{s} is on the crashing stack\n"));
    let text = format!(
        "{}\nexception {:#010x}{what} at {:#x} ({})\n{stack}",
        Kind::Crash.headline(),
        crash.code,
        crash.address,
        crash.place
    );
    // What faulted goes to disk before the dump: a process that dies while dbghelp writes
    // (a stack overflow's dumper thread runs out of time) kept only an empty crash.dmp and
    // no reason (a friend's Windows game, 2026-10-25).
    let info = incident.join(reports::INFO);
    let _ = fs::write(&info, format!("{text}\n{}", crash.words));
    let written = dump(&incident.join("crash.dmp"));
    let _ = fs::write(
        info,
        format!("{text}dump written: {written}\n\n{}", crash.words),
    );
}

/// Bundles every incident folder into its zip, then keeps the newest [`reports::KEEP`].
fn bundle_all() {
    let Some(session) = SESSION.get() else { return };
    let since = session.previous.map(|m| m.started);
    if let Some(previous) = session.previous {
        game_crashed(session, previous);
    }
    let exe = super::proxy::module_path(None)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Windows' archives are read once, on the first incident that wants them.
    let mut wer: Option<Vec<String>> = None;
    for (name, dir) in incidents(&session.dir) {
        let current = lock(&FREEZE).as_deref() == Some(dir.as_path());
        if current {
            continue;
        }
        // Windows' own report of the previous session's end, for an incident of that session.
        if let Some(previous) = session
            .previous
            .filter(|_| reports::kind_of(&name) != Some(Kind::Manual))
        {
            if name >= reports::stamp(previous.started) {
                let found = wer.get_or_insert_with(|| super::wer::find(&exe, previous.started));
                let found = super::wer::write(&dir, found);
                if found > 0 {
                    log::info!("reports: {found} Windows error report(s) added to {name}");
                }
            }
        }
        bundle(session, &name, &dir, since);
    }
    reports::prune(&session.dir);
}

/// Removes the empty dumps in `dir` (the game ended while one was written); a line for
/// `info.txt` about each.
fn drop_empty_dumps(dir: &Path) -> String {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let empty = entry.metadata().is_ok_and(|m| m.is_file() && m.len() == 0);
            if !empty || reports::content_of(&name) != Some(reports::Content::Dump) {
                return None;
            }
            let _ = fs::remove_file(entry.path());
            Some(format!(
                "{name} was empty (the game ended while it was written); left out\n"
            ))
        })
        .collect()
}

/// The zip of one incident; its name.
fn bundle(session: &Session, name: &str, dir: &Path, since: Option<u64>) -> Option<String> {
    let empty = drop_empty_dumps(dir);
    if !empty.is_empty() {
        let info = dir.join(reports::INFO);
        if let Ok(text) = fs::read_to_string(&info) {
            let (head, rest) = text.split_once("\n\n").unwrap_or((&text, ""));
            let _ = fs::write(&info, format!("{head}\n{empty}\n{rest}"));
        }
    }
    let mut parts = reports::parts_of(dir, "report", None);
    parts.extend(reports::parts_of(&session.logs, "logs", None));
    let mut attached = Vec::new();
    let paths = lock(&ATTACHED).clone();
    for (path, label) in paths.iter().zip(reports::attached_labels(&paths)) {
        attached.extend(reports::parts_of(path, &format!("attached/{label}"), since));
    }
    let mut name = name.to_owned();
    let dumped = attached
        .iter()
        .any(|p| reports::content_of(&p.name) == Some(reports::Content::Dump));
    if reports::kind_of(&name) == Some(Kind::Exit) && dumped {
        // the game wrote a crash dump of its own (attached logs alone prove nothing)
        name = name.replacen("_exit", "_crash", 1);
        let info = dir.join(reports::INFO);
        if let Ok(text) = fs::read_to_string(&info) {
            let rest = text.split_once('\n').map_or("", |(_, rest)| rest);
            let _ = fs::write(
                &info,
                format!(
                    "{} ({})\nThe game wrote a crash report of its own; it is attached.\n{rest}",
                    Kind::Crash.headline(),
                    Kind::Crash.key()
                ),
            );
        }
    }
    // A crash incident's info.txt is written inside the exception filter, where nothing may
    // lock: the plugin list and the game's facts are added now.
    let info = dir.join(reports::INFO);
    if let Ok(text) = fs::read_to_string(&info) {
        if !text.contains("\nplugins:") {
            let _ = fs::write(&info, format!("{}\n\n{}", text.trim_end(), details()));
        }
    }
    parts.extend(attached);
    let zip = session.dir.join(format!("{name}.zip"));
    match reports::bundle(&zip, &parts) {
        Ok(size) => {
            let _ = fs::remove_dir_all(dir);
            log::info!(
                "reports: {name}.zip ({} KiB, {} files)",
                size / 1024,
                parts.len()
            );
            Some(format!("{name}.zip"))
        }
        Err(e) => {
            log::warn!("reports: cannot write {}: {e}", zip.display());
            None
        }
    }
}

/// Offers the undecided reports as [`reports::plan_offers`] says: the newest and every one
/// the player left for later, oldest first (the prompts come one after another); older ones
/// never answered go to the Desktop instead of being asked about again.
fn offer_pending() {
    let Some(session) = SESSION.get() else { return };
    let pending = reports::pending(&session.dir);
    let (offers, saves) =
        reports::plan_offers(&pending, |name| reports::is_later(&session.dir, name));
    for name in &saves {
        if save_to_desktop(session, name).is_none() {
            reports::mark_done(&session.dir, name, "skipped");
        }
    }
    let save = setting() == reports::DESKTOP;
    for name in &offers {
        offer(session, name, save);
    }
}

/// Tells the plugins the report `name` is ready; saved to the Desktop at once when `save` or
/// when nobody listens.
fn offer(session: &Session, name: &str, save: bool) {
    let kind = reports::kind_of(name).unwrap_or(Kind::Exit);
    let host = super::http::split_url(&lock(&URL))
        .map(|(h, _, _)| h)
        .unwrap_or_default();
    let listening = !super::state()
        .subscribers(PluginHandle::NONE, topics::REPORT_READY)
        .is_empty();
    let saved = if save || !listening {
        save_to_desktop(session, name)
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let host = if save { String::new() } else { host };
    deliver(
        topics::REPORT_READY,
        &[name, kind.key(), kind.headline(), &host, &saved],
    );
}

fn deliver(topic: &str, fields: &[&str]) {
    let targets = super::state().subscribers(PluginHandle::NONE, topic);
    deliver_event(
        PluginHandle::NONE,
        &targets,
        topic,
        fields.join("\0").as_bytes(),
    );
}

/// The Desktop folder (`FOLDERID_Desktop`, which follows a moved or synced Desktop), else
/// `%USERPROFILE%\Desktop`.
fn desktop() -> Option<PathBuf> {
    type KnownFolderFn = unsafe extern "system" fn(*const GUID, u32, HANDLE, *mut PWSTR) -> HRESULT;
    const DESKTOP: GUID = GUID::from_u128(0xb4bf_cc3a_db2c_424c_b029_7fe9_9a87_c641);
    let name: Vec<u16> = "shell32.dll\0".encode_utf16().collect();
    // SAFETY: NUL terminated name; system directory only.
    let found =
        unsafe { LoadLibraryExW(PCWSTR(name.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
            .ok()
            .and_then(|module| {
                // SAFETY: the documented signature of `SHGetKnownFolderPath`.
                let f = unsafe { GetProcAddress(module, s!("SHGetKnownFolderPath")) }?;
                // SAFETY: as above.
                let f = unsafe {
                    core::mem::transmute::<unsafe extern "system" fn() -> isize, KnownFolderFn>(f)
                };
                let mut path = PWSTR::null();
                // SAFETY: `path` receives a string the shell allocates; it is read once and left
                // allocated (a few bytes, once per saved report).
                let ok = unsafe { f(&DESKTOP, 0, HANDLE::default(), &raw mut path) }.is_ok();
                if !ok || path.is_null() {
                    return None;
                }
                // SAFETY: on success `path` is a NUL terminated wide string.
                unsafe { path.to_string() }.ok().map(PathBuf::from)
            });
    found.or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Desktop")))
}

/// Copies the report `name` to the Desktop as `<game>-<kind>-report-<stamp>.zip`.
fn save_to_desktop(session: &Session, name: &str) -> Option<PathBuf> {
    let desktop = desktop()?;
    let stem = name.strip_suffix(".zip").unwrap_or(name);
    let (stamp, kind) = stem.rsplit_once('_')?;
    let target = desktop.join(format!("{}-{kind}-report-{stamp}.zip", session.game));
    match fs::copy(session.dir.join(name), &target) {
        Ok(_) => {
            reports::mark_done(&session.dir, name, &format!("saved {}", target.display()));
            log::info!("reports: {name} saved to {}", target.display());
            Some(target)
        }
        Err(e) => {
            log::warn!("reports: cannot save {name} to {}: {e}", target.display());
            None
        }
    }
}

/// Sends the report `name`; on its own thread.
fn send(session: &'static Session, name: String) {
    let url = lock(&URL).clone();
    let spawned = std::thread::Builder::new()
        .name("uniloader-report-upload".to_owned())
        .spawn(move || {
            let result = upload(session, &name, &url);
            match result {
                Ok(id) => {
                    reports::mark_done(&session.dir, &name, &format!("sent {id}"));
                    log::info!("reports: {name} sent, report id {id}");
                    deliver(topics::REPORT_DONE, &[&name, "sent", &id]);
                }
                Err(why) => {
                    log::warn!("reports: sending {name} failed: {why}");
                    let saved = save_to_desktop(session, &name).map_or_else(
                        || why.clone(),
                        |p| format!("{why}; saved to {} instead", p.display()),
                    );
                    deliver(topics::REPORT_DONE, &[&name, "failed", &saved]);
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("reports: cannot start the upload: {e}");
    }
}

/// POSTs the report to `url`; the id the server gave it.
fn upload(session: &Session, name: &str, url: &str) -> Result<String, String> {
    if url.is_empty() {
        return Err("no upload address (a plugin sets one with `report url`)".to_owned());
    }
    let path = session.dir.join(name);
    let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_UPLOAD {
        return Err(format!("{} MiB is too large to send", size >> 20));
    }
    let body = fs::read(&path).map_err(|e| e.to_string())?;
    let kind = reports::kind_of(name).unwrap_or(Kind::Exit).key();
    let headers = format!(
        "Content-Type: application/zip\r\nX-Report-Kind: {kind}\r\nX-Report-Game: {}\r\n\
         X-Report-Loader: {}\r\n",
        session.game,
        env!("CARGO_PKG_VERSION")
    );
    let answer = super::http::request(
        "POST",
        url,
        &headers,
        &body,
        (UPLOAD_TIMEOUT_MS, MAX_ANSWER),
    )?;
    let text = String::from_utf8_lossy(&answer.body);
    if !(200..300).contains(&answer.status) {
        return Err(format!("HTTP {}: {}", answer.status, text.trim()));
    }
    Ok(json_string(&text, "id").unwrap_or_else(|| "?".to_owned()))
}

/// The string value of `"key"` in a flat JSON object.
fn json_string(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("\"{key}\""))?;
    let rest = text[at + key.len() + 2..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

/// The `report` console command.
pub(super) fn command(args: &str) -> (Status, Vec<String>) {
    let Some(session) = SESSION.get() else {
        return (Status::UNSUPPORTED, vec!["reports are off".to_owned()]);
    };
    let mut words = args.split_whitespace();
    let verb = words.next().unwrap_or("list");
    let rest: Vec<&str> = words.collect();
    // One quoted word (a URL holds `//`, the console's comment) comes without its quotes.
    let rest = uniloader_core::script::value(&rest.join(" "));
    let named = || reports::checked_name(rest.trim()).map(str::to_owned);
    match verb {
        "list" => {
            let mut lines: Vec<String> = reports::zips(&session.dir)
                .into_iter()
                .map(|z| {
                    let undecided = if reports::is_later(&session.dir, &z) {
                        "left for later"
                    } else {
                        "not decided"
                    };
                    let done = fs::read_to_string(session.dir.join(format!("{z}.done")))
                        .map_or_else(|_| undecided.to_owned(), |d| d.trim().to_owned());
                    format!("{z}: {done}")
                })
                .collect();
            if lines.is_empty() {
                lines.push("no reports".to_owned());
            }
            (Status::OK, lines)
        }
        "now" => {
            let dir = session
                .dir
                .join(reports::incident_name(now(), Kind::Manual));
            if fs::create_dir_all(&dir).is_err() {
                return (Status::ERROR, vec!["cannot create the report".to_owned()]);
            }
            let written = crash::write_dump(&dir.join("session.dmp"), None);
            let text = format!("Asked for from the console; dump written: {written}.");
            let _ = fs::write(dir.join(reports::INFO), info(Kind::Manual, &text));
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match bundle(session, &name, &dir, Some(session.started)) {
                Some(zip) => {
                    offer(session, &zip, setting() == reports::DESKTOP);
                    (Status::OK, vec![format!("made {zip}")])
                }
                None => (Status::ERROR, vec!["cannot write the report".to_owned()]),
            }
        }
        "send" => match named() {
            Some(name) if session.dir.join(&name).is_file() => {
                send(session, name.clone());
                (Status::OK, vec![format!("sending {name}")])
            }
            _ => (Status::NOT_FOUND, vec![format!("no report {rest}")]),
        },
        "later" => match named() {
            Some(name) if session.dir.join(&name).is_file() => {
                reports::mark_later(&session.dir, &name);
                (
                    Status::OK,
                    vec![format!(
                        "report {name} left for later: offered again at the next start"
                    )],
                )
            }
            _ => (Status::NOT_FOUND, vec![format!("no report {rest}")]),
        },
        "desktop" => match named().and_then(|n| save_to_desktop(session, &n).map(|p| (n, p))) {
            Some((name, path)) => {
                let shown = path.display().to_string();
                deliver(topics::REPORT_DONE, &[&name, "saved", &shown]);
                (Status::OK, vec![format!("report {name} saved to {shown}")])
            }
            None => (Status::NOT_FOUND, vec![format!("cannot save {rest}")]),
        },
        "crash" => crash_on_purpose(rest.trim()),
        "loading" => match rest.trim() {
            "on" => {
                crash::loading(true);
                (Status::OK, vec!["loading: freezes not counted".to_owned()])
            }
            "off" => {
                crash::loading(false);
                (Status::OK, vec!["loaded: freezes counted".to_owned()])
            }
            _ => (
                Status::INVALID_ARGUMENT,
                vec![format!(
                    "report loading on|off (now {})",
                    if crash::is_loading() { "on" } else { "off" }
                )],
            ),
        },
        "url" => {
            let url = rest.trim();
            if !url.is_empty() && super::http::split_url(url).is_none() {
                return (Status::INVALID_ARGUMENT, vec!["an https:// URL".to_owned()]);
            }
            url.clone_into(&mut lock(&URL));
            (
                Status::OK,
                vec![format!(
                    "reports go to {}",
                    if url.is_empty() { "nowhere" } else { url }
                )],
            )
        }
        "attach" => {
            let path = PathBuf::from(rest.trim());
            if path.is_file() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if reports::content_of(&name).is_none() {
                    return (
                        Status::INVALID_ARGUMENT,
                        vec![format!(
                            "{rest} is not a kind that goes into reports (.log, .txt, .xml, .dmp)"
                        )],
                    );
                }
            } else if !path.is_dir() {
                return (Status::NOT_FOUND, vec![format!("no folder or file {rest}")]);
            }
            let mut attached = lock(&ATTACHED);
            if !attached.contains(&path) {
                attached.push(path);
            }
            (Status::OK, vec![format!("attached {rest}")])
        }
        other => (
            Status::INVALID_ARGUMENT,
            vec![format!(
                "unknown: report {other}; {}",
                reports::COMMAND_USAGE
            )],
        ),
    }
}

/// `report crash [fault|call|stack|panic]`: a thread of the loader's own faults, so the loader's
/// handlers (not the game's) see it, the path a crash in a plugin's thread takes. `stack`
/// overflows its stack; `panic` panics on it (logged, and only that thread ends).
fn crash_on_purpose(how: &str) -> (Status, Vec<String>) {
    #[expect(unconditional_recursion, reason = "overflowing the stack is the point")]
    fn deeper(n: u64) -> u64 {
        let frame = std::hint::black_box([n; 64]);
        deeper(frame[usize::try_from(n % 64).unwrap_or(0)].wrapping_add(1)).wrapping_add(n)
    }
    let body: fn() = match how {
        "" | "fault" => || {
            let at = std::ptr::with_exposed_provenance_mut::<u8>(0x10);
            // SAFETY: none; crashing is what this command is for.
            unsafe { at.write_volatile(0) };
        },
        "stack" => || {
            std::hint::black_box(deeper(1));
        },
        // Faults inside the C runtime's `memcpy`, a frame below the loader's own.
        "call" => || {
            let from = std::hint::black_box(vec![0u8; 4096]);
            let to = std::ptr::with_exposed_provenance_mut::<u8>(0x10);
            // SAFETY: none; crashing is what this command is for.
            unsafe {
                core::ptr::copy_nonoverlapping(from.as_ptr(), to, std::hint::black_box(4096));
            };
        },
        "panic" => || panic!("report crash panic: a panic on purpose"),
        other => {
            return (
                Status::INVALID_ARGUMENT,
                vec![format!(
                    "report crash [fault|call|stack|panic], not {other}"
                )],
            )
        }
    };
    log::warn!("reports: crashing on purpose (report crash {how})");
    match std::thread::Builder::new()
        .name("uniloader-report-crash".to_owned())
        .spawn(body)
    {
        Ok(_) => (Status::OK, vec!["crashing".to_owned()]),
        Err(e) => (Status::ERROR, vec![e.to_string()]),
    }
}

#[cfg(test)]
mod tests {
    use super::json_string;

    #[test]
    fn ids_come_from_the_answer() {
        assert_eq!(
            json_string(r#"{"id": "a1b2"}"#, "id").as_deref(),
            Some("a1b2")
        );
        assert_eq!(
            json_string(r#"{"x":1,"id":"z"}"#, "id").as_deref(),
            Some("z")
        );
        assert_eq!(json_string("nope", "id"), None);
    }
}
