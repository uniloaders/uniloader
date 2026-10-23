//! Whether the game is running, from the executable name the loader recorded.
//!
//! There is no event for "a process started" that works the same on Windows and Linux, so
//! the editor asks when it opens an install and when the user saves, not on a timer.

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Whether a process runs `exe` (a file name such as `TrainSimWorld.exe`). Under Wine or
/// Proton the Windows path is in the command line, so arguments count too.
#[must_use]
pub fn is_running(exe: &str) -> bool {
    if exe.is_empty() {
        return false;
    }
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet),
    );
    system.processes().values().any(|process| {
        let exe_path = process
            .exe()
            .map(|p| p.to_string_lossy().into_owned())
            .into_iter();
        let args = process
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned());
        exe_path.chain(args).any(|text| names(&text, exe))
    })
}

/// Whether `path` (a Windows or Unix path) names the file `exe`, ignoring case.
fn names(path: &str, exe: &str) -> bool {
    path.rsplit(['/', '\\'])
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case(exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_of_either_kind_name_the_executable() {
        assert!(names(
            "Z:\\games\\TSW\\TrainSimWorld.exe",
            "TrainSimWorld.exe"
        ));
        assert!(names("/games/tsw/trainsimworld.EXE", "TrainSimWorld.exe"));
        assert!(names("TrainSimWorld.exe", "TrainSimWorld.exe"));
        assert!(!names("/games/TrainSimWorld.exe.log", "TrainSimWorld.exe"));
        assert!(!names("-log", "TrainSimWorld.exe"));
    }

    #[test]
    fn this_test_finds_itself_and_not_a_made_up_game() {
        let me = std::env::current_exe().unwrap_or_else(|e| panic!("{e}"));
        let name = me
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert!(is_running(&name), "{name}");
        assert!(!is_running("no-such-game-4711.exe"));
        assert!(!is_running(""));
    }
}
