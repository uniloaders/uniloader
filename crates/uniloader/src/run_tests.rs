//! Tests for [`crate::run`], with a fake platform layer that records what it was asked.

use std::sync::Mutex;

use uniloader_api::PluginHandle;

use super::*;
use crate::console::Source;
use crate::state::tests::{desc, started, state};
use crate::state::{CommandInfo, Phase};

/// A platform layer that records plugin commands and hotkeys instead of calling DLLs.
pub(crate) struct Fake {
    pub(crate) state: Mutex<State>,
    pub(crate) calls: Mutex<Vec<String>>,
}

impl Effects for Fake {
    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    fn perform(&self, outcome: Outcome) -> (Status, Vec<String>) {
        let mut calls = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for persist in outcome.persist {
            persist.write();
        }
        if let Some((_, name, args)) = outcome.command {
            calls.push(format!("command {name} {args}").trim_end().to_owned());
        }
        if let Some(action) = outcome.hotkey {
            calls.push(format!("hotkey {action}"));
        }
        (outcome.status, Vec::new())
    }
}

impl Fake {
    pub(crate) fn run(&self, text: &str) -> Ran {
        match Job::new(Ctx::from(Source::Console), text) {
            Ok(job) => run(self, job),
            Err(e) => panic!("{text}: {e}"),
        }
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        std::mem::take(
            &mut self
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    pub(crate) fn config_dir(&self) -> std::path::PathBuf {
        self.with_state(|s| s.paths.config_dir.clone())
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let dir = self.with_state(|s| s.paths.game_dir.clone());
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A state with a plugin `plugin` that has the setting `setting`, the command `command` and
/// the hotkey action `menu` on f9.
pub(crate) fn fake() -> (Fake, PluginHandle) {
    let mut s = state();
    let h = started(&mut s, "plugin");
    s.register_setting(h, desc("setting"))
        .unwrap_or_else(|e| panic!("{e:?}"));
    let info = CommandInfo {
        help: "A command.".into(),
        usage: String::new(),
    };
    s.register_command(h, "command", info)
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.register_hotkey(h, "menu", "Menu", "f9")
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.phase = Phase::Running;
    s.set_enabled(h, true);
    let fake = Fake {
        state: Mutex::new(s),
        calls: Mutex::new(Vec::new()),
    };
    (fake, h)
}

#[test]
fn several_commands_run_in_order_with_plugin_calls_between() {
    let (f, _) = fake();
    let ran = f.run("set plugin.setting 3; plugin.command a \"b c\"; plugin.setting");
    assert_eq!(ran.status, Status::OK);
    assert_eq!(ran.printed, ["plugin.setting = 3", "plugin.setting = 3"]);
    assert_eq!(f.calls(), [r#"command command a "b c""#]);
}

#[test]
fn the_first_failure_is_the_status_and_the_rest_still_runs() {
    let (f, _) = fake();
    let ran = f.run("nope.x; set plugin.setting 4");
    assert_eq!(ran.status, Status::NOT_FOUND);
    assert_eq!(
        f.with_state(|s| s.get_setting(None, "plugin.setting")),
        Ok("4".to_owned())
    );
}

#[test]
fn exec_runs_a_cfg_file_or_else_the_command() {
    let (f, _) = fake();
    let dir = f.config_dir();
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(
        dir.join("fast.cfg"),
        "set plugin.setting 7 // note\necho done\n",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let ran = f.run("exec fast");
    assert_eq!(ran.printed, ["plugin.setting = 7", "done"]);
    // The owner's example: no cfg of that name, so the command runs with its arguments.
    let ran = f.run("exec plugin.command arg1 arg2");
    assert_eq!(ran.status, Status::OK);
    assert_eq!(f.calls(), ["command command arg1 arg2"]);
    assert_eq!(f.run("exec ../secret").status, Status::INVALID_ARGUMENT);
    assert_eq!(f.run(r"exec C:\x").status, Status::INVALID_ARGUMENT);
}

#[test]
fn aliases_expand_and_runaway_ones_stop() {
    let (f, _) = fake();
    let ran = f.run(r#"alias greet "echo hi; echo there"; greet"#);
    assert_eq!(ran.printed.last().map(String::as_str), Some("there"));
    let ran = f.run(r#"alias loop "loop"; loop"#);
    assert_eq!(ran.status, Status::ERROR);
    assert!(ran.printed.iter().any(|l| l.contains("nested deeper")));
    let ran = f.run(r#"alias wide "echo a; wide; wide"; wide"#);
    assert_eq!(ran.status, Status::ERROR);
    assert_eq!(f.run(r#"alias set "x""#).status, Status::INVALID_ARGUMENT);
}

#[test]
fn wait_hands_the_rest_back() {
    let (f, _) = fake();
    let ran = f.run("echo one; wait 250; echo two; echo three");
    assert_eq!(ran.printed, ["one"]);
    let Some((ms, rest)) = ran.rest else {
        panic!("rest of the script");
    };
    assert_eq!(ms, 250);
    let ran = run(&f, rest);
    assert_eq!(ran.printed, ["two", "three"]);
    assert!(ran.rest.is_none());
    assert!(
        f.run("echo x; wait").rest.is_none(),
        "nothing left after it"
    );
}

#[test]
fn a_dotted_hotkey_action_fires_and_a_command_of_that_name_wins() {
    let (f, _) = fake();
    f.run("plugin.menu");
    assert_eq!(f.calls(), ["hotkey plugin.menu"]);
    f.run("action plugin.menu");
    assert_eq!(f.calls(), ["hotkey plugin.menu"]);
    assert_eq!(f.run("action plugin.nope").status, Status::NOT_FOUND);
}
