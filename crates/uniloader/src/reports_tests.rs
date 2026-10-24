//! Unit tests of the reports' platform-free part ([`super`]).

use super::*;

#[test]
fn stamps_are_utc_and_sort() {
    assert_eq!(stamp(0), "1970-01-01_00-00-00");
    // 2026-10-24 08:12:05 UTC
    assert_eq!(stamp(1_792_829_525), "2026-10-24_08-12-05");
    assert_eq!(stamp(951_782_400), "2000-02-29_00-00-00");
    assert!(stamp(1_792_829_525) < stamp(1_792_829_526));
    assert_eq!(incident_name(0, Kind::Freeze), "1970-01-01_00-00-00_freeze");
    assert_eq!(
        kind_of("1970-01-01_00-00-00_freeze.zip"),
        Some(Kind::Freeze)
    );
    assert_eq!(kind_of("1970-01-01_00-00-00_crash"), Some(Kind::Crash));
    assert_eq!(kind_of("notes.zip"), None);
}

#[test]
fn markers_round_trip() {
    let m = Marker {
        pid: 4242,
        started: 1_792_829_525,
        closing: false,
    };
    assert_eq!(Marker::parse(&m.render()), Some(m));
    let closing = Marker { closing: true, ..m };
    assert_eq!(Marker::parse(&closing.render()), Some(closing));
    assert_eq!(Marker::parse("garbage"), None);
}

#[test]
fn addresses_invites_and_ids_are_scrubbed() {
    assert_eq!(
        redact("[coop] joined 203.0.113.7:7442 via relay"),
        "[coop] joined [ip]:7442 via relay"
    );
    assert_eq!(redact("listening on 192.168.2.11"), "listening on [ip]");
    assert_eq!(
        redact("local 127.0.0.1 and 0.0.0.0"),
        "local 127.0.0.1 and 0.0.0.0"
    );
    // version quads stay readable
    assert_eq!(redact("file version 1.0.0.0"), "file version 1.0.0.0");
    assert_eq!(redact("engine 4.26.2.0"), "engine 4.26.2.0");
    assert_eq!(redact("peer [2001:db8::1]:7442"), "peer [[ip]]:7442");
    assert_eq!(redact("at 08:12:05.123"), "at 08:12:05.123");
    assert_eq!(redact("std::io::Error"), "std::io::Error");
    assert_eq!(
        redact("link tswcoop://abc.def/xyz?k=1 shared"),
        "link tswcoop://[redacted] shared"
    );
    assert_eq!(
        redact("GET https://example.org/v1/x?token=abc done"),
        "GET https://example.org/v1/x?[redacted] done"
    );
    assert_eq!(
        redact("see https://example.org/page"),
        "see https://example.org/page"
    );
    assert_eq!(redact("steam 76561198012345678 ok"), "steam [steam-id] ok");
    assert_eq!(
        redact("key Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4z+/== end"),
        "key [redacted] end"
    );
    // hex hashes are not secrets
    let md5 = "md5 006814336c3ce9c5a3db1b9d5b6b916c";
    assert_eq!(redact(md5), md5);
    assert_eq!(redact("password = hunter2"), "password = [redacted]");
    assert_eq!(redact("\"token\": \"abc\""), "\"token\": [redacted]");
    assert_eq!(redact("3 tokens loaded"), "3 tokens loaded");
    assert_eq!(redact("Straße 10.0.0.1 ü"), "Straße [ip] ü");
    assert_eq!(redact("two\nlines\n"), "two\nlines\n");
}

#[test]
fn only_text_and_dumps_go_in() {
    assert_eq!(content_of("UE4Minidump.dmp"), Some(Content::Dump));
    assert_eq!(content_of("CrashContext.runtime-xml"), Some(Content::Text));
    assert_eq!(content_of("loader.log"), Some(Content::Text));
    assert_eq!(content_of("tswcoop-host-secret.key"), None);
    assert_eq!(content_of("tswcoop-master.json"), None);
    assert_eq!(content_of("coop.toml"), None);
}

#[test]
fn names_from_commands_stay_in_the_folder() {
    assert_eq!(
        checked_name("2026-10-24_08-12-05_crash.zip"),
        Some("2026-10-24_08-12-05_crash.zip")
    );
    assert_eq!(checked_name("../2026-10-24_08-12-05_crash.zip"), None);
    assert_eq!(checked_name("x/2026-10-24_08-12-05_crash.zip"), None);
    assert_eq!(checked_name("2026-10-24_08-12-05_crash"), None);
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("uniloader-reports-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    dir
}

#[test]
fn a_bundle_holds_scrubbed_text_and_dumps_only() {
    let dir = scratch("bundle");
    let crash = dir.join("UE4CC-Windows-1");
    fs::create_dir_all(&crash).unwrap_or_else(|e| panic!("{e}"));
    fs::write(crash.join("UE4Minidump.dmp"), b"MDMP\x93\xa7").unwrap_or_else(|e| panic!("{e}"));
    fs::write(crash.join("game.log"), "joined 203.0.113.7\n").unwrap_or_else(|e| panic!("{e}"));
    fs::write(crash.join("host.key"), "secret").unwrap_or_else(|e| panic!("{e}"));
    let parts = parts_of(&dir, "game", None);
    let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "game/UE4CC-Windows-1/UE4Minidump.dmp",
            "game/UE4CC-Windows-1/game.log"
        ]
    );
    let out = dir.join("report.zip");
    let size = bundle(&out, &parts).unwrap_or_else(|e| panic!("{e}"));
    assert!(size > 0);
    let mut zip = zip::ZipArchive::new(fs::File::open(&out).unwrap_or_else(|e| panic!("{e}")))
        .unwrap_or_else(|e| panic!("{e}"));
    let mut log = String::new();
    zip.by_name("game/UE4CC-Windows-1/game.log")
        .unwrap_or_else(|e| panic!("{e}"))
        .read_to_string(&mut log)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(log, "joined [ip]\n");
    let mut dump = Vec::new();
    zip.by_name("game/UE4CC-Windows-1/UE4Minidump.dmp")
        .unwrap_or_else(|e| panic!("{e}"))
        .read_to_end(&mut dump)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(dump, b"MDMP\x93\xa7");
    assert_eq!(zip.len(), 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn old_reports_are_pruned_and_decided_ones_not_offered() {
    let dir = scratch("prune");
    for i in 0..(KEEP + 2) {
        let name = format!("{}.zip", incident_name(1_000 + i as u64, Kind::Crash));
        fs::write(dir.join(name), b"z").unwrap_or_else(|e| panic!("{e}"));
    }
    prune(&dir);
    let left = zips(&dir);
    assert_eq!(left.len(), KEEP);
    assert_eq!(
        left[0],
        format!("{}.zip", incident_name(1_002, Kind::Crash))
    );
    mark_done(&dir, &left[0], "saved");
    assert_eq!(pending(&dir).len(), KEEP - 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn two_crashes_in_the_same_module_make_a_loop() {
    assert_eq!(
        crash_loop("coop.dll\ncoop.dll\n").as_deref(),
        Some("coop.dll")
    );
    assert_eq!(
        crash_loop("tsw.dll\ncoop.dll\nCOOP.DLL\n").as_deref(),
        Some("COOP.DLL")
    );
    assert_eq!(crash_loop("coop.dll\ntsw.dll\n"), None);
    assert_eq!(crash_loop("coop.dll\n-\ncoop.dll\n"), None);
    assert_eq!(crash_loop("-\n-\n"), None);
    assert_eq!(crash_loop("coop.dll\n"), None);
    assert_eq!(crash_loop(""), None);
}
