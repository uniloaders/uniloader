//! The kitchen sink's use of the overlay service: a panel with one of every widget, a toast,
//! a dialog closed by code and a present subscriber.
//!
//! Optional: without the overlay plugin the kitchen sink runs as before. With it, the log
//! says when the panel body was first drawn, when the first present was seen and how the
//! dialog ended, which is what the smoke test looks for.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use uniloader_overlay::{Card, Overlay, Ui, UiDialog, UiLevel};
use uniloader_sdk::api::Status;
use uniloader_sdk::{Host, PluginError};

/// Frames the panel body was filled in.
static FRAMES: AtomicU32 = AtomicU32::new(0);
/// Presents seen.
static PRESENTS: AtomicU32 = AtomicU32::new(0);
/// The panel's own checkbox, slider and toggles.
static CHECKED: AtomicBool = AtomicBool::new(true);
static SLIDER: AtomicU32 = AtomicU32::new(0);
static BITS: AtomicU32 = AtomicU32::new(0b101);

/// Connect and put everything up. `None` when no overlay is loaded.
pub(crate) fn attach(host: &Host) -> Option<Overlay> {
    let Some(overlay) = Overlay::connect(host) else {
        log::info!("no overlay service; its checks are skipped");
        return None;
    };
    if let Err(e) = overlay.panel("panel", "Kitchen sink", true, draw) {
        log::error!("could not add the kitchen panel: {e}");
    }
    if let Err(e) = overlay.toast(
        UiLevel::SUCCESS,
        "Kitchen sink",
        "Connected to the overlay.",
    ) {
        log::error!("could not show a toast: {e}");
    }
    let card = Card {
        badge: "KS",
        title: "Kitchen sink",
        text: "every plugin API, tried at start",
        icon: "",
        level: UiLevel::INFO,
    };
    match overlay.card("kitchen", &card) {
        Ok(()) => log::info!("kitchen card in the showcase"),
        Err(e) => log::info!("no showcase card: {e}"),
    }
    let presents = overlay.on_present(|info| {
        if PRESENTS.fetch_add(1, Ordering::Relaxed) == 0 {
            log::info!("kitchen saw a present {}x{}", info.width, info.height);
        }
    });
    match presents {
        Ok(()) => {}
        // An overlay without a frame hook (tsw_ui draws with the game's own menus).
        Err(PluginError::Status(Status::UNSUPPORTED)) => {
            log::info!("this overlay has no presents; that check is skipped");
        }
        Err(e) => log::error!("could not subscribe to presents: {e}"),
    }
    // Opened and closed straight away: the answer arrives on the next frame, as `CLOSED`.
    let dialog = overlay.dialog(
        UiDialog::CONFIRM,
        UiLevel::INFO,
        "Kitchen sink",
        "Closed by code before anyone could answer.",
        "",
        |answer, _| log::info!("kitchen dialog answered {}", answer.0),
    );
    match dialog {
        Ok(id) => {
            if let Err(e) = overlay.close(id) {
                log::error!("could not close the dialog: {e}");
            }
        }
        Err(e) => log::error!("could not open a dialog: {e}"),
    }
    Some(overlay)
}

/// The panel body: one of every widget.
fn draw(ui: &mut Ui<'_>) {
    let frame = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if frame == 1 {
        log::info!("kitchen panel drawn");
    }
    ui.heading("Kitchen sink");
    ui.label(&format!("Frame {frame}"));
    ui.separator();
    ui.setting("level");
    ui.setting("mode");
    ui.setting("motto");
    ui.space(0.0);
    let mut checked = CHECKED.load(Ordering::Relaxed);
    if ui.checkbox("A checkbox", &mut checked) {
        CHECKED.store(checked, Ordering::Relaxed);
    }
    let mut slider = f32::from_bits(SLIDER.load(Ordering::Relaxed));
    if ui.slider("A slider", &mut slider, 0.0, 1.0) {
        SLIDER.store(slider.to_bits(), Ordering::Relaxed);
    }
    let mut level = 3;
    ui.slider_int("An integer slider", &mut level, 0, 10);
    let mut bits = i64::from(BITS.load(Ordering::Relaxed));
    if ui.toggles("Toggles", &mut bits, 4, 0.0) {
        BITS.store(u32::try_from(bits).unwrap_or(0), Ordering::Relaxed);
    }
    if ui.button("Log a line") {
        log::info!("kitchen button clicked");
    }
}

/// Checks that need no frame: the panel registry through the service, and the overlay's
/// console command. In the smoke test host the console and the settings editor are left
/// open, so its captured frame shows them; in a game they are closed again.
pub(crate) fn checks(check: &mut dyn FnMut(&str, bool), overlay: &Overlay, host: &Host) {
    // Whichever plugin provides the service: the egui `overlay`, or a game's own UI. Both
    // have a `toggle` command for their console and settings screens.
    let provider = host
        .modules()
        .into_iter()
        .find(|m| m.plugin == Some(overlay.provider()))
        .map_or_else(|| "overlay".to_owned(), |m| m.name);
    check(
        "overlay opens its console by command",
        host.console_exec(&format!("{provider}.toggle console"))
            .is_ok(),
    );
    check(
        "overlay opens its settings editor by command",
        host.console_exec(&format!("{provider}.toggle settings"))
            .is_ok(),
    );
    check(
        "overlay refuses to toggle nonsense",
        host.console_exec(&format!("{provider}.toggle nonsense"))
            .is_err(),
    );
    check("overlay panel starts open", overlay.is_open("panel"));
    check(
        "overlay panel toggles",
        overlay.toggle("panel").is_ok() && !overlay.is_open("panel"),
    );
    check(
        "overlay panel reopens",
        overlay.set_open("panel", true).is_ok() && overlay.is_open("panel"),
    );
    check(
        "overlay refuses a duplicate panel",
        overlay.panel("panel", "again", false, |_| {}).is_err(),
    );
    check(
        "overlay refuses a bad panel name",
        overlay.panel("Bad Name", "bad", false, |_| {}).is_err(),
    );
    check(
        "overlay unknown panel",
        overlay.toggle("missing").is_err() && !overlay.is_open("missing"),
    );
    check(
        "overlay panel removed and re-added",
        overlay.remove_panel("panel").is_ok()
            && overlay.panel("panel", "Kitchen sink", true, draw).is_ok(),
    );
    check(
        "overlay close refuses an unknown id",
        overlay.close(u64::MAX).is_err(),
    );
    // In a game they would cover the screen at every start: closed again.
    if !in_smoke_host() {
        let _ = host.console_exec(&format!("{provider}.toggle console"));
        let _ = host.console_exec(&format!("{provider}.toggle settings"));
        let _ = overlay.set_open("panel", false);
    }
}

/// Whether this is uniloader's smoke test host, whose captured frame shows the windows.
fn in_smoke_host() -> bool {
    std::env::current_exe().is_ok_and(|exe| {
        exe.file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("smoke-host.exe"))
    })
}
