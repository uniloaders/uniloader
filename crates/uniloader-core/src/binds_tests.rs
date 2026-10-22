//! Unit tests for [`crate::binds`].

use super::*;

struct Keys {
    down: Vec<u16>,
    mods: Modifiers,
}

impl KeyState for Keys {
    fn is_down(&self, vk: u16) -> bool {
        self.down.contains(&vk)
    }
    fn modifiers(&self) -> Modifiers {
        self.mods
    }
}

const F9: u16 = 0x78;
const F10: u16 = 0x79;
const F11: u16 = 0x7A;
const F12: u16 = 0x7B;
const CTRL: Modifiers = Modifiers {
    ctrl: true,
    alt: false,
    shift: false,
};

fn c(text: &str) -> Chord {
    chord(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn poll(t: &mut Table, down: &[u16], mods: Modifiers) -> Vec<(Edge, Fire)> {
    t.poll(&Keys {
        down: down.to_vec(),
        mods,
    })
    .into_iter()
    .map(|e| (e.edge, e.fire))
    .collect()
}

fn none() -> Modifiers {
    Modifiers::default()
}

fn hotkey(name: &str) -> Fire {
    Fire::Hotkey(name.to_owned())
}

fn script(text: &str) -> Fire {
    Fire::Script(text.to_owned())
}

fn table() -> Table {
    let mut t = Table::default();
    let ok = |r: Result<Vec<String>, BindError>| r.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        ok(t.register("vr.toggle", "Toggle", "f12, f9", Target::Hotkey)).len(),
        0
    );
    assert_eq!(
        ok(t.register("vr.recenter", "Recenter", "ctrl+f11", Target::Hotkey)).len(),
        0
    );
    t
}

#[test]
fn presses_fire_with_exact_modifiers_and_releases_follow_the_main_key() {
    let mut t = table();
    assert_eq!(
        poll(&mut t, &[F9], none()),
        [(Edge::Press, hotkey("vr.toggle"))]
    );
    assert!(poll(&mut t, &[F9], none()).is_empty(), "held fires once");
    assert_eq!(
        poll(&mut t, &[], none()),
        [(Edge::Release, hotkey("vr.toggle"))]
    );
    assert!(poll(&mut t, &[F11], none()).is_empty(), "f11 needs ctrl");
    poll(&mut t, &[], none());
    assert_eq!(
        poll(&mut t, &[F11], CTRL),
        [(Edge::Press, hotkey("vr.recenter"))]
    );
    // Ctrl let go first: the release still comes when f11 goes up.
    assert_eq!(poll(&mut t, &[F11], none()).len(), 0);
    assert_eq!(
        poll(&mut t, &[], none()),
        [(Edge::Release, hotkey("vr.recenter"))]
    );
}

#[test]
fn plus_binds_run_minus_on_release_and_focus_loss_releases() {
    let mut t = table();
    t.bind(c("f10"), "+fast");
    t.bind(c("ctrl+f10"), "set a.b 1; echo x");
    assert_eq!(
        poll(&mut t, &[F10], none()),
        [(Edge::Press, script("+fast"))]
    );
    assert_eq!(t.reset().len(), 1, "losing focus releases what is held");
    assert_eq!(
        poll(&mut t, &[F10], CTRL),
        [(Edge::Press, script("set a.b 1; echo x"))]
    );
    assert!(
        poll(&mut t, &[], none()).is_empty(),
        "nothing to run on release"
    );
    assert_eq!(release_of("+fast"), Some("-fast".to_owned()));
    assert_eq!(release_of(" +fast 2 "), Some("-fast".to_owned()));
    assert_eq!(release_of("+a; +b"), None);
    assert_eq!(release_of("+"), None);
}

#[test]
fn a_user_bind_takes_the_chord_and_unbind_is_remembered_by_entry() {
    let mut t = table();
    t.bind(c("f9"), "echo mine");
    assert_eq!(
        t.entry("vr.toggle").map(Entry::binding).as_deref(),
        Some("f12")
    );
    assert_eq!(
        poll(&mut t, &[F9], none()),
        [(Edge::Press, script("echo mine"))]
    );
    assert!(t.unbind(c("f9")));
    assert!(t.unbind(c("f12")), "a suggestion is unbound too");
    assert!(!t.unbind(c("f12")));
    assert_eq!(
        t.entry("vr.toggle").map(Entry::binding).as_deref(),
        Some("none")
    );
    let saved = t.save();
    assert!(saved.contains("rebind vr.toggle none\n"), "{saved}");

    // Next start: the plugin now suggests other keys, and the removal still holds.
    let mut next = Table::default();
    assert_eq!(next.load(&saved), Vec::<String>::new());
    let _ = next.register("vr.toggle", "Toggle", "f5", Target::Hotkey);
    assert_eq!(
        next.entry("vr.toggle").map(Entry::binding).as_deref(),
        Some("none")
    );
    assert_eq!(next.reset_entry("vr.toggle"), Ok(()));
    assert_eq!(
        next.entry("vr.toggle").map(Entry::binding).as_deref(),
        Some("f5")
    );
    assert!(next.save().lines().all(|l| !l.starts_with("rebind")));
}

#[test]
fn the_first_suggestion_keeps_a_chord_and_later_ones_are_shadowed() {
    let mut t = table();
    let warnings = t
        .register(
            "coop.menu",
            "Menu",
            "f9",
            Target::Script("coop.menu".into()),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        poll(&mut t, &[F9], none()),
        [(Edge::Press, hotkey("vr.toggle"))]
    );
    let rows = t.rows();
    assert!(rows
        .iter()
        .any(|r| r.origin == Origin::Shadowed("coop.menu".into())));
    // Moving the first one off f9 hands the chord to the next.
    t.rebind("vr.toggle", vec![c("f12")]);
    assert_eq!(
        t.lookup(c("f9")),
        Some((script("coop.menu"), Some("coop".to_owned())))
    );
    // A user bind registered before the plugin wins over its suggestion.
    let mut u = Table::default();
    u.bind(c("f12"), "echo user");
    let warnings = u
        .register("vr.toggle", "Toggle", "f12", Target::Hotkey)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        u.lookup(c("f12")).map(|(f, _)| f),
        Some(script("echo user"))
    );
}

#[test]
fn reset_chord_and_reset_all_and_unbindall() {
    let mut t = table();
    t.bind(c("f12"), "echo x");
    t.reset_chord(c("f12"));
    assert_eq!(
        t.lookup(c("f12")).map(|(f, _)| f),
        Some(hotkey("vr.toggle"))
    );
    assert_eq!(
        t.entry("vr.toggle").map(Entry::binding).as_deref(),
        Some("f9, f12")
    );
    t.unbind_all();
    assert!(t.lookup(c("f12")).is_none() && t.lookup(c("ctrl+f11")).is_none());
    t.reset_all();
    assert!(t.lookup(c("ctrl+f11")).is_some());
    assert_eq!(
        t.reset_entry("nope.x"),
        Err(BindError::Unknown("nope.x".into()))
    );
    assert!(matches!(
        t.register("vr.toggle", "", "f1", Target::Hotkey),
        Err(BindError::Duplicate(_))
    ));
    assert!(matches!(
        t.register("vr.bad", "", "nokey", Target::Hotkey),
        Err(BindError::Key(_))
    ));
}

#[test]
fn save_and_load_round_trip() {
    let mut t = table();
    t.bind(c("ctrl+f9"), "set plugin.setting value;reload");
    t.bind(c("a"), "exec plugin.command arg1 arg2");
    t.bind(c("f1"), r#"echo "two words""#);
    t.rebind("vr.recenter", vec![c("f11"), c("shift+f11")]);
    t.rebind("later.plugin", Vec::new());
    assert_eq!(t.alias("+fast", "set uniloader.speed 4"), Ok(()));
    assert_eq!(t.alias("bad.name", "x"), Err(BindError::AliasName));
    let saved = t.save();
    let mut back = Table::default();
    assert_eq!(back.load(&saved), Vec::<String>::new());
    let _ = back.register("vr.toggle", "Toggle", "f12, f9", Target::Hotkey);
    let _ = back.register("vr.recenter", "Recenter", "ctrl+f11", Target::Hotkey);
    assert_eq!(back.save(), saved);
    assert_eq!(back.alias_script("+fast"), Some("set uniloader.speed 4"));
    assert_eq!(
        back.lookup(c("CTRL+F9")).map(|(f, _)| f),
        Some(script("set plugin.setting value;reload"))
    );
    assert_eq!(
        back.lookup(c("f11")).map(|(f, _)| f),
        Some(hotkey("vr.recenter"))
    );
    assert!(
        saved.contains("rebind later.plugin none"),
        "kept for a plugin not loaded yet"
    );
    let warnings = back.load("echo hi\nbind\nbind f99 x\n");
    assert_eq!(warnings.len(), 3, "{warnings:?}");
}

#[test]
fn scan_code_presses_fire_once_with_no_release() {
    let mut t = Table::default();
    let _ = t.register("ui.console", "Console", "sc29", Target::Hotkey);
    let events = t.press(0x29, none());
    assert_eq!(events.len(), 1);
    assert_eq!(t.press(0x28, none()).len(), 0);
    assert_eq!(t.reset().len(), 0);
    assert_eq!(poll(&mut t, &[F12], none()), []);
}
