//! Shared MCP daemon: one `trouve-search` process serving every agent
//! session over a unix domain socket.
//!
//! Agents spawn one MCP stdio server per session, and each server holds up
//! to [`crate::mcp::IndexCache`]'s limit of full in-memory indexes — working
//! across many sessions multiplies that RAM for no benefit, since the
//! sessions overwhelmingly index the same repositories. Instead, the bare
//! `trouve-search` MCP entry now runs as a thin stdio⇄socket proxy: the
//! first session starts a detached daemon (`trouve-search daemon`) and every
//! session forwards its JSON-RPC lines to it, so all sessions share one
//! model and one index cache.
//!
//! Lifecycle: the daemon is keyed (via the socket name) by binary version,
//! content types, and embedding model, under the trouve cache folder. A
//! lock file serializes competing daemon starts; the daemon exits on its
//! own after an idle period with no connected sessions. If the daemon
//! cannot be reached (or dies mid-session), the proxy falls back to serving
//! in-process, so a session never loses search over daemon trouble.
//!
//! Windows has no unix sockets; the MCP entry serves in-process there,
//! exactly as before. Set `TROUVE_DAEMON=0` to opt out on unix too.

use std::process::ExitCode;

use crate::types::ContentType;

/// Serve the default MCP entry: proxy to the shared daemon where possible,
/// otherwise serve stdio in-process.
pub fn serve_default(content: &[ContentType]) -> ExitCode {
    #[cfg(unix)]
    if unix::daemon_enabled() {
        return unix::proxy_stdio(content);
    }
    crate::mcp::serve(content)
}

/// Run the shared daemon in the foreground (the `daemon` subcommand).
pub fn run_daemon(content: &[ContentType]) -> ExitCode {
    #[cfg(unix)]
    {
        unix::run_daemon(content)
    }
    #[cfg(not(unix))]
    {
        let _ = content;
        eprintln!(
            "the shared daemon requires unix domain sockets; run `trouve-search` \
             without a subcommand instead"
        );
        ExitCode::FAILURE
    }
}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io::{BufRead, BufReader, ErrorKind, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use serde_json::Value;

    use crate::mcp::{IndexCache, respond_line, serve_lines};
    use crate::store::resolve_cache_folder;
    use crate::types::ContentType;
    use crate::utils::{is_git_url, resolve_model_name};

    /// Exit after this long with no connected sessions (override with
    /// `TROUVE_DAEMON_IDLE_SECONDS`; `0` disables idle shutdown).
    const DEFAULT_IDLE: Duration = Duration::from_secs(15 * 60);
    /// How long a proxy waits for a freshly spawned daemon to bind.
    const SPAWN_WAIT: Duration = Duration::from_secs(10);

    pub(super) fn daemon_enabled() -> bool {
        !matches!(
            std::env::var("TROUVE_DAEMON").as_deref().map(str::trim),
            Ok("0") | Ok("off") | Ok("false") | Ok("no")
        )
    }

    fn idle_timeout() -> Option<Duration> {
        match std::env::var("TROUVE_DAEMON_IDLE_SECONDS") {
            Ok(v) => match v.trim().parse::<u64>() {
                Ok(0) => None,
                Ok(secs) => Some(Duration::from_secs(secs)),
                Err(_) => Some(DEFAULT_IDLE),
            },
            Err(_) => Some(DEFAULT_IDLE),
        }
    }

    /// Socket path for this daemon identity. The name hashes everything
    /// that shapes the daemon's in-memory cache — binary version (an old
    /// daemon must never serve a newer client), content types, and the
    /// resolved embedding model — so mismatched configurations get separate
    /// daemons instead of wrong answers. The cache location is implicit in
    /// the parent directory.
    fn socket_path(content: &[ContentType]) -> PathBuf {
        let mut tags: Vec<&str> = content.iter().map(ContentType::as_str).collect();
        tags.sort_unstable();
        tags.dedup();
        let mut hasher = blake3::Hasher::new();
        hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
        for tag in &tags {
            hasher.update(b"\x00");
            hasher.update(tag.as_bytes());
        }
        hasher.update(b"\x00");
        hasher.update(resolve_model_name().as_bytes());
        let digest = hasher.finalize().to_hex();
        resolve_cache_folder()
            .join("daemon")
            .join(format!("mcp-{}.sock", &digest.as_str()[..16]))
    }

    // ---------------------------------------------------------------- daemon

    pub(super) fn run_daemon(content: &[ContentType]) -> ExitCode {
        let sock = socket_path(content);
        let dir = sock.parent().expect("socket path has a parent");
        if fs::create_dir_all(dir).is_err() {
            eprintln!("cannot create daemon directory {}", dir.display());
            return ExitCode::FAILURE;
        }
        // Owner-only: the socket accepts tool calls from anyone who can
        // connect, same trust boundary as the cache files next to it. This
        // is the actual isolation mechanism, so refusing to run beats
        // silently serving from a world-traversable directory.
        if let Err(e) = fs::set_permissions(dir, fs::Permissions::from_mode(0o700)) {
            eprintln!("cannot restrict permissions on {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }

        let lock_path = sock.with_extension("lock");
        let Ok(lock) = fs::File::create(&lock_path) else {
            eprintln!("cannot create lock file {}", lock_path.display());
            return ExitCode::FAILURE;
        };
        if lock.try_lock().is_err() {
            // Another daemon already owns this socket; nothing to do.
            return ExitCode::SUCCESS;
        }
        // Holding the lock proves any existing socket file is a leftover
        // from a crashed daemon, so removing it is safe.
        let _ = fs::remove_file(&sock);
        let listener = match UnixListener::bind(&sock) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot bind {}: {e}", sock.display());
                return ExitCode::FAILURE;
            }
        };
        // Non-blocking accept so the loop can notice idleness. A blocking
        // listener would hang in accept() forever and never idle out.
        if let Err(e) = listener.set_nonblocking(true) {
            eprintln!("cannot configure {} as non-blocking: {e}", sock.display());
            return ExitCode::FAILURE;
        }

        let cache = Arc::new(IndexCache::new(content.to_vec()));
        let active = Arc::new(AtomicUsize::new(0));
        let idle = idle_timeout();
        let mut last_activity = Instant::now();
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    last_activity = Instant::now();
                    // Some platforms make accepted sockets inherit the
                    // listener's non-blocking flag; serve_lines would then
                    // misread WouldBlock as a closed stream. Drop the
                    // connection rather than serve it wrongly configured —
                    // the proxy falls back to serving in-process.
                    if let Err(e) = stream.set_nonblocking(false) {
                        eprintln!("cannot configure accepted daemon connection: {e}");
                        continue;
                    }
                    active.fetch_add(1, Ordering::SeqCst);
                    let cache = Arc::clone(&cache);
                    let active = Arc::clone(&active);
                    std::thread::spawn(move || {
                        // Decrement even if the handler panics, or the
                        // daemon would never idle out.
                        struct Guard(Arc<AtomicUsize>);
                        impl Drop for Guard {
                            fn drop(&mut self) {
                                self.0.fetch_sub(1, Ordering::SeqCst);
                            }
                        }
                        let _guard = Guard(active);
                        let Ok(read_half) = stream.try_clone() else {
                            return;
                        };
                        serve_lines(&cache, BufReader::new(read_half), stream);
                    });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if active.load(Ordering::SeqCst) > 0 {
                        last_activity = Instant::now();
                    } else if let Some(idle) = idle
                        && last_activity.elapsed() >= idle
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(_) => break,
            }
        }
        // Remove the socket before the lock: a successor can only bind
        // after it takes a fresh lock, which requires this one gone.
        let _ = fs::remove_file(&sock);
        let _ = fs::remove_file(&lock_path);
        ExitCode::SUCCESS
    }

    // ----------------------------------------------------------------- proxy

    /// One line-oriented connection to the daemon.
    struct DaemonConn {
        stream: UnixStream,
        reader: BufReader<UnixStream>,
    }

    impl DaemonConn {
        fn open(sock: &Path) -> std::io::Result<DaemonConn> {
            let stream = UnixStream::connect(sock)?;
            let reader = BufReader::new(stream.try_clone()?);
            Ok(DaemonConn { stream, reader })
        }

        /// Send one request line; read exactly one response line when the
        /// request warrants one (the server never speaks unprompted).
        fn roundtrip(
            &mut self,
            line: &str,
            expects_response: bool,
        ) -> std::io::Result<Option<String>> {
            writeln!(self.stream, "{line}")?;
            self.stream.flush()?;
            if !expects_response {
                return Ok(None);
            }
            let mut response = String::new();
            if self.reader.read_line(&mut response)? == 0 {
                return Err(std::io::Error::from(ErrorKind::UnexpectedEof));
            }
            Ok(Some(response.trim_end().to_string()))
        }
    }

    fn spawn_daemon(content: &[ContentType]) -> std::io::Result<()> {
        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("daemon");
        if !content.is_empty() {
            cmd.arg("--content");
            for ct in content {
                cmd.arg(ct.as_str());
            }
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // Don't pin whatever directory the agent launched us in.
            .current_dir("/");
        // Detach into its own session so the daemon survives the agent
        // closing this session's process group.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        // Reap in the background; the child exits quickly when another
        // daemon already holds the lock.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    fn connect_or_spawn(sock: &Path, content: &[ContentType]) -> Option<DaemonConn> {
        match DaemonConn::open(sock) {
            Ok(conn) => return Some(conn),
            // The socket path exceeds sockaddr_un's limit (104 bytes on
            // macOS): no daemon can ever bind it, so don't spawn one and
            // wait — serve in-process straight away.
            Err(e) if e.kind() == ErrorKind::InvalidInput => return None,
            Err(_) => {}
        }
        spawn_daemon(content).ok()?;
        // Wait for a daemon to bind — ours, or a competing proxy's whose
        // daemon won the lock (just as good).
        let deadline = Instant::now() + SPAWN_WAIT;
        loop {
            std::thread::sleep(Duration::from_millis(50));
            if let Ok(conn) = DaemonConn::open(sock) {
                return Some(conn);
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    /// Absolute form of a relative `repo` argument. The daemon runs in its
    /// own working directory, so paths relative to this session's cwd must
    /// be resolved before forwarding.
    fn absolutize_repo(repo: &str) -> Option<String> {
        if repo.is_empty() || is_git_url(repo) || Path::new(repo).is_absolute() {
            return None;
        }
        let joined = std::env::current_dir().ok()?.join(repo);
        let resolved = joined.canonicalize().unwrap_or(joined);
        Some(resolved.to_string_lossy().into_owned())
    }

    /// Prepare one stdin line for forwarding: rewrite a relative `repo` to
    /// an absolute path, and decide whether the server will answer it.
    /// Mirrors `handle_request`: a reply comes back for unparsable lines
    /// (parse error) and for requests with a non-null id; notifications get
    /// nothing.
    fn prepare_line(line: &str) -> (String, bool) {
        let Ok(mut request) = serde_json::from_str::<Value>(line) else {
            return (line.to_string(), true);
        };
        let id = request.get("id");
        let expects_response = !(id.is_none() || id == Some(&Value::Null));
        if request.get("method").and_then(Value::as_str) == Some("tools/call")
            && let Some(repo) = request
                .pointer("/params/arguments/repo")
                .and_then(Value::as_str)
            && let Some(abs) = absolutize_repo(repo)
        {
            *request.pointer_mut("/params/arguments/repo").unwrap() = Value::String(abs);
            return (request.to_string(), expects_response);
        }
        (line.to_string(), expects_response)
    }

    /// Serve stdio by forwarding to the shared daemon, falling back to an
    /// in-process cache if the daemon is unreachable or dies mid-session.
    pub(super) fn proxy_stdio(content: &[ContentType]) -> ExitCode {
        let sock = socket_path(content);
        enum Backend {
            Daemon(DaemonConn),
            Local(IndexCache),
        }
        let mut backend = match connect_or_spawn(&sock, content) {
            Some(conn) => Backend::Daemon(conn),
            None => Backend::Local(IndexCache::new(content.to_vec())),
        };

        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let emit = |response: &str| {
            let mut out = stdout.lock();
            let _ = writeln!(out, "{response}");
            let _ = out.flush();
        };
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let (line, expects_response) = prepare_line(&line);
            if let Backend::Daemon(conn) = &mut backend {
                match forward(conn, &sock, content, &line, expects_response) {
                    Ok((response, reconnected)) => {
                        if let Some(conn) = reconnected {
                            backend = Backend::Daemon(conn);
                        }
                        if let Some(response) = response {
                            emit(&response);
                        }
                        continue;
                    }
                    Err(_) => {
                        // Daemon gone and not restartable: finish the
                        // session in-process rather than going dark.
                        backend = Backend::Local(IndexCache::new(content.to_vec()));
                    }
                }
            }
            let Backend::Local(cache) = &backend else {
                unreachable!()
            };
            if let Some(response) = respond_line(cache, &line) {
                emit(&response.to_string());
            }
        }
        ExitCode::SUCCESS
    }

    /// Forward one line, transparently reconnecting (and respawning the
    /// daemon) once if the connection died. Returns the response (if any)
    /// and the replacement connection when a reconnect happened.
    fn forward(
        conn: &mut DaemonConn,
        sock: &Path,
        content: &[ContentType],
        line: &str,
        expects_response: bool,
    ) -> Result<(Option<String>, Option<DaemonConn>), ()> {
        match conn.roundtrip(line, expects_response) {
            Ok(response) => Ok((response, None)),
            Err(_) => {
                let mut fresh = connect_or_spawn(sock, content).ok_or(())?;
                let response = fresh.roundtrip(line, expects_response).map_err(|_| ())?;
                Ok((response, Some(fresh)))
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn socket_path_is_stable_and_content_sensitive() {
            let code = socket_path(&[ContentType::Code]);
            assert_eq!(code, socket_path(&[ContentType::Code]));
            // Order and duplicates don't matter; the set does.
            assert_eq!(
                socket_path(&[ContentType::Docs, ContentType::Code, ContentType::Code]),
                socket_path(&[ContentType::Code, ContentType::Docs])
            );
            assert_ne!(code, socket_path(&[ContentType::Docs]));
            assert!(
                code.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("mcp-")
            );
        }

        #[test]
        fn prepare_line_response_expectations() {
            // Unparsable: the server answers with a parse error.
            assert!(prepare_line("{not json").1);
            // Request with an id.
            assert!(prepare_line(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).1);
            // Notification (no id) and null id: no response.
            assert!(!prepare_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).1);
            assert!(!prepare_line(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).1);
        }

        #[test]
        fn prepare_line_absolutizes_relative_repo() {
            let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search","arguments":{"query":"x","repo":"."}}}"#;
            let (rewritten, expects) = prepare_line(line);
            assert!(expects);
            let v: Value = serde_json::from_str(&rewritten).unwrap();
            let repo = v
                .pointer("/params/arguments/repo")
                .unwrap()
                .as_str()
                .unwrap();
            assert!(Path::new(repo).is_absolute(), "got {repo}");

            // Absolute paths and git URLs pass through untouched.
            for repo in ["/abs/path", "git@github.com:org/repo.git"] {
                let line = format!(
                    r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"search","arguments":{{"query":"x","repo":"{repo}"}}}}}}"#
                );
                let (out, _) = prepare_line(&line);
                assert_eq!(out, line);
            }
        }
    }
}
