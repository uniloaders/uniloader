//! The RCON server: Source RCON over TCP, so `mcrcon`, `rcon-cli`, ARRCON and Python's `rcon`
//! work unchanged. Whoever holds the password runs console scripts as if typed in the game,
//! which includes everything plugins can do, so it is off by default, listens on loopback by
//! default and always needs a password.
//!
//! Limits: four connections at once; an unauthenticated connection closes after 10 s, an idle
//! one after 15 min; three failed logins from one address within 10 min ban it for 15 min; a
//! failed login is answered after 1 s; 20 commands per second per connection. Every login and
//! script is written to `logs/rcon.log` (scripts redacted, output never).
//!
//! Platform independent: the loader passes in what runs a script.

use std::collections::BTreeMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use source_rcon::{Packet, AUTH, AUTH_FAILED_ID, AUTH_RESPONSE, EXEC_COMMAND, RESPONSE_VALUE};
use uniloader_core::settings::{Desc, Kind};

/// The setting keys, under the loader's own entry `uniloader`.
pub mod keys {
    /// Master switch.
    pub const ENABLED: &str = "rcon";
    /// Address to listen on.
    pub const BIND: &str = "rcon_bind";
    /// TCP port.
    pub const PORT: &str = "rcon_port";
    /// Password, stored hashed.
    pub const PASSWORD: &str = "rcon_password";
    /// Addresses allowed to connect.
    pub const ALLOW: &str = "rcon_allow";
}

/// The default port: next to the DayZ debug plugin's 48621, clear of Source's 27015.
pub const DEFAULT_PORT: u16 = 48622;
/// Shortest password accepted.
pub const MIN_PASSWORD: usize = 12;

const MAX_CONNECTIONS: usize = 4;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_FAILURES: usize = 3;
const FAILURE_WINDOW: Duration = Duration::from_secs(10 * 60);
const BAN: Duration = Duration::from_secs(15 * 60);
const FAILURE_DELAY: Duration = Duration::from_secs(1);
const COMMANDS_PER_SECOND: usize = 20;
/// Largest command a client may send, the same cap Source servers keep.
const MAX_REQUEST_BODY: usize = 4096;

fn desc(key: &str, title: &str, kind: Kind, default: &str, description: &str) -> Desc {
    Desc {
        key: key.to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        kind,
        default: default.to_owned(),
        min: 0.0,
        max: 0.0,
        choices: Vec::new(),
        labels: Vec::new(),
        restart_required: false,
        advanced: true,
        transient: false,
        secret: false,
    }
}

/// The RCON settings of the loader's entry.
#[must_use]
pub fn settings() -> Vec<Desc> {
    let mut enabled = desc(
        keys::ENABLED,
        "Remote console (RCON)",
        Kind::Bool,
        "false",
        "Accept Source RCON connections. Whoever has the password can run anything the \
         console can, as if typed in the game. Needs a password.",
    );
    enabled.advanced = false;
    // No bounds on purpose: bounds make a slider in the settings screens, which moved through
    // a thousand ports a frame and restarted the server for each.
    let port = desc(
        keys::PORT,
        "RCON port",
        Kind::Int,
        &DEFAULT_PORT.to_string(),
        "TCP port the remote console listens on, 1 to 65535.",
    );
    let mut password = desc(
        keys::PASSWORD,
        "RCON password",
        Kind::String,
        "",
        "At least 12 characters. Stored as a hash; nothing shows it again.",
    );
    password.secret = true;
    password.advanced = false;
    vec![
        enabled,
        desc(
            keys::BIND,
            "RCON address",
            Kind::String,
            "127.0.0.1",
            "Address to listen on: 127.0.0.1 for this computer only, 0.0.0.0 or one of its \
             addresses for the network. RCON sends the password in clear text: never open \
             it to the internet; use a VPN such as Tailscale.",
        ),
        port,
        password,
        desc(
            keys::ALLOW,
            "RCON allowed addresses",
            Kind::String,
            "",
            "Addresses or networks allowed to connect, comma separated (192.168.2.0/24, \
             100.64.0.0/10); empty allows any that can reach the port.",
        ),
    ]
}

/// An address range, `a.b.c.d/n`, or one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Net {
    addr: IpAddr,
    prefix: u32,
}

impl Net {
    /// Parse `addr` or `addr/prefix`.
    ///
    /// # Errors
    /// Not an address, or a prefix longer than the address.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (addr, prefix) = match text.trim().split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (text.trim(), None),
        };
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("{text:?} is not an address"))?;
        let bits = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p
                .parse::<u32>()
                .ok()
                .filter(|p| *p <= bits)
                .ok_or_else(|| format!("{text:?} has a bad prefix"))?,
            None => bits,
        };
        Ok(Net { addr, prefix })
    }

    /// Whether `ip` is in the range.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        let to_bits = |ip: IpAddr| match ip {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)) << 96, 32),
            IpAddr::V6(v6) => (u128::from(v6), 128),
        };
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 @ IpAddr::V4(_) => v4,
        };
        let ((net, width), (have, other)) = (to_bits(self.addr), to_bits(ip));
        if width != other {
            return false;
        }
        let mask = if self.prefix == 0 {
            0
        } else {
            u128::MAX << (128 - self.prefix)
        };
        net & mask == have & mask
    }
}

/// What the server needs to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Where to listen.
    pub addr: SocketAddr,
    /// The stored Argon2id hash of the password.
    pub password_hash: String,
    /// Allowed peers; empty allows any.
    pub allow: Vec<Net>,
    /// `rcon.log`.
    pub audit: PathBuf,
}

/// Build the config from the settings' text values, or say why the server stays off.
///
/// # Errors
/// Switched off, no password, or a value that does not parse.
pub fn config(values: &BTreeMap<String, String>, audit: PathBuf) -> Result<Config, String> {
    let get = |key: &str| values.get(key).map_or("", String::as_str);
    if get(keys::ENABLED) != "true" {
        return Err("off".to_owned());
    }
    let password_hash = get(keys::PASSWORD).to_owned();
    if password_hash.is_empty() {
        return Err("no password set (uniloader.rcon_password)".to_owned());
    }
    let ip: IpAddr = get(keys::BIND)
        .trim()
        .parse()
        .map_err(|_| format!("rcon_bind {:?} is not an address", get(keys::BIND)))?;
    let port: u16 = get(keys::PORT)
        .parse()
        .map_err(|_| format!("rcon_port {:?} is not a port", get(keys::PORT)))?;
    let allow = get(keys::ALLOW)
        .split(',')
        .filter(|part| !part.trim().is_empty())
        .map(Net::parse)
        .collect::<Result<Vec<Net>, String>>()?;
    Ok(Config {
        addr: SocketAddr::new(ip, port),
        password_hash,
        allow,
        audit,
    })
}

/// What runs a script for a connection: returns its output and the script as the audit log
/// may show it (secrets redacted).
pub type Exec = dyn Fn(&str) -> (Vec<String>, String) + Send + Sync;

/// Shared between the listener and its connections.
struct Shared {
    config: Config,
    exec: Arc<Exec>,
    stop: AtomicBool,
    open: AtomicUsize,
    next_id: AtomicUsize,
    failures: Mutex<BTreeMap<IpAddr, Vec<Instant>>>,
    banned: Mutex<BTreeMap<IpAddr, Instant>>,
    streams: Mutex<BTreeMap<usize, TcpStream>>,
    audit: Mutex<()>,
}

impl Shared {
    fn audit(&self, line: &str) {
        log::info!("rcon: {line}");
        let _guard = self.audit.lock().unwrap_or_else(PoisonError::into_inner);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.audit)
            .and_then(|mut f| writeln!(f, "{stamp} {line}"));
        if let Err(e) = written {
            log::warn!("rcon: cannot write {}: {e}", self.config.audit.display());
        }
    }

    fn is_banned(&self, ip: IpAddr) -> bool {
        let mut banned = self.banned.lock().unwrap_or_else(PoisonError::into_inner);
        match banned.get(&ip) {
            Some(until) if *until > Instant::now() => true,
            Some(_) => {
                banned.remove(&ip);
                false
            }
            None => false,
        }
    }

    /// Count a failed login; returns whether the address is banned now.
    ///
    /// Addresses whose failures and bans have run out are dropped on the way, so a stream of
    /// failures from ever new addresses does not grow either map for good.
    fn failed(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut failures = self.failures.lock().unwrap_or_else(PoisonError::into_inner);
        failures.retain(|_, at| {
            at.retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
            !at.is_empty()
        });
        let recent = failures.entry(ip).or_default();
        recent.push(now);
        let mut banned = self.banned.lock().unwrap_or_else(PoisonError::into_inner);
        banned.retain(|_, until| *until > now);
        if recent.len() < MAX_FAILURES {
            return banned.contains_key(&ip);
        }
        failures.remove(&ip);
        banned.insert(ip, now + BAN);
        true
    }
}

/// A running server. Dropping it does not stop it; [`Server::stop`] does.
pub struct Server {
    shared: Arc<Shared>,
    addr: SocketAddr,
    listener: Option<JoinHandle<()>>,
}

impl Server {
    /// Listen as `config` says.
    ///
    /// # Errors
    /// The address cannot be bound.
    pub fn start(config: Config, exec: Arc<Exec>) -> Result<Self, String> {
        let listener = TcpListener::bind(config.addr)
            .map_err(|e| format!("cannot listen on {}: {e}", config.addr))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        if let Some(parent) = config.audit.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let shared = Arc::new(Shared {
            config,
            exec,
            stop: AtomicBool::new(false),
            open: AtomicUsize::new(0),
            next_id: AtomicUsize::new(1),
            failures: Mutex::new(BTreeMap::new()),
            banned: Mutex::new(BTreeMap::new()),
            streams: Mutex::new(BTreeMap::new()),
            audit: Mutex::new(()),
        });
        let accepting = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("uniloader-rcon".to_owned())
            .spawn(move || accept(&accepting, &listener))
            .map_err(|e| e.to_string())?;
        shared.audit(&format!("listening on {addr}"));
        Ok(Server {
            shared,
            addr,
            listener: Some(thread),
        })
    }

    /// Where it listens (with the real port when 0 was asked for).
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop listening and close every connection.
    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // `accept` has no timeout; a connection of our own wakes it to see the flag.
        let wake = if self.addr.ip().is_unspecified() {
            SocketAddr::new(IpAddr::from([127, 0, 0, 1]), self.addr.port())
        } else {
            self.addr
        };
        let _ = TcpStream::connect_timeout(&wake, Duration::from_secs(1));
        if let Some(thread) = self.listener.take() {
            let _ = thread.join();
        }
        let streams = std::mem::take(
            &mut *self
                .shared
                .streams
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for stream in streams.values() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        self.shared.audit("stopped");
    }
}

fn accept(shared: &Arc<Shared>, listener: &TcpListener) {
    for stream in listener.incoming() {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else { continue };
        let Ok(peer) = stream.peer_addr() else {
            continue;
        };
        let ip = peer.ip();
        if !shared.config.allow.is_empty() && !shared.config.allow.iter().any(|n| n.contains(ip)) {
            shared.audit(&format!("{peer} refused: not in rcon_allow"));
            continue;
        }
        if shared.is_banned(ip) {
            shared.audit(&format!("{peer} refused: banned"));
            continue;
        }
        if shared.open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            shared.open.fetch_sub(1, Ordering::SeqCst);
            shared.audit(&format!(
                "{peer} refused: {MAX_CONNECTIONS} connections open"
            ));
            continue;
        }
        let id = shared.next_id.fetch_add(1, Ordering::SeqCst);
        if let Ok(clone) = stream.try_clone() {
            shared
                .streams
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id, clone);
        }
        let connection = Arc::clone(shared);
        let spawned = std::thread::Builder::new()
            .name(format!("uniloader-rcon-{id}"))
            .spawn(move || {
                serve(&connection, stream, peer, id);
                connection
                    .streams
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                connection.open.fetch_sub(1, Ordering::SeqCst);
            });
        if let Err(e) = spawned {
            shared.open.fetch_sub(1, Ordering::SeqCst);
            log::warn!("rcon: cannot start a connection thread: {e}");
        }
    }
}

/// One connection, from login to close.
///
/// The login has [`LOGIN_TIMEOUT`] in total, not per packet, and a failed login that bans
/// the address also ends the connection it came on: otherwise one connection kept open could
/// go on guessing a password a second for as long as it liked.
fn serve(shared: &Shared, mut stream: TcpStream, peer: SocketAddr, id: usize) {
    let who = format!("{peer} #{id}");
    shared.audit(&format!("{who} connected"));
    let login_by = Instant::now() + LOGIN_TIMEOUT;
    let mut authed = false;
    let mut recent: Vec<Instant> = Vec::new();
    loop {
        if !authed {
            let left = login_by.saturating_duration_since(Instant::now());
            if left.is_zero() {
                shared.audit(&format!("{who} closed: no login within {LOGIN_TIMEOUT:?}"));
                return;
            }
            let _ = stream.set_read_timeout(Some(left));
        }
        let packet = match Packet::read_from(&mut stream, MAX_REQUEST_BODY) {
            Ok(p) => p,
            Err(e) => {
                shared.audit(&format!("{who} closed: {e}"));
                return;
            }
        };
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        let mut banned = false;
        let answer = if authed {
            command(shared, &who, &packet, &mut recent)
        } else if packet.kind == AUTH {
            let ok = crate::secret::verify(&shared.config.password_hash, &packet.body);
            if ok {
                authed = true;
                let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
                shared.audit(&format!("{who} auth ok"));
            } else {
                banned = shared.failed(peer.ip());
                shared.audit(&format!(
                    "{who} auth failed{}",
                    if banned {
                        "; address banned for 15 min"
                    } else {
                        ""
                    }
                ));
                std::thread::sleep(FAILURE_DELAY);
            }
            let reply_id = if ok { packet.id } else { AUTH_FAILED_ID };
            vec![
                Packet::new(packet.id, RESPONSE_VALUE, ""),
                Packet::new(reply_id, AUTH_RESPONSE, ""),
            ]
        } else {
            shared.audit(&format!("{who} closed: command before login"));
            return;
        };
        let bytes: Vec<u8> = answer.iter().flat_map(Packet::encode).collect();
        if let Err(e) = stream.write_all(&bytes) {
            shared.audit(&format!("{who} closed: {e}"));
            return;
        }
        if banned {
            shared.audit(&format!("{who} closed: address banned"));
            return;
        }
    }
}

/// An authenticated client's packet: run a command, or mirror the end marker.
fn command(shared: &Shared, who: &str, packet: &Packet, recent: &mut Vec<Instant>) -> Vec<Packet> {
    match packet.kind {
        EXEC_COMMAND => {
            throttle(recent);
            let (lines, logged) = (shared.exec)(&packet.body);
            shared.audit(&format!("{who} ran {logged:?}: {} lines", lines.len()));
            source_rcon::responses(packet.id, &lines.join("\n"))
        }
        // Clients send an empty response after a command and wait for it to come back, to
        // know the output before it is complete.
        RESPONSE_VALUE => vec![Packet::new(packet.id, RESPONSE_VALUE, "")],
        _ => Vec::new(),
    }
}

/// Hold a connection to [`COMMANDS_PER_SECOND`].
fn throttle(recent: &mut Vec<Instant>) {
    let now = Instant::now();
    recent.retain(|at| now.duration_since(*at) < Duration::from_secs(1));
    if recent.len() >= COMMANDS_PER_SECOND {
        if let Some(oldest) = recent.first() {
            std::thread::sleep(Duration::from_secs(1).saturating_sub(now.duration_since(*oldest)));
        }
        recent.remove(0);
    }
    recent.push(Instant::now());
}

#[cfg(test)]
#[path = "rcon_tests.rs"]
mod tests;
