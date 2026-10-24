//! The player's side of crash reports: the loader says a report is ready
//! ([`topics::REPORT_READY`]), the overlay asks whether to send it to the mod authors or save
//! it to the Desktop, and answers with the loader's `report send` / `report desktop`
//! commands; what became of it ([`topics::REPORT_DONE`]) is a toast.

// The dialog callback is an ABI function the overlay hands to itself.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::sync::Mutex;

use uniloader_overlay::{UiAnswer, UiDialog, UiLevel, UiNotice};
use uniloader_sdk::api::Str;
use uniloader_sdk::{topics, Host};

use crate::notices;

/// How long the toast about a report stays.
const DONE_SECONDS: f32 = 12.0;

/// The open prompts: zip name and dialog id, so a report decided elsewhere (the console)
/// takes its prompt down.
static OPEN: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

fn open() -> std::sync::MutexGuard<'static, Vec<(String, u64)>> {
    OPEN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The fields of a NUL separated payload.
fn fields(payload: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(payload)
        .split('\0')
        .map(str::to_owned)
        .collect()
}

/// Handles the two report topics; `false` for any other topic.
pub(crate) fn on_event(host: &Host, topic: &str, payload: &[u8]) -> bool {
    if topic == topics::REPORT_READY {
        ready(host, &fields(payload));
        return true;
    }
    if topic == topics::REPORT_DONE {
        done(host, &fields(payload));
        return true;
    }
    false
}

fn ready(host: &Host, f: &[String]) {
    let get = |i: usize| f.get(i).map_or("", String::as_str);
    let (name, headline, server, saved) = (get(0), get(2), get(3), get(4));
    if name.is_empty() {
        return;
    }
    let what = "It holds crash dumps and the game's and mods' logs, with IP addresses, \
                invites, ids and passwords removed.";
    if !saved.is_empty() {
        let text = format!(
            "{what}\n\nIt was saved to your Desktop:\n{saved}\n\nSend it to the mod's author \
             to help fix this."
        );
        notices::dialog(
            host.me().raw(),
            UiDialog::MESSAGE,
            UiLevel::WARNING,
            &words(headline, &text, "", ""),
            None,
        );
        crate::shared::recount();
        return;
    }
    let (text, accept, cancel) = if server.is_empty() {
        (
            format!("{what}\n\nSave it to your Desktop to send it to the mod's author?"),
            "Save to Desktop",
            "Not now",
        )
    } else {
        (
            format!(
                "{what}\n\nSend it to the mod authors ({server}), or save it to your \
                 Desktop and send it yourself?"
            ),
            "Send report",
            "Save to Desktop",
        )
    };
    // The zip's name rides along as the callback's context; one small leak per report.
    let ctx = Box::into_raw(Box::new(Prompt {
        name: name.to_owned(),
        can_send: !server.is_empty(),
    }));
    let id = notices::dialog(
        host.me().raw(),
        UiDialog::CONFIRM,
        UiLevel::WARNING,
        &words(headline, &text, accept, cancel),
        Some(notices::Answer {
            callback: answered,
            ctx: ctx as usize,
        }),
    );
    open().push((name.to_owned(), id));
    crate::shared::recount();
}

fn words<'a>(
    title: &'a str,
    text: &'a str,
    accept: &'a str,
    cancel: &'a str,
) -> notices::DialogText<'a> {
    notices::DialogText {
        title,
        text,
        default_text: "",
        accept,
        cancel,
        remember: "",
    }
}

/// What a report dialog's answer acts on.
struct Prompt {
    name: String,
    can_send: bool,
}

/// The report dialog's answer: send, save, or (closed) leave it for the next start.
unsafe extern "C" fn answered(ctx: *mut c_void, _id: u64, answer: UiAnswer, _text: Str) {
    // SAFETY: `ctx` is the `Prompt` leaked in `ready`, read here once and never freed.
    let Some(prompt) = (unsafe { (ctx as *const Prompt).as_ref() }) else {
        return;
    };
    let verb = match (answer, prompt.can_send) {
        (UiAnswer::ACCEPTED, true) => "send",
        (UiAnswer::ACCEPTED, false) | (UiAnswer::CANCELLED, true) => "desktop",
        _ => return,
    };
    if let Some(host) = crate::host() {
        let line = format!("uniloader.report {verb} {}", prompt.name);
        if let Err(e) = host.console_exec(&line) {
            log::warn!("{line}: {e}");
        }
    }
}

fn done(host: &Host, f: &[String]) {
    let get = |i: usize| f.get(i).map_or("", String::as_str);
    let prompts: Vec<u64> = {
        let mut open = open();
        let ids = open
            .iter()
            .filter(|(n, _)| n == get(0))
            .map(|&(_, id)| id)
            .collect();
        open.retain(|(n, _)| n != get(0));
        ids
    };
    for id in prompts {
        // Closed, which the callback ignores; already gone when the player answered it.
        let _ = notices::take_owned(host.me().raw(), id);
    }
    let (level, title, text) = match get(1) {
        "sent" => (
            UiLevel::SUCCESS,
            "Report sent, thank you",
            format!(
                "Report id {}: give it to the mod's author when you tell them.",
                get(2)
            ),
        ),
        "saved" => (
            UiLevel::INFO,
            "Report saved",
            format!("Saved to {}", get(2)),
        ),
        _ => (UiLevel::WARNING, "Report not sent", get(2).to_owned()),
    };
    notices::passing(None, UiNotice::NOTICE, level, title, &text, DONE_SECONDS);
    // A hidden overlay draws nothing until told it has something to show.
    crate::shared::recount();
}
