//! The command buffer: every script the loader runs goes through it, so a key bind, a typed
//! line, a plugin's `console_exec` and an RCON command never interleave, and `wait` has
//! something to come back to.
//!
//! One execution lock serialises runs. A synchronous caller (a plugin, the console window,
//! RCON) runs its script itself when the lock is free, and otherwise queues it and waits up to
//! [`SYNC_TIMEOUT`] for the input thread to run it. A call from inside a run (a plugin
//! command calling `console_exec`) runs inline instead of queueing, so it cannot deadlock on
//! the run that called it. This lock is the one the loader holds across plugin calls; the
//! inline rule and the timeout are what keep that safe. Key binds and the rest of a script
//! after a `wait` are queued and run by the input thread, which [`Buffer::wait_for_work`]
//! wakes as soon as something is due.
//!
//! A `wait` parks only the rest of its own script; other scripts keep running meanwhile.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Condvar, Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use uniloader_api::Status;

use crate::run::{self, Effects, Job, Ran};

/// The answer to a caller the console kept waiting too long.
fn busy() -> (Status, Vec<String>) {
    (
        Status::ERROR,
        vec![format!(
            "the console was busy for {} s; the script runs when it is free",
            SYNC_TIMEOUT.as_secs()
        )],
    )
}

/// How long a synchronous caller waits for a busy console before giving up on the answer.
/// The script still runs when the console is free.
pub const SYNC_TIMEOUT: Duration = Duration::from_secs(5);

thread_local! {
    /// Whether this thread is inside a run, holding the execution lock.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
    /// Whether this thread drains the queue (the input thread): queueing a script from here
    /// would wait on itself.
    static DRAINS: Cell<bool> = const { Cell::new(false) };
}

/// How often the draining thread looks whether the console came free.
const RETRY: Duration = Duration::from_millis(1);

/// A queued script.
struct Pending {
    due: Instant,
    job: Job,
    done: Option<mpsc::Sender<(Status, Vec<String>)>>,
}

/// The loader's one command buffer.
pub struct Buffer {
    queue: Mutex<VecDeque<Pending>>,
    wake: Condvar,
    exec: Mutex<()>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Marks the thread as inside a run for as long as it lives.
struct Inside;

impl Inside {
    fn enter() -> Self {
        INSIDE.with(|inside| inside.set(true));
        Inside
    }
}

impl Drop for Inside {
    fn drop(&mut self) {
        INSIDE.with(|inside| inside.set(false));
    }
}

impl Buffer {
    /// An empty buffer.
    #[must_use]
    pub const fn new() -> Self {
        Buffer {
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            exec: Mutex::new(()),
        }
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, VecDeque<Pending>> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, pending: Pending) {
        self.queue().push_back(pending);
        self.wake.notify_all();
    }

    /// Run `job` and return its status and output, running it here when the console is free
    /// or this thread is already inside a run, and through the queue otherwise.
    pub fn submit(&self, effects: &impl Effects, job: Job) -> (Status, Vec<String>) {
        if INSIDE.with(Cell::get) {
            return self.run_here(effects, job);
        }
        if let Some(_lock) = self.try_exec() {
            let _inside = Inside::enter();
            return self.run_here(effects, job);
        }
        if DRAINS.with(Cell::get) {
            // A plugin's hotkey callback on the input thread: only this thread runs the queue,
            // so wait for the console itself rather than for a drain that cannot come.
            let deadline = Instant::now() + SYNC_TIMEOUT;
            while Instant::now() < deadline {
                std::thread::sleep(RETRY);
                if let Some(_lock) = self.try_exec() {
                    let _inside = Inside::enter();
                    return self.run_here(effects, job);
                }
            }
            self.post(job);
            return busy();
        }
        let (done, answer) = mpsc::channel();
        self.push(Pending {
            due: Instant::now(),
            job,
            done: Some(done),
        });
        answer.recv_timeout(SYNC_TIMEOUT).unwrap_or_else(|_| busy())
    }

    /// The execution lock if it is free. A run that panicked holding it leaves nothing to
    /// guard (the lock holds no data), so a poisoned lock counts as free.
    fn try_exec(&self) -> Option<std::sync::MutexGuard<'_, ()>> {
        match self.exec.try_lock() {
            Ok(lock) => Some(lock),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Queue `job` for the input thread, for a key bind.
    pub fn post(&self, job: Job) {
        self.push(Pending {
            due: Instant::now(),
            job,
            done: None,
        });
    }

    fn run_here(&self, effects: &impl Effects, job: Job) -> (Status, Vec<String>) {
        let ran = run::run(effects, job);
        self.finish(effects, ran)
    }

    /// Hand the output on, and park the rest of a script that waits.
    fn finish(&self, effects: &impl Effects, ran: Ran) -> (Status, Vec<String>) {
        effects.printed(&ran.printed);
        if let Some((ms, rest)) = ran.rest {
            self.push(Pending {
                due: Instant::now() + Duration::from_millis(ms),
                job: rest,
                done: None,
            });
        }
        (ran.status, ran.printed)
    }

    /// Run every queued script that is due. Called by the input thread every tick.
    pub fn drain(&self, effects: &impl Effects) {
        DRAINS.with(|drains| drains.set(true));
        let _lock = self.exec.lock().unwrap_or_else(PoisonError::into_inner);
        let _inside = Inside::enter();
        loop {
            let next = {
                let mut queue = self.queue();
                let now = Instant::now();
                let due = queue.iter().position(|p| p.due <= now);
                due.and_then(|i| queue.remove(i))
            };
            let Some(pending) = next else {
                return;
            };
            let ran = run::run(effects, pending.job);
            let answer = self.finish(effects, ran);
            if let Some(done) = pending.done {
                // The caller may have timed out and gone; then nobody wants the answer.
                let _ = done.send(answer);
            }
        }
    }

    /// Block until a queued script is due, or `at_most` passed.
    pub fn wait_for_work(&self, at_most: Duration) {
        let queue = self.queue();
        let now = Instant::now();
        let until_due = queue
            .iter()
            .map(|p| p.due.saturating_duration_since(now))
            .min()
            .unwrap_or(at_most)
            .min(at_most);
        if until_due.is_zero() {
            return;
        }
        let _ = self
            .wake
            .wait_timeout(queue, until_due)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::console::{Ctx, Source};
    use crate::run::tests::fake;

    fn job(text: &str) -> Job {
        Job::new(Ctx::from(Source::Console), text).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn a_free_console_runs_the_script_at_once_and_parks_what_waits() {
        let (f, _) = fake();
        let buffer = Buffer::new();
        let (status, printed) = buffer.submit(&f, job("echo a; wait 0; echo b"));
        assert_eq!((status, printed), (Status::OK, vec!["a".to_owned()]));
        buffer.drain(&f);
        let tail = f.with_state(|s| s.console.back().map(|l| l.text.clone()));
        assert_eq!(tail.as_deref(), Some("b"));
    }

    #[test]
    fn a_wait_is_not_due_before_its_time() {
        let (f, _) = fake();
        let buffer = Buffer::new();
        buffer.submit(&f, job("wait 60000; echo late"));
        buffer.post(job("echo now"));
        let started = Instant::now();
        buffer.wait_for_work(Duration::from_secs(2));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the post is due"
        );
        buffer.drain(&f);
        let lines = f.with_state(|s| s.console_since(0));
        assert!(lines.contains(&"now".to_owned()));
        assert!(!lines.contains(&"late".to_owned()));
    }

    #[test]
    fn a_busy_console_queues_and_the_drain_answers() {
        let (f, _) = fake();
        let f = Arc::new(f);
        let buffer = Arc::new(Buffer::new());
        // Hold the execution lock as a long-running script would.
        let lock = buffer.exec.lock().unwrap_or_else(PoisonError::into_inner);
        let caller = {
            let (f, buffer) = (Arc::clone(&f), Arc::clone(&buffer));
            std::thread::spawn(move || buffer.submit(&*f, job("echo queued")))
        };
        // The caller queues; wait until it has, then let the "drain" run it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while buffer.queue().is_empty() && Instant::now() < deadline {
            buffer.wait_for_work(Duration::from_millis(10));
        }
        drop(lock);
        buffer.drain(&*f);
        let answer = caller.join().unwrap_or_else(|_| panic!("caller panicked"));
        assert_eq!(answer, (Status::OK, vec!["queued".to_owned()]));
    }

    #[test]
    fn the_draining_thread_waits_for_the_console_not_for_itself() {
        let (f, _) = fake();
        let buffer = Arc::new(Buffer::new());
        buffer.drain(&f); // this thread is the input thread now
        let (held, release) = (mpsc::channel(), mpsc::channel::<()>());
        let holder = {
            let buffer = Arc::clone(&buffer);
            std::thread::spawn(move || {
                let _lock = buffer.exec.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = held.0.send(());
                let _ = release.1.recv_timeout(Duration::from_secs(5));
            })
        };
        held.1
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|e| panic!("holder: {e}"));
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let _ = release.0.send(());
        });
        let started = Instant::now();
        let answer = buffer.submit(&f, job("echo hotkey"));
        assert_eq!(answer, (Status::OK, vec!["hotkey".to_owned()]));
        assert!(started.elapsed() < Duration::from_secs(2), "ran once free");
        for t in [holder, releaser] {
            t.join().unwrap_or_else(|_| panic!("helper panicked"));
        }
    }

    #[test]
    fn a_poisoned_console_still_runs_at_once() {
        let (f, _) = fake();
        let buffer = Arc::new(Buffer::new());
        let poisoner = {
            let buffer = Arc::clone(&buffer);
            std::thread::spawn(move || {
                let _lock = buffer.exec.lock().unwrap_or_else(PoisonError::into_inner);
                panic!("a run that panicked");
            })
        };
        assert!(poisoner.join().is_err());
        assert!(buffer.exec.is_poisoned());
        let answer = buffer.submit(&f, job("echo after"));
        assert_eq!(answer, (Status::OK, vec!["after".to_owned()]));
    }
}
