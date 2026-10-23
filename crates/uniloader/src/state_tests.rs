//! Unit tests for [`crate::state`].

use super::*;

pub(crate) fn state() -> State {
    let dir = std::env::temp_dir().join(format!(
        "uniloader-state-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    State::new(Paths::from_game_dir(&dir), LoaderConfig::default())
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn described(name: &str) -> Described {
    Described {
        name: name.to_owned(),
        version: "1".to_owned(),
        ..Described::default()
    }
}

pub(crate) fn desc(key: &str) -> settings::Desc {
    settings::Desc {
        key: key.into(),
        title: String::new(),
        description: String::new(),
        kind: settings::Kind::Int,
        default: "1".into(),
        min: 0.0,
        max: 10.0,
        choices: Vec::new(),
        labels: Vec::new(),
        restart_required: false,
        advanced: false,
        transient: false,
        secret: false,
    }
}

pub(crate) fn started(s: &mut State, name: &str) -> PluginHandle {
    let h = s
        .add_plugin(described(name), &format!("{name}.dll"))
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.phase = Phase::Starting(h);
    h
}

#[test]
fn registrations_require_start_phase() {
    let mut s = state();
    let h = s
        .add_plugin(described("a"), "a.dll")
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(s.register_setting(h, desc("x")), Err(Status::WRONG_PHASE));
    s.phase = Phase::Starting(h);
    assert_eq!(s.register_setting(h, desc("x")), Ok(()));
    assert_eq!(
        s.register_setting(h, desc("x")),
        Err(Status::ALREADY_EXISTS)
    );
    assert_eq!(
        s.register_setting(h, desc("Bad Key")),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        s.register_hotkey(h, "go", "Go", "f5"),
        Ok("a.go".to_owned())
    );
    assert_eq!(
        s.register_hotkey(h, "go", "Go", "f5"),
        Err(Status::ALREADY_EXISTS)
    );
    assert_eq!(
        s.register_hotkey(h, "bad", "Bad", "nokey"),
        Err(Status::INVALID_ARGUMENT)
    );
    s.phase = Phase::Running;
    assert_eq!(s.subscribe(h, "late"), Err(Status::WRONG_PHASE));
    assert_eq!(
        s.add_plugin(described("a"), "a2.dll"),
        Err(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn settings_resolve_own_and_qualified_names() {
    let mut s = state();
    let a = started(&mut s, "a");
    s.register_setting(a, desc("x"))
        .unwrap_or_else(|e| panic!("{e:?}"));
    let b = started(&mut s, "b");
    s.register_setting(b, desc("x"))
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.phase = Phase::Running;
    s.set_enabled(a, true);
    s.set_enabled(b, true);

    assert_eq!(s.get_setting(Some(a), "x"), Ok("1".into()));
    assert_eq!(s.get_setting(None, "x"), Err(Status::NOT_FOUND));
    assert_eq!(s.get_setting(None, "b.x"), Ok("1".into()));
    let changed = s
        .set_setting(Some(a), "b.x", "7")
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(
        !s.paths.plugin_config("b").exists(),
        "written after the lock"
    );
    let older = changed.persist.clone();
    changed
        .persist
        .unwrap_or_else(|| panic!("persisted"))
        .write();
    assert_eq!(
        changed.notify,
        Some(Notify::SettingChanged {
            plugin: b,
            key: "x".into(),
            value: "7".into()
        })
    );
    assert_eq!(
        s.set_setting(None, "b.x", "7"),
        Ok(Changed {
            notify: None,
            persist: None
        }),
        "unchanged value notifies nobody"
    );
    assert!(matches!(
        s.set_setting(None, "b.x", "99"),
        Err((Status::INVALID_ARGUMENT, _))
    ));
    assert!(matches!(
        s.set_setting(None, "c.x", "1"),
        Err((Status::NOT_FOUND, _))
    ));
    assert!(s.paths.plugin_config("b").exists(), "persisted to disk");
    assert_eq!(crate::config::load_plugin_values(&s.paths, "b")["x"], "7");
    // A later set written first wins over an earlier one arriving late.
    let later = s
        .set_setting(None, "b.x", "8")
        .unwrap_or_else(|e| panic!("{e:?}"));
    later.persist.unwrap_or_else(|| panic!("persisted")).write();
    older.unwrap_or_else(|| panic!("persisted")).write();
    assert_eq!(crate::config::load_plugin_values(&s.paths, "b")["x"], "8");
    let _ = std::fs::remove_dir_all(&s.paths.game_dir);
}

#[test]
fn subscribers_exclude_sender_and_disabled() {
    let mut s = state();
    let a = started(&mut s, "a");
    s.subscribe(a, "pose").unwrap_or_else(|e| panic!("{e:?}"));
    let b = started(&mut s, "b");
    s.subscribe(b, "pose").unwrap_or_else(|e| panic!("{e:?}"));
    let c = started(&mut s, "c");
    s.subscribe(c, "pose").unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(s.subscribe(c, ""), Err(Status::INVALID_ARGUMENT));
    for h in [a, b] {
        s.set_enabled(h, true);
    }
    assert_eq!(s.subscribers(a, "pose"), vec![b]);
    assert_eq!(s.subscribers(a, "other"), Vec::<PluginHandle>::new());
}

#[test]
fn a_pause_is_not_a_stop() {
    let mut s = state();
    let a = started(&mut s, "a");
    let state_of = |s: &State| s.plugin(a).map(PluginRecord::state);
    s.set_enabled(a, true);
    assert_eq!(state_of(&s), Some(PluginState::RUNNING));
    s.set_enabled(a, false);
    assert_eq!(state_of(&s), Some(PluginState::DISABLED));
    assert!(!s.has_ended(a));
    s.set_ended(a, PluginState::STOPPED);
    assert_eq!(state_of(&s), Some(PluginState::STOPPED));
    assert!(s.has_ended(a));
    // Enabling again does not bring a stopped plugin back.
    s.set_enabled(a, true);
    assert_eq!(state_of(&s), Some(PluginState::STOPPED));
}

#[test]
fn services_need_a_running_provider() {
    let mut s = state();
    let a = started(&mut s, "a");
    assert_eq!(s.register_service(a, "overlay", 1, 0x10), Ok(()));
    assert_eq!(
        s.register_service(PluginHandle(9), "x", 1, 0x10),
        Err(Status::NOT_FOUND)
    );
    assert_eq!(s.service("overlay"), None, "not running yet");
    s.set_enabled(a, true);
    assert_eq!(s.service("overlay").map(|x| x.table), Some(0x10));
    s.set_enabled(a, false);
    assert_eq!(s.service("overlay"), None, "paused provider");
}

#[test]
fn rebinding_persists_only_differences_from_the_default() {
    let mut s = state();
    let a = started(&mut s, "a");
    s.register_hotkey(a, "go", "Go", "f5")
        .unwrap_or_else(|e| panic!("{e:?}"));
    let save = s
        .rebind_hotkey("a.go", "ctrl+f6, f7")
        .unwrap_or_else(|e| panic!("{e:?}"));
    save.write();
    let text = std::fs::read_to_string(s.paths.binds_file()).unwrap_or_default();
    assert!(text.contains("rebind a.go \"ctrl+f6, f7\"\n"), "{text}");
    // Read back by the next start.
    let next = State::new(s.paths.clone(), LoaderConfig::default());
    assert!(next.binds.save().contains("rebind a.go"));
    s.rebind_hotkey("a.go", "f5")
        .unwrap_or_else(|e| panic!("{e:?}"))
        .write();
    let text = std::fs::read_to_string(s.paths.binds_file()).unwrap_or_default();
    assert!(!text.contains("rebind a.go"), "{text}");
    assert_eq!(s.rebind_hotkey("a.nope", "f5"), Err(Status::NOT_FOUND));
    assert_eq!(
        s.rebind_hotkey("a.go", "nokey"),
        Err(Status::INVALID_ARGUMENT)
    );
    let _ = std::fs::remove_dir_all(&s.paths.game_dir);
}

#[test]
fn captured_output_survives_a_full_history() {
    let mut s = state();
    // Fill the buffer first: a position in it is then meaningless, which is the bug this
    // guards against — capture used to skip by index and returned nothing.
    for i in 0..(CONSOLE_HISTORY + 5) {
        s.console_print(format!("old {i}"));
    }
    let mark = s.console_mark();
    s.console_print("new 1".to_owned());
    s.console_print("new 2".to_owned());
    assert_eq!(
        s.console_since(mark),
        vec!["new 1".to_owned(), "new 2".to_owned()]
    );
}

#[test]
fn capture_cannot_return_evicted_lines() {
    let mut s = state();
    let mark = s.console_mark();
    for i in 0..(CONSOLE_HISTORY + 5) {
        s.console_print(i.to_string());
    }
    let captured = s.console_since(mark);
    assert_eq!(captured.len(), CONSOLE_HISTORY);
    assert_eq!(captured.first().map(String::as_str), Some("5"));
}

#[test]
fn console_view_includes_log_records() {
    let mut s = state();
    let mark = s.console_mark();
    s.console_push(
        scrollback::Kind::Log(log::Level::Warn),
        "t".to_owned(),
        "careful".to_owned(),
    );
    s.console_print("out".to_owned());
    assert_eq!(s.console_since(mark), vec!["out".to_owned()]);
    let all = s.console_all_since(mark);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].kind, scrollback::Kind::Log(log::Level::Warn));
    assert_eq!(
        (all[0].target.as_str(), all[0].text.as_str()),
        ("t", "careful")
    );
    assert_eq!(all[0].clock.len(), "HH:MM:SS".len());
}

#[test]
fn a_capturing_plugin_keeps_only_its_own_hotkeys() {
    let mut s = state();
    let described = |name: &str| Described {
        name: name.to_owned(),
        ..Described::default()
    };
    let overlay = s.add_plugin(described("overlay"), "o.dll");
    let other = s.add_plugin(described("other"), "x.dll");
    let (Ok(overlay), Ok(other)) = (overlay, other) else {
        panic!("plugins added");
    };
    let event = |owner: Option<&str>, edge| uniloader_core::binds::Event {
        chord: uniloader_core::binds::chord("f1").unwrap_or_else(|e| panic!("{e}")),
        edge,
        fire: uniloader_core::binds::Fire::Script(String::new()),
        owner: owner.map(str::to_owned),
    };
    let press = uniloader_core::binds::Edge::Press;
    let fired = || {
        vec![
            event(Some("overlay"), press),
            event(Some("other"), press),
            event(None, press),
            event(None, uniloader_core::binds::Edge::Release),
        ]
    };
    let owners = |events: Vec<uniloader_core::binds::Event>| -> Vec<Option<String>> {
        events.into_iter().map(|e| e.owner).collect()
    };
    assert_eq!(s.allowed_binds(fired()).len(), 4, "nobody captures");
    s.claim_input(overlay, true);
    assert_eq!(s.allowed_binds(fired()).len(), 4, "a stopped owner is void");
    s.plugins[0].enabled = true;
    s.plugins[1].enabled = true;
    assert_eq!(
        owners(s.allowed_binds(fired())),
        [Some("overlay".to_owned()), None],
        "its own presses, and every release"
    );
    s.claim_input(other, true);
    assert_eq!(
        owners(s.allowed_binds(fired())),
        [Some("other".to_owned()), None]
    );
    s.claim_input(other, false);
    assert_eq!(
        owners(s.allowed_binds(fired())),
        [Some("overlay".to_owned()), None],
        "giving a claim up hands the keyboard back"
    );
    s.claim_input(overlay, false);
    assert_eq!(s.allowed_binds(fired()).len(), 4);
}

#[test]
fn completion_names_include_builtins() {
    let s = state();
    let names = s.completion_names();
    assert!(names.iter().any(|n| n == "plugin load"));
    assert!(names.windows(2).all(|w| w[0] < w[1]), "sorted, no repeats");
}

#[test]
fn console_history_is_bounded() {
    let mut s = state();
    for i in 0..(CONSOLE_HISTORY + 5) {
        s.console_print(i.to_string());
    }
    assert_eq!(s.console.len(), CONSOLE_HISTORY);
    assert_eq!(s.console.front().map(|line| line.text.as_str()), Some("5"));
}
