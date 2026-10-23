use super::*;
use crate::plugins::tests::bare;
use uniloader_core::schema::{Bind, Command, Setting};
use uniloader_core::settings::{Desc, Kind};

fn plugin() -> schema::Plugin {
    let mut p = bare("demo", "demo.dll", false);
    p.binds = vec![
        Bind {
            name: "demo.menu".into(),
            title: "Menu".into(),
            default: "f10".into(),
            script: None,
        },
        Bind {
            name: "demo.fast".into(),
            title: "Fast".into(),
            default: "f8, mouse5".into(),
            script: Some("set uniloader.speed 2".into()),
        },
    ];
    p.commands = vec![Command {
        name: "hello".into(),
        help: String::new(),
        usage: String::new(),
    }];
    let desc = |key: &str, secret: bool| Desc {
        key: key.into(),
        title: key.into(),
        description: String::new(),
        kind: Kind::String,
        default: String::new(),
        min: 0.0,
        max: 0.0,
        choices: Vec::new(),
        labels: Vec::new(),
        restart_required: false,
        advanced: false,
        transient: false,
        secret,
    };
    p.settings = vec![
        Setting {
            desc: desc("name", false),
            hashed: None,
        },
        Setting {
            desc: desc("token", true),
            hashed: None,
        },
    ];
    p
}

fn chord(text: &str) -> Chord {
    binds::chord(text).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn saved_moves_apply_to_the_suggestions() {
    let p = plugin();
    let t = BindTable::load(
        "// header\nrebind demo.menu \"ctrl+f10\"\nbind f6 \"echo hi\"\nalias +go \"demo.hello\"\n",
        [&p],
    );
    assert!(t.warnings.is_empty(), "{:?}", t.warnings);
    let menu = t
        .entries()
        .find(|e| e.name == "demo.menu")
        .unwrap_or_else(|| panic!("no entry"));
    assert_eq!(menu.binding(), "ctrl+f10");
    assert_eq!(menu.default_binding(), "f10");
    assert_eq!(t.user_binds(), [(chord("f6"), "echo hi".to_owned())]);
    assert_eq!(t.aliases().collect::<Vec<_>>(), [("+go", "demo.hello")]);
}

#[test]
fn edits_are_written_the_way_the_loader_writes_them() {
    let p = plugin();
    let known = Known::from_schemas([&p]);
    let mut t = BindTable::load("", [&p]);
    t.rebind("demo.menu", "Ctrl+F10, sc29")
        .unwrap_or_else(|e| panic!("{e}"));
    let warnings = t
        .bind(None, "F6", "echo hi; demo.hello", &known)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(warnings.is_empty(), "{warnings:?}");
    // A user bind on a suggested key moves the suggestion off it.
    t.bind(None, "f8", "demo.name bob", &known)
        .unwrap_or_else(|e| panic!("{e}"));
    t.set_alias("+fast", "set uniloader.speed 4", &known)
        .unwrap_or_else(|e| panic!("{e}"));
    let text = t.text();
    assert_eq!(
        text,
        "// Written by uniloader after every change. Edit it while the game is closed, or\n\
         // use the console: bind, unbind, rebind, alias, binds reset.\n\
         rebind demo.fast mouse5\n\
         rebind demo.menu \"ctrl+f10, sc29\"\n\
         alias +fast \"set uniloader.speed 4\"\n\
         bind f6 \"echo hi; demo.hello\"\n\
         bind f8 \"demo.name bob\"\n"
    );
    // Read back by a fresh table, as the loader would at the next start.
    let again = BindTable::load(&text, [&p]);
    assert_eq!(again.text(), text);
    assert!(again.warnings.is_empty(), "{:?}", again.warnings);

    // Removing the user bind gives the suggestion its key back (after its other key, which
    // the loader's table keeps as a move); resetting drops the move.
    t.remove_bind(chord("f8"));
    let fast = t
        .entries()
        .find(|e| e.name == "demo.fast")
        .map(binds::Entry::binding);
    assert_eq!(fast.as_deref(), Some("mouse5, f8"));
    t.reset_entry("demo.fast");
    t.reset_entry("demo.menu");
    t.remove_alias("+fast");
    let text = t.text();
    assert!(!text.contains("demo.fast"), "{text}");
    assert!(!text.contains("demo.menu"), "{text}");
    assert!(!text.contains("alias +fast"), "{text}");
    assert_eq!(
        t.user_binds(),
        [(chord("f6"), "echo hi; demo.hello".to_owned())]
    );
}

#[test]
fn editing_a_bind_moves_it_to_the_new_key() {
    let p = plugin();
    let known = Known::from_schemas([&p]);
    let mut t = BindTable::load("bind f6 \"echo hi\"\n", [&p]);
    t.bind(Some(chord("f6")), "f7", "echo hi", &known)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(t.user_binds(), [(chord("f7"), "echo hi".to_owned())]);
}

#[test]
fn bad_keys_and_scripts_are_refused() {
    let p = plugin();
    let known = Known::from_schemas([&p]);
    let mut t = BindTable::load("", [&p]);
    assert!(t.bind(None, "nokey", "echo", &known).is_err());
    assert!(t.bind(None, "none", "echo", &known).is_err());
    assert!(t.bind(None, "f6", "", &known).is_err());
    assert!(t.bind(None, "f6", "echo \"open", &known).is_err());
    assert!(t.rebind("demo.menu", "f1, bogus").is_err());
    // Secrets never go into binds.cfg, not even through an alias inside a bind.
    for script in [
        "set demo.token abc",
        "demo.token abc",
        "alias x \"set demo.token abc\"",
    ] {
        let error = t
            .bind(None, "f6", script, &known)
            .err()
            .unwrap_or_else(|| panic!("{script} accepted"));
        assert!(error.contains("secret"), "{error}");
    }
    assert!(t.set_alias("bad.name", "echo", &known).is_err());
    assert_eq!(t.user_binds(), Vec::new());
}

#[test]
fn unknown_names_are_only_warnings() {
    let p = plugin();
    let known = Known::from_schemas([&p]);
    let mut t = BindTable::load("alias go \"echo\"\n", [&p]);
    let warnings = t
        .bind(
            None,
            "f6",
            "other.thing; go; nothing; action demo.menu; action x.y",
            &known,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings[0].contains("other.thing"));
    assert!(warnings[1].contains("nothing"));
    assert!(warnings[2].contains("x.y"));
    // The picker offers no secret.
    let names = known.names();
    assert!(names.contains(&"demo.hello".to_owned()));
    assert!(names.contains(&"demo.name".to_owned()));
    assert!(names.contains(&"action demo.menu".to_owned()));
    assert!(!names.contains(&"demo.token".to_owned()));
}

#[test]
fn lines_the_loader_skips_are_reported() {
    let p = plugin();
    let t = BindTable::load("echo hello\nbind nokey x\n", [&p]);
    assert_eq!(t.warnings.len(), 2, "{:?}", t.warnings);
    assert!(t.warnings[0].starts_with("binds.cfg line 1"));
}

#[test]
fn a_suggestion_on_a_taken_key_is_shadowed() {
    let p = plugin();
    let mut other = bare("other", "other.dll", false);
    other.binds = vec![Bind {
        name: "other.menu".into(),
        title: "Menu".into(),
        default: "f10".into(),
        script: None,
    }];
    let t = BindTable::load("", [&p, &other]);
    assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
    assert_eq!(t.shadowed("other.menu"), [chord("f10")]);
    assert_eq!(t.shadowed("demo.menu"), Vec::new());
}
