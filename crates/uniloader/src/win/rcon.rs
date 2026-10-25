//! The live RCON server: started from the `uniloader.rcon*` settings at startup and
//! restarted whenever one of them changes. Scripts run through the command buffer as
//! [`Source::Rcon`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use uniloader_core::script;

use crate::console::{Ctx, Source};
use crate::rcon::{self, Exec, Server};
use crate::speed::NAME;

use super::{exec, state};

static SERVER: Mutex<Option<Server>> = Mutex::new(None);

/// [`reconfigure`] on a thread of its own. Two of them in a row are fine: each reads the
/// settings when it runs and they take turns on [`SERVER`].
pub(super) fn reconfigure_later() {
    let spawned = std::thread::Builder::new()
        .name("uniloader-rcon-config".to_owned())
        .spawn(reconfigure);
    if let Err(e) = spawned {
        log::warn!("rcon: no thread to restart on ({e}); restarting here");
        reconfigure();
    }
}

/// Stop the server if it runs, then start it again if the settings say so.
pub(super) fn reconfigure() {
    let (values, audit) = {
        let guard = state();
        let values: BTreeMap<String, String> = rcon::settings()
            .into_iter()
            .filter_map(|d| {
                let value = guard.get_setting(None, &format!("{NAME}.{}", d.key)).ok()?;
                Some((d.key, value))
            })
            .collect();
        (values, guard.paths.logs_dir.join("rcon.log"))
    };
    let mut server = SERVER.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(old) = server.take() {
        old.stop();
    }
    let config = match rcon::config(&values, audit) {
        Ok(config) => config,
        Err(why) => {
            log::info!("rcon off: {why}");
            return;
        }
    };
    let run: Arc<Exec> = Arc::new(|text: &str| {
        let ctx = Ctx::from(Source::Rcon);
        let logged = {
            let guard = state();
            script::commands(text)
                .unwrap_or_default()
                .iter()
                .map(|command| crate::secret::redact(&guard, &ctx, command))
                .collect::<Vec<String>>()
                .join("; ")
        };
        let (_, lines) = exec::run_script(ctx, text);
        (lines, logged)
    });
    match Server::start(config, run) {
        Ok(started) => {
            if !started.addr().ip().is_loopback() {
                log::warn!(
                    "rcon listens on {}: anyone who can reach it and has the password controls \
                     the game; the password travels in clear text",
                    started.addr()
                );
            }
            *server = Some(started);
        }
        Err(e) => log::error!("rcon: {e}"),
    }
}
