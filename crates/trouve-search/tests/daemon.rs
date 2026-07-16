//! Integration tests for the shared MCP daemon: concurrent stdio sessions
//! proxy to one unix-socket daemon and share its index cache.
//!
//! These spawn the real `trouve-search` binary and run offline against the
//! tiny deterministic local model (see `tests/common/mod.rs`).
#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use common::{test_env, write_file};
use serde_json::{Value, json};

/// One MCP stdio session against the trouve-search binary.
struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpSession {
    /// Spawn `trouve-search` (no subcommand) with an isolated cache, the
    /// toy model, and a short daemon idle timeout so leftover daemons from
    /// a test run exit promptly.
    fn spawn(cache: &str, model: &str, daemon: bool) -> McpSession {
        let mut child = Command::new(env!("CARGO_BIN_EXE_trouve-search"))
            .env("TROUVE_CACHE_LOCATION", cache)
            .env("TROUVE_MODEL_NAME", model)
            .env("TROUVE_DAEMON", if daemon { "1" } else { "0" })
            .env("TROUVE_DAEMON_IDLE_SECONDS", "2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn trouve-search");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        McpSession {
            child,
            stdin,
            stdout,
        }
    }

    fn request(&mut self, request: Value) -> Value {
        writeln!(self.stdin, "{request}").unwrap();
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "server closed the stream instead of answering {request}"
        );
        serde_json::from_str(&line).unwrap()
    }

    fn notify(&mut self, notification: Value) {
        writeln!(self.stdin, "{notification}").unwrap();
    }

    fn close(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

fn initialize(session: &mut McpSession) {
    let response = session.request(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05"}
    }));
    assert_eq!(response["result"]["serverInfo"]["name"], "trouve-search");
    session.notify(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
}

fn search(session: &mut McpSession, id: u64, repo: &Path, query: &str) -> Value {
    session.request(json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": "search", "arguments": {
            "query": query, "repo": repo.to_string_lossy(), "max_snippet_lines": 0
        }}
    }))
}

fn sample_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "src/auth.py",
        "def authenticate(user, password):\n    session = login(user, password)\n    return session\n",
    );
    write_file(
        dir.path(),
        "src/db.py",
        "def connect(config):\n    connection = database(config)\n    return connection\n",
    );
    dir
}

fn socket_count(cache: &str) -> usize {
    match std::fs::read_dir(PathBuf::from(cache).join("daemon")) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "sock"))
            .count(),
        Err(_) => 0,
    }
}

#[test]
fn concurrent_sessions_share_one_daemon() {
    let model = test_env();
    // A dedicated short cache path: the shared test cache lives under the
    // system temp dir, which on macOS runners is deep enough that the
    // daemon socket path would overflow sockaddr_un's 104-byte limit and
    // the daemon could never bind.
    let cache = format!("/tmp/trouve-daemon-test-{}", std::process::id());
    std::fs::create_dir_all(&cache).unwrap();
    let repo = sample_repo();

    // Two overlapping sessions: the first spawns the daemon, the second
    // must connect to the same one.
    let mut a = McpSession::spawn(&cache, model, true);
    let mut b = McpSession::spawn(&cache, model, true);
    initialize(&mut a);
    initialize(&mut b);

    let response = search(&mut a, 2, repo.path(), "authenticate");
    assert_eq!(response["result"]["isError"], false, "got {response}");
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("auth.py"), "got {text}");

    // Session b hits the daemon's already-built index for the same repo.
    let response = search(&mut b, 2, repo.path(), "database connection");
    assert_eq!(response["result"]["isError"], false, "got {response}");
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("db.py"), "got {text}");

    // Both sessions were served by a single daemon socket.
    assert_eq!(socket_count(&cache), 1);

    // Interleaved pings still route to the right session.
    let pong = a.request(json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}));
    assert_eq!(pong["id"], 3);
    let pong = b.request(json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}));
    assert_eq!(pong["id"], 4);

    a.close();
    b.close();
}

/// Spawn a foreground `trouve-search daemon` and wait for its socket.
fn spawn_daemon(cache: &str, model: &str) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_trouve-search"))
        .arg("daemon")
        .env("TROUVE_CACHE_LOCATION", cache)
        .env("TROUVE_MODEL_NAME", model)
        .env("TROUVE_DAEMON_IDLE_SECONDS", "30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn trouve-search daemon");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while socket_count(cache) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "daemon never bound its socket"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    child
}

#[test]
fn proxy_reconnects_after_daemon_death() {
    let model = test_env();
    let cache = format!("/tmp/trouve-daemon-reconnect-{}", std::process::id());
    std::fs::create_dir_all(&cache).unwrap();
    let repo = sample_repo();

    // The session connects to a daemon we control.
    let mut daemon = spawn_daemon(&cache, model);
    let mut session = McpSession::spawn(&cache, model, true);
    initialize(&mut session);
    let response = search(&mut session, 2, repo.path(), "authenticate");
    assert_eq!(response["result"]["isError"], false, "got {response}");

    // Kill the daemon out from under the session. The next request hits
    // the dead connection; the proxy must respawn a daemon, reconnect,
    // and replay the line transparently.
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let response = search(&mut session, 3, repo.path(), "database connection");
    assert_eq!(response["result"]["isError"], false, "got {response}");
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("db.py"), "got {text}");
    assert_eq!(socket_count(&cache), 1, "respawned daemon should be bound");
    session.close();
}

#[test]
fn proxy_falls_back_in_process_when_daemon_dies_unrestartably() {
    let model = test_env();
    let cache = format!("/tmp/trouve-daemon-fallback-{}", std::process::id());
    std::fs::create_dir_all(&cache).unwrap();
    let repo = sample_repo();

    let mut daemon = spawn_daemon(&cache, model);
    let mut session = McpSession::spawn(&cache, model, true);
    initialize(&mut session);
    let response = search(&mut session, 2, repo.path(), "authenticate");
    assert_eq!(response["result"]["isError"], false, "got {response}");

    // Kill the daemon and make respawning impossible: a regular file where
    // the daemon directory belongs fails the successor's create_dir_all.
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let daemon_dir = PathBuf::from(&cache).join("daemon");
    std::fs::remove_dir_all(&daemon_dir).unwrap();
    std::fs::write(&daemon_dir, b"not a directory").unwrap();

    // The reconnect attempt times out (~10s), then the session finishes
    // in-process rather than going dark.
    let response = search(&mut session, 3, repo.path(), "database connection");
    assert_eq!(response["result"]["isError"], false, "got {response}");
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("db.py"), "got {text}");
    session.close();
    std::fs::remove_file(&daemon_dir).unwrap();
}

#[test]
fn proxy_serves_in_process_when_the_socket_path_cannot_exist() {
    let model = test_env();
    // sockaddr_un caps socket paths (104 bytes on macOS, 108 on Linux); a
    // cache location deeper than that can never host a daemon socket. The
    // proxy must detect this immediately and serve in-process, not spawn
    // doomed daemons and wait on them.
    let cache = format!(
        "/tmp/trouve-daemon-longpath-{}-{}",
        std::process::id(),
        "x".repeat(110)
    );
    std::fs::create_dir_all(&cache).unwrap();
    let repo = sample_repo();

    let start = std::time::Instant::now();
    let mut session = McpSession::spawn(&cache, model, true);
    initialize(&mut session);
    let response = search(&mut session, 2, repo.path(), "authenticate");
    assert_eq!(response["result"]["isError"], false, "got {response}");
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("auth.py"), "got {text}");
    // Well under the 10s spawn wait: the fallback was immediate.
    assert!(
        start.elapsed() < std::time::Duration::from_secs(8),
        "fallback should not wait for a daemon that can never bind"
    );
    assert_eq!(socket_count(&cache), 0);
    session.close();
}

#[test]
fn daemon_disabled_serves_in_process() {
    let model = test_env();
    let cache = std::env::var("TROUVE_CACHE_LOCATION").unwrap();
    // A cache location private to this test so the other test's daemon
    // socket can't be miscounted here.
    let cache = format!("{cache}-nodaemon");
    std::fs::create_dir_all(&cache).unwrap();
    let repo = sample_repo();

    let mut session = McpSession::spawn(&cache, model, false);
    initialize(&mut session);
    let response = search(&mut session, 2, repo.path(), "authenticate");
    assert_eq!(response["result"]["isError"], false, "got {response}");
    assert_eq!(socket_count(&cache), 0);
    session.close();
}
