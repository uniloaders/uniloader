//! Crash and freeze reports, the platform-free part: where they live, what goes in, what is
//! scrubbed out, and the zip a player sends (owner request 2026-10-24: "crash reports that
//! they can send me").
//!
//! An incident is a folder under `<root>/reports/` (`2026-10-24_08-12-05_freeze/`) that the
//! platform layer fills with dumps and an `info.txt`; [`bundle`] turns it, the session's logs
//! and whatever plugins attached (a game's own crash folder, a plugin's log file) into
//! `<root>/reports/2026-10-24_08-12-05_freeze.zip`. Text is tail-cut and passed through
//! [`redact`]; only dumps go in as bytes, and files of any other kind are left out, so a key
//! or config file in an attached folder never travels.
//!
//! A `running.txt` marker ([`Marker`]) says a session is on; a normal end marks it closing or
//! removes it. One left at the next start means the session ended without that: crashed,
//! killed, or frozen and closed by force.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The reports folder's name, next to `config/` and `logs/`.
pub const DIR: &str = "reports";
/// The marker file in the reports folder.
pub const MARKER: &str = "running.txt";
/// The incident's own description, written by the platform layer.
pub const INFO: &str = "info.txt";
/// The most of one text file that goes into a report: its end.
pub const MAX_TEXT: u64 = 4 << 20;
/// The most of one dump that goes into a report.
pub const MAX_DUMP: u64 = 64 << 20;
/// Reports kept; older zips are removed.
pub const KEEP: usize = 10;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The game crashed (a crash dump exists).
    Crash,
    /// The game stopped responding and was ended while it did.
    Freeze,
    /// The session ended without closing normally, and nothing says why.
    Exit,
    /// The player asked for a report of the running session.
    Manual,
}

impl Kind {
    /// Every kind with its key, in the folder and zip names.
    const ALL: [(Kind, &'static str); 4] = [
        (Kind::Crash, "crash"),
        (Kind::Freeze, "freeze"),
        (Kind::Exit, "exit"),
        (Kind::Manual, "manual"),
    ];

    /// The key in names.
    #[must_use]
    pub fn key(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(k, _)| *k == self)
            .map_or("exit", |(_, key)| key)
    }

    /// The kind of a key.
    #[must_use]
    pub fn parse(key: &str) -> Option<Kind> {
        Self::ALL
            .iter()
            .find(|(_, k)| *k == key)
            .map(|(kind, _)| *kind)
    }

    /// What the player is told happened.
    #[must_use]
    pub fn headline(self) -> &'static str {
        match self {
            Kind::Crash => "The game crashed last time",
            Kind::Freeze => "The game froze last time",
            Kind::Exit => "The game closed unexpectedly last time",
            Kind::Manual => "A report of this session is ready",
        }
    }
}

/// The running marker: the session's process and when it started (Unix seconds), and whether
/// it was seen closing normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    pub pid: u32,
    pub started: u64,
    pub closing: bool,
}

impl Marker {
    /// The file's text.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "pid {}\nstarted {}\n{}",
            self.pid,
            self.started,
            if self.closing { "closing\n" } else { "" }
        )
    }

    /// A marker from its text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Marker> {
        let mut pid = None;
        let mut started = None;
        let mut closing = false;
        for line in text.lines() {
            match line.split_once(' ') {
                Some(("pid", v)) => pid = v.trim().parse().ok(),
                Some(("started", v)) => started = v.trim().parse().ok(),
                _ if line.trim() == "closing" => closing = true,
                _ => {}
            }
        }
        Some(Marker {
            pid: pid?,
            started: started?,
            closing,
        })
    }
}

/// Seconds since the Unix epoch.
#[must_use]
pub fn unix(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `2026-10-24_08-12-05` (UTC) for Unix seconds `secs`: sorts by time, fits a file name.
#[must_use]
pub fn stamp(secs: u64) -> String {
    let days = secs / 86_400;
    let rest = secs % 86_400;
    let (year, month, day) = civil(days);
    format!(
        "{year:04}-{month:02}-{day:02}_{:02}-{:02}-{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The calendar date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// An incident folder's name, and the zip's without `.zip`.
#[must_use]
pub fn incident_name(secs: u64, kind: Kind) -> String {
    format!("{}_{}", stamp(secs), kind.key())
}

/// The kind in an incident or zip name.
#[must_use]
pub fn kind_of(name: &str) -> Option<Kind> {
    let stem = name.strip_suffix(".zip").unwrap_or(name);
    Kind::parse(stem.rsplit('_').next()?)
}

/// Keys whose values are never shown, in `key = value` or `key: value` text.
const SECRET_KEYS: [&str; 8] = [
    "password", "passwd", "token", "secret", "apikey", "api_key", "api-key", "invite",
];

/// `text` with what identifies a player or opens their session cut out: IP addresses (not
/// loopback, and not version-like quads such as `4.26.2.0`: first number 1-9, last 0, never a
/// host), the part after `scheme://` for schemes other than http(s) and file (invites), query
/// strings, Steam ids, long mixed-case tokens and the values of password/token/secret keys.
#[must_use]
pub fn redact(text: &str) -> String {
    text.lines().map(redact_line).collect::<Vec<_>>().join("\n")
        + if text.ends_with('\n') { "\n" } else { "" }
}

fn redact_line(line: &str) -> String {
    if let Some(at) = secret_value(line) {
        return format!("{} [redacted]", &line[..at]);
    }
    split_keep(line).into_iter().map(redact_word).collect()
}

/// Where the value of the first secret key in `line` starts (after its `=` or `:`). Every
/// occurrence counts, and a key may end a longer one (`rcon_password`, `--token`), but not
/// follow a letter or digit (`mytokens`) or a URL's `?`/`&` (its query is cut below).
fn secret_value(line: &str) -> Option<usize> {
    // ASCII lower case keeps every byte where it was, so its positions index `line`.
    let lower = line.to_ascii_lowercase();
    SECRET_KEYS
        .iter()
        .flat_map(|k| lower.match_indices(k))
        .filter_map(|(i, k)| {
            let starts = lower[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && !matches!(c, '?' | '&'));
            if !starts {
                return None;
            }
            let rest = &line[i + k.len()..];
            let sep = rest.find(['=', ':'])?;
            // `password = x`, `token: x`, `password_hash = x`, not `tokens loaded 3`
            rest[..sep]
                .trim()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '"')
                .then_some(i + k.len() + sep + 1)
        })
        .min()
}

/// `line` cut into words and the separators between them, in order.
fn split_keep(line: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_word = None;
    for (i, c) in line.char_indices() {
        let wordy = !(c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '(' | ')' | '[' | ']' | ',' | ';' | '<' | '>'
            ));
        if in_word.is_some_and(|was| was != wordy) {
            parts.push(&line[start..i]);
            start = i;
        }
        in_word = Some(wordy);
    }
    if start < line.len() {
        parts.push(&line[start..]);
    }
    parts
}

fn redact_word(word: &str) -> String {
    if let Some(i) = word.find("://") {
        let scheme = word[..i].to_ascii_lowercase();
        let tail = scheme
            .rfind(|c: char| !c.is_ascii_alphanumeric() && c != '+' && c != '-')
            .map_or(0, |j| j + 1);
        let scheme = &scheme[tail..];
        if !matches!(scheme, "http" | "https" | "file") {
            return format!("{}://[redacted]", &word[..i]);
        }
        if let Some(q) = word.find('?') {
            return format!("{}?[redacted]", &word[..q]);
        }
        return word.to_owned();
    }
    if is_ipv6(word) {
        return "[ip]".to_owned();
    }
    redact_runs(&redact_ipv4(word))
}

/// Whether `word` (perhaps with a port or zone, in brackets or not) is an `IPv6` address.
fn is_ipv6(word: &str) -> bool {
    let w = word.trim_matches(|c| c == '[' || c == ']');
    let w = w.split('%').next().unwrap_or(w);
    let w = w.split("]:").next().unwrap_or(w);
    let colons = w.matches(':').count();
    if colons < 2
        || !w
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
    {
        return false;
    }
    let groups: Vec<&str> = w.split(':').collect();
    if groups.iter().any(|g| g.len() > 4 && !g.contains('.')) {
        return false;
    }
    // `08:12:05` is a clock: an address has `::` or at least four colons
    w.contains("::") && groups.iter().filter(|g| !g.is_empty()).count() >= 2 || colons >= 4
}

/// Dotted quads in `word` replaced, unless loopback, unspecified, or a version (1-9 first, 0
/// last: `1.0.0.0`, `4.26.2.0`). A quad that starts 1-9 but is a host (`5.6.7.8`, many home
/// connections) goes like any other.
fn redact_ipv4(word: &str) -> String {
    let bytes = word.as_bytes();
    let mut out = String::with_capacity(word.len());
    let (mut copied, mut i) = (0, 0);
    while i < bytes.len() {
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'.');
        if boundary && bytes[i].is_ascii_digit() {
            if let Some((len, octets)) = quad_at(&bytes[i..]) {
                let version = (1..=9).contains(&octets[0]) && octets[3] == 0;
                let keep = octets[0] == 127 || octets == [0, 0, 0, 0] || version;
                if !keep {
                    // `i` and `i + len` sit on ASCII digits: character boundaries
                    out.push_str(&word[copied..i]);
                    out.push_str("[ip]");
                    i += len;
                    copied = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&word[copied..]);
    out
}

/// A dotted quad at the start of `b` (not followed by another `.digit`): its length and
/// octets.
fn quad_at(b: &[u8]) -> Option<(usize, [u16; 4])> {
    let mut octets = [0u16; 4];
    let mut i = 0;
    for (n, octet) in octets.iter_mut().enumerate() {
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() && i - start < 3 {
            *octet = *octet * 10 + u16::from(b[i] - b'0');
            i += 1;
        }
        if i == start || *octet > 255 {
            return None;
        }
        if n < 3 {
            if b.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
    }
    let followed = b.get(i).is_some_and(u8::is_ascii_digit)
        || (b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit));
    (!followed).then_some((i, octets))
}

/// Steam ids (17 digits from `7656119`) and long tokens (32+ characters of letters, digits
/// and `+/=_-` with upper case, lower case and digits all in it) replaced in `word`.
fn redact_runs(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        let digits = run.chars().all(|c| c.is_ascii_digit());
        if digits && run.len() == 17 && run.starts_with("7656119") {
            out.push_str("[steam-id]");
        } else if run.len() >= 32
            && run.chars().any(|c| c.is_ascii_uppercase())
            && run.chars().any(|c| c.is_ascii_lowercase())
            && run.chars().any(|c| c.is_ascii_digit())
        {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in word.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-') {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// What a file goes into a report as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// Its end, scrubbed.
    Text,
    /// Its bytes.
    Dump,
}

/// How a file of this name goes into a report; `None` keeps it out.
#[must_use]
pub fn content_of(name: &str) -> Option<Content> {
    let ext = Path::new(name).extension()?;
    if ext.eq_ignore_ascii_case("dmp") {
        return Some(Content::Dump);
    }
    ["log", "txt", "xml", "runtime-xml"]
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
        .then_some(Content::Text)
}

/// One file for a report: where it is and its name in the zip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub path: PathBuf,
    pub name: String,
}

/// The reportable files at `path`: a folder's files and its subfolders' (two levels), named
/// under `prefix/`, or a single file, named `prefix` (which then needs its extension); with
/// `since`, only those changed since then (Unix seconds).
#[must_use]
pub fn parts_of(path: &Path, prefix: &str, since: Option<u64>) -> Vec<Part> {
    let mut parts = Vec::new();
    match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => collect(path, prefix, since, 2, &mut parts),
        Ok(meta) => parts.extend(part(path.to_path_buf(), prefix.to_owned(), &meta, since)),
        Err(_) => {}
    }
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    parts
}

fn collect(dir: &Path, prefix: &str, since: Option<u64>, depth: u32, parts: &mut Vec<Part>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if depth > 0 {
                collect(&path, &format!("{prefix}/{name}"), since, depth - 1, parts);
            }
            continue;
        }
        parts.extend(part(path, format!("{prefix}/{name}"), &meta, since));
    }
}

/// The file at `path` as `name` in the zip, when it is of a kind that goes in and changed
/// since `since`.
fn part(path: PathBuf, name: String, meta: &fs::Metadata, since: Option<u64>) -> Option<Part> {
    let fresh = since.is_none_or(|s| meta.modified().is_ok_and(|m| unix(m) >= s));
    (meta.is_file() && fresh && content_of(&name).is_some()).then_some(Part { path, name })
}

/// The zip folder or file name for each attached path: its own name, with `-2`, `-3`… on a
/// name taken before (a zip holds each name once; two `Logs` folders would fail the bundle).
#[must_use]
pub fn attached_labels(paths: &[PathBuf]) -> Vec<String> {
    let mut taken: Vec<String> = Vec::with_capacity(paths.len());
    for path in paths {
        let own = path
            .file_name()
            .map_or_else(|| "folder".to_owned(), |n| n.to_string_lossy().into_owned());
        let mut label = own.clone();
        let mut n = 2;
        while taken.iter().any(|t| t.eq_ignore_ascii_case(&label)) {
            label = numbered(&own, n);
            n += 1;
        }
        taken.push(label);
    }
    taken
}

/// `name` with `-n` before its extension (`game.log` → `game-2.log`), so it keeps its kind.
fn numbered(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{n}.{ext}"),
        _ => format!("{name}-{n}"),
    }
}

/// The end of the file at `path`, at most `max` bytes, cut to a line start.
fn tail(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let skip = len.saturating_sub(max);
    file.seek(SeekFrom::Start(skip))?;
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes)?;
    if skip > 0 {
        if let Some(nl) = bytes.iter().position(|b| *b == b'\n') {
            bytes.drain(..=nl);
        }
    }
    Ok(bytes)
}

/// Writes `parts` into a new zip at `out`: text tail-cut and scrubbed, dumps as they are; a
/// part that cannot be read is listed in `missing.txt` instead. The zip's size.
///
/// # Errors
/// When the zip cannot be written.
pub fn bundle(out: &Path, parts: &[Part]) -> io::Result<u64> {
    let tmp = out.with_extension("zip.part");
    let written = write_zip(&tmp, parts).and_then(|()| fs::rename(&tmp, out));
    if written.is_err() {
        // A disk that filled up mid-zip leaves no half zip behind to fill it further.
        let _ = fs::remove_file(&tmp);
    }
    written?;
    Ok(fs::metadata(out)?.len())
}

fn write_zip(tmp: &Path, parts: &[Part]) -> io::Result<()> {
    let file = fs::File::create(tmp)?;
    let mut zip = zip::ZipWriter::new(io::BufWriter::new(file));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut missing = Vec::new();
    for part in parts {
        let bytes = match content_of(&part.name) {
            Some(Content::Text) => tail(&part.path, MAX_TEXT)
                .map(|b| redact(&String::from_utf8_lossy(&b)).into_bytes()),
            Some(Content::Dump) => tail(&part.path, MAX_DUMP),
            None => continue,
        };
        match bytes {
            Ok(bytes) => {
                zip.start_file(part.name.as_str(), options)
                    .map_err(io::Error::other)?;
                zip.write_all(&bytes)?;
            }
            Err(e) => missing.push(format!("{}: {e}", part.name)),
        }
    }
    if !missing.is_empty() {
        zip.start_file("missing.txt", options)
            .map_err(io::Error::other)?;
        zip.write_all(missing.join("\n").as_bytes())?;
    }
    let mut writer = zip.finish().map_err(io::Error::other)?;
    writer.flush()
}

/// Whether `name` ends in `.zip`.
fn is_zip(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// The zips in `dir`, oldest first.
#[must_use]
pub fn zips(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_zip(n) && kind_of(n).is_some())
        .collect();
    names.sort();
    names
}

/// Removes all but the newest [`KEEP`] zips in `dir` (and their `.done` notes).
pub fn prune(dir: &Path) {
    let names = zips(dir);
    for name in names.iter().take(names.len().saturating_sub(KEEP)) {
        let _ = fs::remove_file(dir.join(name));
        let _ = fs::remove_file(dir.join(format!("{name}.done")));
    }
}

/// The zips in `dir` nobody has decided about yet (no `.done` note beside them).
#[must_use]
pub fn pending(dir: &Path) -> Vec<String> {
    zips(dir)
        .into_iter()
        .filter(|n| !dir.join(format!("{n}.done")).exists())
        .collect()
}

/// Notes what became of the zip `name` in `dir` (sent with its id, saved to the Desktop), so
/// it is not offered again.
pub fn mark_done(dir: &Path, name: &str, what: &str) {
    let _ = fs::write(dir.join(format!("{name}.done")), format!("{what}\n"));
}

/// A safe zip name from a command argument: one of ours, no path in it.
#[must_use]
pub fn checked_name(name: &str) -> Option<&str> {
    let ok = !name.contains(['/', '\\']) && !name.contains("..") && kind_of(name).is_some();
    (ok && is_zip(name)).then_some(name)
}

/// The setting saying what happens with a report: its key on the loader's entry.
pub const SETTING: &str = "crash_reports";
/// Ask whether to send it or save it to the Desktop.
pub const ASK: &str = "ask";
/// Save it to the Desktop without asking, never send it.
pub const DESKTOP: &str = "desktop";
/// Make no reports.
pub const OFF: &str = "off";

/// The loader's report setting.
#[must_use]
pub fn settings() -> Vec<uniloader_core::settings::Desc> {
    vec![uniloader_core::settings::Desc {
        key: SETTING.to_owned(),
        title: "Crash reports".to_owned(),
        description: "When the game crashes, freezes or closes unexpectedly, the next start \
                      makes a report: crash dumps and logs, with IP addresses, invites, ids \
                      and passwords removed. Ask: offer to send it to the mod authors or save \
                      it to the Desktop. Desktop: always save it there, never send it. Off: no \
                      reports."
            .to_owned(),
        kind: uniloader_core::settings::Kind::Enum,
        default: ASK.to_owned(),
        min: 0.0,
        max: 0.0,
        choices: vec![ASK.to_owned(), DESKTOP.to_owned(), OFF.to_owned()],
        labels: vec![
            "Ask what to do".to_owned(),
            "Save to the Desktop".to_owned(),
            "Off".to_owned(),
        ],
        restart_required: false,
        advanced: false,
        transient: false,
        secret: false,
    }]
}

/// The `report` command's help and usage.
pub const COMMAND_HELP: &str = "Crash and freeze reports: list them, make one now, send one \
                                or save it to the Desktop.";
/// The last `n` lines of a log, redacted as reports are: what a session was doing when it
/// ended without a word.
#[must_use]
pub fn last_lines(log: &str, n: usize) -> String {
    let lines: Vec<&str> = log.lines().collect();
    let start = lines.len().saturating_sub(n);
    redact(&lines[start..].join("\n"))
}

/// A crash that named none of the loader's modules, in the suspects list.
pub const NO_SUSPECT: &str = "-";

/// The module the suspects list's last two crashes name, when both name the same one: a
/// crash loop.
#[must_use]
pub fn crash_loop(list: &str) -> Option<String> {
    let mut newest = list.lines().map(str::trim).filter(|l| !l.is_empty()).rev();
    let (a, b) = (newest.next()?, newest.next()?);
    (a != NO_SUSPECT && a.eq_ignore_ascii_case(b)).then(|| a.to_owned())
}

/// The `report` command's usage.
pub const COMMAND_USAGE: &str =
    "report [list | now | send <zip> | desktop <zip> | url <https url> \
     | attach <folder|file> | loading on|off | crash [fault|call|stack|panic]]";

#[cfg(test)]
#[path = "reports_tests.rs"]
mod tests;
