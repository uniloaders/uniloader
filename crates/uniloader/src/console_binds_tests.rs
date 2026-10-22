//! Tests for the bind-family built-ins, run through [`crate::run`].

use uniloader_api::Status;
use uniloader_core::binds::{chord, Fire};

use crate::run::tests::fake;
use crate::run::Effects;

fn fires(f: &crate::run::tests::Fake, key: &str) -> Option<Fire> {
    let key = chord(key).unwrap_or_else(|e| panic!("{e}"));
    f.with_state(|s| s.binds.lookup(key)).map(|(fire, _)| fire)
}

fn saved(f: &crate::run::tests::Fake) -> String {
    std::fs::read_to_string(f.config_dir().join("binds.cfg")).unwrap_or_default()
}

#[test]
fn the_owners_examples() {
    let (f, _) = fake();
    assert_eq!(
        f.run(r#"bind A "exec plugin.command arg1 arg2""#).status,
        Status::OK
    );
    assert_eq!(
        f.run(r#"bind CTRL+F9 "set plugin.setting value;reload""#)
            .status,
        Status::OK
    );
    assert_eq!(
        fires(&f, "a"),
        Some(Fire::Script("exec plugin.command arg1 arg2".into()))
    );
    let text = saved(&f);
    assert!(
        text.contains("bind ctrl+f9 \"set plugin.setting value;reload\"\n"),
        "{text}"
    );
    // What pressing A runs.
    let ran = f.run("exec plugin.command arg1 arg2");
    assert_eq!(ran.status, Status::OK);
    assert_eq!(f.calls(), ["command command arg1 arg2"]);
    // A plugin's suggestion (f9 runs plugin.menu) is unbound, and that is saved.
    assert_eq!(fires(&f, "f9"), Some(Fire::Hotkey("plugin.menu".into())));
    f.run("unbind f9");
    assert_eq!(fires(&f, "f9"), None);
    assert!(saved(&f).contains("rebind plugin.menu none\n"));
    f.run("unbind A");
    assert!(!saved(&f).contains("bind a "));
    f.run("binds reset plugin.menu");
    assert_eq!(fires(&f, "f9"), Some(Fire::Hotkey("plugin.menu".into())));
}

#[test]
fn listing_and_showing_binds() {
    let (f, _) = fake();
    f.run("bind f9 \"echo mine\"");
    let ran = f.run("binds");
    assert!(
        ran.printed
            .iter()
            .any(|l| l.contains("removed plugin.menu")),
        "{:?}",
        ran.printed
    );
    assert!(ran.printed.iter().any(|l| l.contains("(user)")));
    assert_eq!(f.run("bind f9").printed, [r#"f9 = "echo mine"  (user)"#]);
    assert_eq!(f.run("bind f2").printed, ["f2 is not bound"]);
    assert_eq!(f.run("bind nokey x").status, Status::INVALID_ARGUMENT);
    assert_eq!(f.run("unbindall").status, Status::OK);
    assert_eq!(
        f.run("binds").printed.len(),
        1,
        "only the removed suggestion"
    );
    f.run("binds reset all");
    assert_eq!(fires(&f, "f9"), Some(Fire::Hotkey("plugin.menu".into())));
    f.run("rebind plugin.menu ctrl+f9");
    assert_eq!(
        fires(&f, "ctrl+f9"),
        Some(Fire::Hotkey("plugin.menu".into()))
    );
    let ran = f.run("rebind later.thing f3");
    assert!(ran.printed[0].contains("kept for when one does"));
}

#[test]
fn toggle_increment_and_reset() {
    let (f, _) = fake();
    // The fixture's setting is an int 0..=10 with default 1.
    f.run("incrementvar plugin.setting 0 3 2");
    assert_eq!(f.run("plugin.setting").printed, ["plugin.setting = 3"]);
    f.run("incrementvar plugin.setting 0 3 2");
    assert_eq!(
        f.run("plugin.setting").printed,
        ["plugin.setting = 0"],
        "wraps to min"
    );
    f.run("toggle plugin.setting 5 7");
    assert_eq!(f.run("plugin.setting").printed, ["plugin.setting = 5"]);
    f.run("toggle plugin.setting 5 7");
    assert_eq!(f.run("plugin.setting").printed, ["plugin.setting = 7"]);
    assert_eq!(
        f.run("toggle plugin.setting").status,
        Status::INVALID_ARGUMENT
    );
    f.run("reset plugin.setting");
    assert_eq!(f.run("plugin.setting").printed, ["plugin.setting = 1"]);
    assert_eq!(f.run("reset plugin.nope").status, Status::NOT_FOUND);
    assert!((super::step_value(0.9, 0.1, 4.0, 0.1) - 1.0).abs() < f64::EPSILON);
    assert!((super::step_value(4.0, 0.1, 4.0, 0.1) - 0.1).abs() < f64::EPSILON);
}

#[test]
fn plus_minus_aliases_and_alias_listing() {
    let (f, _) = fake();
    f.run(r#"alias +fast "set plugin.setting 4"; alias -fast "reset plugin.setting""#);
    f.run("bind mouse5 +fast");
    f.run("+fast");
    assert_eq!(f.run("plugin.setting").printed, ["plugin.setting = 4"]);
    f.run("-fast");
    assert_eq!(f.run("plugin.setting").printed, ["plugin.setting = 1"]);
    let ran = f.run("aliases");
    assert_eq!(ran.printed.len(), 2);
    assert!(saved(&f).contains("alias +fast \"set plugin.setting 4\"\n"));
    f.run(r#"alias +fast """#);
    assert_eq!(f.run("aliases").printed.len(), 1);
}

#[test]
fn secrets_never_reach_the_console_binds_or_files() {
    let (f, h) = fake();
    f.with_state(|s| {
        s.phase = crate::state::Phase::Starting(h);
        let mut desc = crate::state::tests::desc("token");
        desc.kind = uniloader_core::settings::Kind::String;
        desc.default = String::new();
        desc.secret = true;
        s.register_setting(h, desc)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.hashed.insert("plugin.token".to_owned());
        s.phase = crate::state::Phase::Running;
    });
    assert_eq!(f.run("plugin.token").printed, ["plugin.token = <unset>"]);
    let ran = f.run("set plugin.token hunter2hunter2");
    assert_eq!(ran.printed, ["plugin.token = <set>"]);
    f.run("plugin.token hunter2hunter2; list plugin.t");
    assert_eq!(
        f.run(r#"bind f1 "set plugin.token x""#).status,
        Status::INVALID_ARGUMENT
    );
    assert_eq!(
        f.run(r#"alias a "plugin.token x""#).status,
        Status::INVALID_ARGUMENT
    );
    let everything: Vec<String> =
        f.with_state(|s| s.console.iter().map(|l| l.text.clone()).collect());
    assert!(
        everything.iter().all(|l| !l.contains("hunter2")),
        "{everything:?}"
    );
    assert!(everything.contains(&"> set plugin.token <redacted>".to_owned()));
    // Stored as a hash, in memory and on disk.
    let stored = f.with_state(|s| s.get_setting(None, "plugin.token"));
    let stored = stored.unwrap_or_default();
    assert!(crate::secret::verify(&stored, "hunter2hunter2"));
    let file = std::fs::read_to_string(f.config_dir().join("plugin.toml")).unwrap_or_default();
    assert!(
        !file.contains("hunter2") && file.contains("$argon2id"),
        "{file}"
    );
    assert!(!saved(&f).contains("hunter2"));
}

#[test]
fn keys_the_game_uses_are_pointed_out() {
    let (f, _) = fake();
    f.with_state(|s| s.reserve_keys("game: Destination", "f7, f8"))
        .unwrap_or_else(|e| panic!("{e:?}"));
    let ran = f.run("bind f7 \"echo x\"");
    assert_eq!(ran.status, Status::OK, "never blocked");
    assert!(ran
        .printed
        .iter()
        .any(|l| l.contains("also game: Destination")));
    assert!(f
        .run("binds f7")
        .printed
        .iter()
        .any(|l| l.ends_with("[also game: Destination]")));
}
