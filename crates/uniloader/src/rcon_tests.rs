//! Tests for [`crate::rcon`]: a real client on a loopback port, no game.

use std::io::Read;

use super::*;

fn server(password: &str) -> (Server, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "uniloader-rcon-{}-{}",
        std::process::id(),
        password.len()
    ));
    let config = Config {
        addr: SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0),
        password_hash: crate::secret::hash(password).unwrap_or_else(|e| panic!("{e}")),
        allow: Vec::new(),
        audit: dir.join("rcon.log"),
    };
    let exec: Arc<Exec> = Arc::new(|script: &str| {
        let lines = match script {
            "big" => vec!["x".repeat(3000), "y".repeat(3000)],
            "secret" => vec!["ok".to_owned()],
            other => vec![format!("ran {other}")],
        };
        let logged = if script == "secret" {
            "set a.b <redacted>".to_owned()
        } else {
            script.to_owned()
        };
        (lines, logged)
    });
    let server = Server::start(config, exec).unwrap_or_else(|e| panic!("{e}"));
    (server, dir)
}

fn connect(server: &Server) -> TcpStream {
    let stream = TcpStream::connect(server.addr()).unwrap_or_else(|e| panic!("{e}"));
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    stream
}

fn send(stream: &mut TcpStream, id: i32, kind: i32, body: &str) {
    Packet::new(id, kind, body)
        .write_to(stream)
        .unwrap_or_else(|e| panic!("{e}"));
}

fn recv(stream: &mut TcpStream) -> Packet {
    Packet::read_from(stream, 8192).unwrap_or_else(|e| panic!("{e}"))
}

fn closed(stream: &mut TcpStream) -> bool {
    let mut byte = [0u8; 1];
    matches!(stream.read(&mut byte), Ok(0) | Err(_))
}

#[test]
fn login_commands_and_the_end_marker() {
    let password = "correct horse battery";
    let (server, dir) = server(password);
    let mut c = connect(&server);
    send(&mut c, 5, AUTH, password);
    assert_eq!(recv(&mut c), Packet::new(5, RESPONSE_VALUE, ""));
    assert_eq!(recv(&mut c), Packet::new(5, AUTH_RESPONSE, ""));
    send(&mut c, 6, EXEC_COMMAND, "echo hi");
    assert_eq!(recv(&mut c), Packet::new(6, RESPONSE_VALUE, "ran echo hi"));
    // Output over 4096 bytes comes in several packets, then the mirrored end marker.
    send(&mut c, 7, EXEC_COMMAND, "big");
    send(&mut c, 8, RESPONSE_VALUE, "");
    let mut body = String::new();
    loop {
        let p = recv(&mut c);
        if p.id == 8 {
            assert_eq!(p.body, "");
            break;
        }
        assert_eq!(p.id, 7);
        assert!(p.body.len() <= 4096);
        body.push_str(&p.body);
    }
    assert_eq!(body.len(), 6001);
    send(&mut c, 9, EXEC_COMMAND, "secret");
    recv(&mut c);
    server.stop();
    assert!(closed(&mut c), "stopping closes connections");
    let log = std::fs::read_to_string(dir.join("rcon.log")).unwrap_or_default();
    assert!(log.contains("auth ok"), "{log}");
    assert!(log.contains("set a.b <redacted>"));
    assert!(!log.contains(password), "the password is never logged");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bad_logins_answer_minus_one_and_ban_the_address() {
    let (server, dir) = server("the right one!");
    for attempt in 0..3 {
        let mut c = connect(&server);
        send(&mut c, 1, AUTH, "wrong password");
        assert_eq!(recv(&mut c).kind, RESPONSE_VALUE);
        let answer = recv(&mut c);
        assert_eq!(
            (answer.id, answer.kind),
            (AUTH_FAILED_ID, AUTH_RESPONSE),
            "attempt {attempt}"
        );
    }
    // Banned now: the connection is closed without an answer, even with the right password.
    let mut c = connect(&server);
    send(&mut c, 1, AUTH, "the right one!");
    assert!(closed(&mut c));
    server.stop();
    let log = std::fs::read_to_string(dir.join("rcon.log")).unwrap_or_default();
    assert!(log.contains("banned"), "{log}");
    assert!(!log.contains("wrong password"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_ban_also_ends_the_connection_that_kept_guessing() {
    let (server, dir) = server("one connection only");
    let mut c = connect(&server);
    for attempt in 0..3 {
        send(&mut c, 1, AUTH, "a wrong guess");
        assert_eq!(recv(&mut c).kind, RESPONSE_VALUE, "attempt {attempt}");
        assert_eq!(recv(&mut c).id, AUTH_FAILED_ID, "attempt {attempt}");
    }
    assert!(
        closed(&mut c),
        "a fourth guess on the same connection is not read"
    );
    server.stop();
    let log = std::fs::read_to_string(dir.join("rcon.log")).unwrap_or_default();
    assert!(log.contains("closed: address banned"), "{log}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn commands_before_login_and_oversized_packets_close_the_connection() {
    let (server, dir) = server("another password");
    let mut c = connect(&server);
    send(&mut c, 1, EXEC_COMMAND, "quit");
    assert!(closed(&mut c));
    let mut c = connect(&server);
    send(&mut c, 1, AUTH, &"x".repeat(5000));
    assert!(closed(&mut c));
    server.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn networks_and_config() {
    let net = Net::parse("192.168.2.0/24").unwrap_or_else(|e| panic!("{e}"));
    assert!(net.contains("192.168.2.77".parse().unwrap_or_else(|e| panic!("{e}"))));
    assert!(!net.contains("192.168.3.1".parse().unwrap_or_else(|e| panic!("{e}"))));
    assert!(net.contains(
        "::ffff:192.168.2.1"
            .parse()
            .unwrap_or_else(|e| panic!("{e}"))
    ));
    let one = Net::parse("::1").unwrap_or_else(|e| panic!("{e}"));
    assert!(one.contains("::1".parse().unwrap_or_else(|e| panic!("{e}"))));
    assert!(Net::parse("10.0.0.0/33").is_err());
    assert!(Net::parse("nope").is_err());

    let mut values: BTreeMap<String, String> =
        settings().into_iter().map(|d| (d.key, d.default)).collect();
    assert_eq!(config(&values, PathBuf::new()), Err("off".to_owned()));
    values.insert(keys::ENABLED.to_owned(), "true".to_owned());
    assert!(config(&values, PathBuf::new()).is_err_and(|e| e.contains("no password")));
    values.insert(keys::PASSWORD.to_owned(), "$argon2id$x".to_owned());
    values.insert(keys::ALLOW.to_owned(), "127.0.0.1, 10.0.0.0/8".to_owned());
    let built = config(&values, PathBuf::new()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        built.addr,
        "127.0.0.1:48622".parse().unwrap_or_else(|e| panic!("{e}"))
    );
    assert_eq!(built.allow.len(), 2);
}
