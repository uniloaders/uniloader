//! Toasts, notices and modal dialogs: what a plugin puts on screen without owning a panel.
//!
//! Shared state rather than render-thread state, because a plugin asks from whatever thread
//! it is on and the overlay draws them on the next frame. A toast queued before the first
//! frame is not lost: its clock starts when it is first drawn.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use uniloader_overlay::{DialogFn, UiAnswer, UiDialog, UiLevel, UiNotice};
use uniloader_sdk::api::PluginHandle;

/// How long a toast stays when the caller does not say.
const TOAST_SECONDS: f32 = 5.0;
/// How long a notice stays when the caller does not say.
const NOTICE_SECONDS: f32 = 3.0;
/// How long the fade at the end takes.
const FADE: f32 = 0.6;

/// What was asked to be shown.
#[derive(Debug, Clone)]
pub(crate) enum Kind {
    /// A corner card or a centred message, with its lifetime in seconds.
    Passing(UiNotice, f32),
    /// A modal dialog, with the text being typed for an input one.
    Modal(UiDialog, String),
}

/// Where a dialog's answer goes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Answer {
    /// The owner's callback.
    pub(crate) callback: DialogFn,
    /// Its context, as an address so the entry stays `Send`.
    pub(crate) ctx: usize,
}

/// One thing on screen.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    /// Id handed back to the owner, unique for the process.
    pub(crate) id: u64,
    /// Who asked, or `None` for the overlay itself.
    pub(crate) owner: Option<PluginHandle>,
    pub(crate) kind: Kind,
    pub(crate) level: UiLevel,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) accept: String,
    pub(crate) cancel: String,
    /// Where a dialog's answer goes.
    pub(crate) answer: Option<Answer>,
    /// When it was first drawn.
    pub(crate) shown: Option<Instant>,
}

impl Entry {
    /// Whether this one takes the keyboard and mouse.
    pub(crate) fn is_modal(&self) -> bool {
        matches!(self.kind, Kind::Modal(..))
    }

    /// How opaque to draw it, fading out at the end of its life.
    pub(crate) fn opacity(&self) -> f32 {
        let Kind::Passing(_, seconds) = self.kind else {
            return 1.0;
        };
        let Some(shown) = self.shown else {
            return 1.0;
        };
        let left = seconds - shown.elapsed().as_secs_f32();
        (left / FADE).clamp(0.0, 1.0)
    }

    fn expired(&self) -> bool {
        match self.kind {
            Kind::Passing(_, seconds) => self
                .shown
                .is_some_and(|at| at.elapsed() > Duration::from_secs_f32(seconds)),
            Kind::Modal(..) => false,
        }
    }
}

/// What the owner of a dialog is told when it goes away.
#[derive(Debug, Clone)]
pub(crate) struct Answered {
    pub(crate) owner: PluginHandle,
    pub(crate) id: u64,
    pub(crate) answer: UiAnswer,
    pub(crate) text: String,
    pub(crate) callback: Answer,
}

static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Whether anything is queued, so a frame with nothing to draw costs one atomic read.
static ANY: AtomicBool = AtomicBool::new(false);
/// Whether a modal dialog is open, which makes the overlay take input.
static MODAL: AtomicBool = AtomicBool::new(false);

fn entries() -> MutexGuard<'static, Vec<Entry>> {
    ENTRIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn recount(list: &[Entry]) {
    ANY.store(!list.is_empty(), Ordering::Relaxed);
    MODAL.store(list.iter().any(Entry::is_modal), Ordering::Relaxed);
}

/// Whether anything is on screen.
pub(crate) fn any() -> bool {
    ANY.load(Ordering::Relaxed)
}

/// Whether a modal dialog is open.
pub(crate) fn modal() -> bool {
    MODAL.load(Ordering::Relaxed)
}

/// Queue a toast or a notice. Returns its id.
pub(crate) fn passing(
    owner: Option<PluginHandle>,
    kind: UiNotice,
    level: UiLevel,
    title: &str,
    text: &str,
    seconds: f32,
) -> u64 {
    let seconds = if seconds > 0.0 {
        seconds
    } else if kind == UiNotice::TOAST {
        TOAST_SECONDS
    } else {
        NOTICE_SECONDS
    };
    add(Entry {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        owner,
        kind: Kind::Passing(kind, seconds),
        level,
        title: title.to_owned(),
        text: text.to_owned(),
        accept: String::new(),
        cancel: String::new(),
        answer: None,
        shown: None,
    })
}

/// What a dialog says, as the owner asked for it.
pub(crate) struct DialogText<'a> {
    pub(crate) title: &'a str,
    pub(crate) text: &'a str,
    pub(crate) default_text: &'a str,
    pub(crate) accept: &'a str,
    pub(crate) cancel: &'a str,
}

/// Queue a modal dialog. Returns its id.
pub(crate) fn dialog(
    owner: PluginHandle,
    kind: UiDialog,
    level: UiLevel,
    text: &DialogText<'_>,
    answer: Option<Answer>,
) -> u64 {
    add(Entry {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        owner: Some(owner),
        kind: Kind::Modal(kind, text.default_text.to_owned()),
        level,
        title: text.title.to_owned(),
        text: text.text.to_owned(),
        accept: text.accept.to_owned(),
        cancel: text.cancel.to_owned(),
        answer,
        shown: None,
    })
}

fn add(entry: Entry) -> u64 {
    let id = entry.id;
    log::debug!(
        "ui {id} {} by {}: {:?} {:?}",
        describe(&entry.kind),
        entry
            .owner
            .map_or_else(|| "overlay".to_owned(), |h| h.0.to_string()),
        entry.title,
        entry.text
    );
    let mut list = entries();
    list.push(entry);
    recount(&list);
    id
}

fn describe(kind: &Kind) -> &'static str {
    match kind {
        Kind::Passing(UiNotice::TOAST, _) => "toast",
        Kind::Passing(_, _) => "notice",
        Kind::Modal(UiDialog::MESSAGE, _) => "message dialog",
        Kind::Modal(UiDialog::CONFIRM, _) => "confirm dialog",
        Kind::Modal(_, _) => "input dialog",
    }
}

/// A snapshot for the render thread, in the order things were asked for.
pub(crate) fn snapshot() -> Vec<Entry> {
    entries().clone()
}

/// Start the clock on everything in `ids` that had none, and drop whatever has run out.
pub(crate) fn mark_shown(ids: &[u64]) {
    let now = Instant::now();
    let mut list = entries();
    for entry in list.iter_mut() {
        if entry.shown.is_none() && ids.contains(&entry.id) {
            entry.shown = Some(now);
        }
    }
    list.retain(|entry| !entry.expired());
    recount(&list);
}

/// Store what was typed into an input dialog, so it survives to the next frame.
pub(crate) fn set_input(id: u64, text: &str) {
    let mut list = entries();
    if let Some(entry) = list.iter_mut().find(|e| e.id == id) {
        if let Kind::Modal(_, buffer) = &mut entry.kind {
            text.clone_into(buffer);
        }
    }
}

/// Take one entry out, whoever owns it, and say what to tell its owner.
pub(crate) fn take(id: u64, answer: UiAnswer) -> Option<Answered> {
    let mut list = entries();
    let index = list.iter().position(|e| e.id == id)?;
    let entry = list.remove(index);
    recount(&list);
    drop(list);
    answered(&entry, answer)
}

/// Take one entry out on behalf of its owner, refusing another plugin's id. `Err` when the
/// id is not the owner's; `Ok(None)` when it was not a dialog.
pub(crate) fn take_owned(owner: PluginHandle, id: u64) -> Result<Option<Answered>, ()> {
    let mut list = entries();
    let index = list
        .iter()
        .position(|e| e.id == id && e.owner == Some(owner))
        .ok_or(())?;
    let entry = list.remove(index);
    recount(&list);
    drop(list);
    Ok(answered(&entry, UiAnswer::CLOSED))
}

/// Drop everything one plugin owns, for when it stops. Its dialogs are not answered: the
/// plugin is gone.
pub(crate) fn take_all(owner: PluginHandle) {
    let mut list = entries();
    list.retain(|e| e.owner != Some(owner));
    recount(&list);
}

/// What to tell the owner of a dialog that is going away.
fn answered(entry: &Entry, answer: UiAnswer) -> Option<Answered> {
    let Kind::Modal(kind, buffer) = &entry.kind else {
        return None;
    };
    let text = if *kind == UiDialog::INPUT && answer == UiAnswer::ACCEPTED {
        buffer.clone()
    } else {
        String::new()
    };
    log::debug!("ui {} answered {} {text:?}", entry.id, answer.0);
    Some(Answered {
        owner: entry.owner?,
        id: entry.id,
        answer,
        text,
        callback: entry.answer?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn ignore(
        _ctx: *mut core::ffi::c_void,
        _id: u64,
        _answer: UiAnswer,
        _text: uniloader_sdk::api::Str,
    ) {
    }

    #[test]
    fn a_dialog_answers_once_and_only_its_owner_may_close_it() {
        let owner = PluginHandle(7);
        let words = DialogText {
            title: "t",
            text: "x",
            default_text: "typed",
            accept: "",
            cancel: "",
        };
        let callback = Answer {
            callback: ignore,
            ctx: 0,
        };
        let id = dialog(
            owner,
            UiDialog::INPUT,
            UiLevel::INFO,
            &words,
            Some(callback),
        );
        assert!(modal());
        assert!(take_owned(PluginHandle(8), id).is_err());
        let answered = take(id, UiAnswer::ACCEPTED);
        assert_eq!(
            answered.map(|a| (a.owner, a.text)),
            Some((owner, "typed".to_owned()))
        );
        assert!(take(id, UiAnswer::ACCEPTED).is_none(), "already gone");
    }
}
