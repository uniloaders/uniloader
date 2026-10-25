//! The writer thread for settings and binds files.
//!
//! A setting is set from whichever thread asks: the game thread (a game UI's settings page), the
//! render thread (the overlay's editor), the console. A slider drag sets a new value every
//! frame, and writing the file there (a temporary file and a rename) was a disk write per
//! frame on the thread the game waits for. [`queue`] keeps only the newest [`Persist`] per file
//! and wakes one thread, which waits [`SETTLE`] for a drag's further frames before it writes.
//! [`flush`] writes whatever is still queued, for an orderly end (`quit`, the loader's stop).
//!
//! A process that is killed (a crash, `ExitProcess` without the loader's stop) loses the sets
//! of its last [`SETTLE`]; nothing can write them then, as other threads may hold the heap.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use crate::state::Persist;

/// How long the writer lets sets of the same moment pile up before writing.
const SETTLE: Duration = Duration::from_millis(250);

/// The newest unwritten version of each file.
static PENDING: Mutex<BTreeMap<PathBuf, Persist>> = Mutex::new(BTreeMap::new());
static WAKE: Condvar = Condvar::new();
/// Held by whoever writes a batch, so [`flush`] returns only after the writer's batch too.
static WRITING: Mutex<()> = Mutex::new(());
/// Whether the writer thread runs (`false`: it could not be started; writes are inline).
static WRITER: OnceLock<bool> = OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Write `persist` soon, on the writer thread; a newer version of the same file queued
/// before it is written replaces it.
pub fn queue(persist: Persist) {
    let running = *WRITER.get_or_init(|| {
        std::thread::Builder::new()
            .name("uniloader-persist".to_owned())
            .spawn(run)
            .inspect_err(|e| log::warn!("settings are written inline: no writer thread: {e}"))
            .is_ok()
    });
    if !running {
        persist.write();
        return;
    }
    newest(&mut lock(&PENDING), persist);
    WAKE.notify_one();
}

/// Keep `persist` in `pending` unless a newer version of its file is there already.
fn newest(pending: &mut BTreeMap<PathBuf, Persist>, persist: Persist) {
    match pending.get(persist.path()) {
        Some(queued) if queued.seq() > persist.seq() => {}
        _ => {
            pending.insert(persist.path().to_path_buf(), persist);
        }
    }
}

/// Write everything queued now, on this thread, and wait for a batch the writer has in hand.
pub fn flush() {
    let _writing = lock(&WRITING);
    let batch = std::mem::take(&mut *lock(&PENDING));
    for persist in batch.into_values() {
        persist.write();
    }
}

/// The writer thread: wait for work, let it settle, write it.
fn run() {
    loop {
        {
            let mut pending = lock(&PENDING);
            while pending.is_empty() {
                pending = WAKE.wait(pending).unwrap_or_else(PoisonError::into_inner);
            }
        }
        std::thread::sleep(SETTLE);
        flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_version_of_a_file_wins_whatever_order_it_comes_in() {
        let dir = std::env::temp_dir().join(format!("uniloader-persist-{}", std::process::id()));
        let mut state = crate::state::State::new(
            crate::config::Paths::resolve(&dir),
            crate::config::LoaderConfig::default(),
        );
        let older = state.save_binds();
        let newer = state.save_binds();
        assert_eq!(older.path(), newer.path());
        let mut pending = BTreeMap::new();
        newest(&mut pending, newer);
        newest(&mut pending, older);
        let kept = pending.values().next().map(Persist::seq);
        assert_eq!(kept, Some(2), "the later set stays queued");
    }

    #[test]
    fn a_flush_writes_what_is_queued() {
        let dir = std::env::temp_dir().join(format!("uniloader-flush-{}", std::process::id()));
        let mut state = crate::state::State::new(
            crate::config::Paths::resolve(&dir),
            crate::config::LoaderConfig::default(),
        );
        let save = state.save_binds();
        let path = save.path().to_path_buf();
        queue(save);
        flush();
        assert!(path.is_file(), "{} written", path.display());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
