//! Windows Error Reporting's own record of the game's crash: the `Report.wer` of an
//! `AppCrash`, `BEX` (a fail-fast or a buffer overrun, which no handler of ours sees) or
//! `AppHang` report in the user's or the machine's report archive and queue. It names the
//! faulting module, its offset and the exception, and lists every loaded module, also when
//! the loader's handlers never ran or the process died before its own dump was done.
//!
//! The files are UTF-16; they go into the incident as UTF-8 text (`wer-<n>.txt`), so the
//! report's scrubbing applies to them, with the user's profile folder replaced.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Kinds of report folder that are about a crash or a hang, lower case.
const KINDS: [&str; 5] = ["appcrash_", "bex64_", "bex_", "apphang_", "critical_"];
/// Reports taken at most, the newest.
const MAX_REPORTS: usize = 3;
/// A bigger `Report.wer` is not read.
const MAX_SIZE: u64 = 2 << 20;

/// The folders Windows keeps its reports in.
fn roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for var in ["LOCALAPPDATA", "ProgramData"] {
        if let Some(base) = std::env::var_os(var) {
            let wer = PathBuf::from(base).join("Microsoft\\Windows\\WER");
            roots.push(wer.join("ReportArchive"));
            roots.push(wer.join("ReportQueue"));
        }
    }
    roots
}

/// The text of a `Report.wer`: UTF-16 with a byte order mark, or plain bytes.
fn decode(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| u16::from_le_bytes(*p))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Whether the report is about the executable `exe` (a file name, lower case): the file
/// name of its `AppPath` or `OriginalFilename` is `exe` itself, not merely ends with it.
fn is_about(text: &str, exe: &str) -> bool {
    text.lines().any(|line| {
        let line = line.to_lowercase();
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        (key == "apppath" || key == "originalfilename")
            && value.trim_end().rsplit(['\\', '/']).next() == Some(exe)
    })
}

/// Windows' reports about `exe` made since `since` (Unix seconds), the newest
/// [`MAX_REPORTS`], newest first: what [`write`] puts into an incident.
pub(super) fn find(exe: &str, since: u64) -> Vec<String> {
    let exe = exe.to_lowercase();
    let mut found: Vec<(SystemTime, String)> = Vec::new();
    for root in roots() {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !KINDS.iter().any(|k| name.starts_with(k)) {
                continue;
            }
            let report = entry.path().join("Report.wer");
            let Ok(meta) = fs::metadata(&report) else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            if crate::reports::unix(modified) < since || meta.len() > MAX_SIZE {
                continue;
            }
            let Ok(bytes) = fs::read(&report) else {
                continue;
            };
            let text = decode(&bytes);
            if is_about(&text, &exe) {
                found.push((modified, text));
            }
        }
    }
    found.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
    found.truncate(MAX_REPORTS);
    found.into_iter().map(|(_, text)| text).collect()
}

/// Writes `reports` ([`find`]) into the incident folder `into`; how many.
pub(super) fn write(into: &Path, reports: &[String]) -> usize {
    let mut written = 0;
    for (n, text) in reports.iter().enumerate() {
        let target = into.join(format!("wer-{}.txt", n + 1));
        if fs::write(target, super::crashinfo::shown_text(text)).is_ok() {
            written += 1;
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use super::{decode, is_about};

    #[test]
    fn reports_are_read_as_utf16_and_matched_by_path() {
        let text = "Version=1\r\nAppPath=C:\\Games\\TrainSimWorld.exe\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let decoded = decode(&bytes);
        assert_eq!(decoded, text);
        assert!(is_about(&decoded, "trainsimworld.exe"));
        assert!(!is_about(&decoded, "other.exe"));
        assert!(
            !is_about(&decoded, "simworld.exe"),
            "a longer name ending the same"
        );
        assert!(is_about(
            "OriginalFilename=TrainSimWorld.exe",
            "trainsimworld.exe"
        ));
        assert!(!is_about(
            "LoadedModule[0]=C:\\TrainSimWorld.exe",
            "trainsimworld.exe"
        ));
    }
}
