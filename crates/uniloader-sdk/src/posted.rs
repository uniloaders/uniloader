//! [`crate::Host::console_post`]: one thread per plugin that runs posted console lines.

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, PoisonError};

use crate::Host;

/// The running thread's queue; `None` until the first line, or after the thread ended.
static QUEUE: Mutex<Option<Sender<String>>> = Mutex::new(None);

/// Queue `line` for the console thread, starting it on first use.
pub(crate) fn post(host: Host, line: &str) {
    let mut queue = QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
    let line = match queue.as_ref().map(|q| q.send(line.to_owned())) {
        Some(Ok(())) => return,
        // The thread ended (it panicked on a line): a new one takes over.
        Some(Err(unsent)) => unsent.0,
        None => line.to_owned(),
    };
    let (sender, lines) = mpsc::channel::<String>();
    let started = crate::guard::spawn("console-post", move || {
        for line in lines {
            if let Err(e) = host.console_exec(&line) {
                log::warn!("{line}: {e}");
            }
        }
    });
    match started {
        Ok(_) => {
            // The receiver lives in the thread just started.
            let _ = sender.send(line);
            *queue = Some(sender);
        }
        Err(e) => log::warn!("{line}: no console thread ({e})"),
    }
}
