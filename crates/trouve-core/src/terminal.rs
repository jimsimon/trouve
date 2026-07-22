//! Integrated terminals: zero or more interactive shells per session,
//! spawned in the session's worktree over independent PTYs.
//!
//! The manager keeps a capped backlog of raw output per terminal (so a
//! client attaching later replays the screen history) plus a broadcast
//! channel for live bytes. Output is addressed by absolute byte offset:
//! subscribers pass the offset they have seen, get the retained backlog
//! from there, and follow the broadcast for the rest. Escape-sequence
//! interpretation is entirely client-side.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::broadcast;

/// Retained output per terminal. Enough for a few thousand lines of
/// scrollback; older bytes are dropped (offsets keep counting).
const BACKLOG_CAP: usize = 512 * 1024;

#[derive(Default)]
pub struct TerminalManager {
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    /// Session id → terminal ids, in creation order.
    by_session: Mutex<HashMap<String, Vec<String>>>,
    /// Sessions whose terminals were torn down by archive/delete/workspace
    /// close. Keeping this tombstone closes the race where a request reads
    /// the session just before teardown and otherwise spawns a late shell.
    closed_sessions: Mutex<std::collections::HashSet<String>>,
    /// Serializes shell spawns and the compatibility open-or-attach path.
    open_lock: Mutex<()>,
    /// Once shutdown begins, no new shells may be spawned. Reader threads
    /// retain their own `Arc<Terminal>`, so manager drop must actively kill
    /// children rather than relying on the terminal values being dropped.
    shutting_down: AtomicBool,
}

pub struct Terminal {
    pub id: String,
    pub session_id: String,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    size: Mutex<(u16, u16)>, // (cols, rows)
    backlog: Mutex<Backlog>,
    live: broadcast::Sender<bytes::Bytes>,
    exited: AtomicBool,
}

struct Backlog {
    /// Absolute offset of `data[0]`.
    start: u64,
    data: Vec<u8>,
}

impl Terminal {
    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    pub fn size(&self) -> (u16, u16) {
        *self.size.lock().unwrap()
    }

    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master.lock().unwrap().resize(PtySize {
            rows: rows.max(2),
            cols: cols.max(2),
            pixel_width: 0,
            pixel_height: 0,
        })?;
        *self.size.lock().unwrap() = (cols, rows);
        Ok(())
    }

    pub fn kill(&self) {
        let _ = self.child.lock().unwrap().kill();
    }

    /// Retained output from `after` (or the earliest still retained) plus a
    /// live receiver opened before the snapshot, so nothing falls in the
    /// gap. Returns (start offset of the replay, replay bytes, receiver).
    pub fn subscribe(&self, after: u64) -> (u64, Vec<u8>, broadcast::Receiver<bytes::Bytes>) {
        // Open the receiver and snapshot the backlog under one lock, and have
        // the reader broadcast under that same lock (see the reader thread).
        // That makes "append + broadcast" and "subscribe + snapshot" mutually
        // exclusive, so a chunk can't land in both the replay and the live
        // stream — which would otherwise double-render it and permanently
        // skew every subsequent SSE offset.
        let backlog = self.backlog.lock().unwrap();
        let rx = self.live.subscribe();
        let from = after.max(backlog.start);
        let skip = (from - backlog.start) as usize;
        let replay = backlog.data.get(skip..).unwrap_or_default().to_vec();
        (from, replay, rx)
    }
}

impl TerminalManager {
    /// The session's default live terminal, spawning one if none is live.
    /// This preserves the original single-terminal endpoint semantics.
    pub fn open_default(
        &self,
        session_id: &str,
        worktree: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Terminal>> {
        let _open = self.open_lock.lock().unwrap();
        self.ensure_open(session_id)?;
        if let Some(existing) = self
            .list_session(session_id)
            .into_iter()
            .find(|terminal| !terminal.exited())
        {
            return Ok(existing);
        }
        self.spawn(session_id, worktree, cols, rows)
    }

    /// Spawn a new independent terminal for a session.
    pub fn create(
        &self,
        session_id: &str,
        worktree: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Terminal>> {
        let _open = self.open_lock.lock().unwrap();
        self.ensure_open(session_id)?;
        self.spawn(session_id, worktree, cols, rows)
    }

    fn ensure_open(&self, session_id: &str) -> Result<()> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(anyhow!("terminal manager is shutting down"));
        }
        if self.closed_sessions.lock().unwrap().contains(session_id) {
            return Err(anyhow!("terminal session {session_id} is closed"));
        }
        Ok(())
    }

    fn spawn(
        &self,
        session_id: &str,
        worktree: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Terminal>> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: rows.max(2),
                cols: cols.max(2),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("openpty: {e}"))?;

        let mut cmd = CommandBuilder::new(default_shell());
        cmd.cwd(worktree);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow!("spawning shell: {e}"))?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| anyhow!("pty reader: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| anyhow!("pty writer: {e}"))?;

        let (live, _) = broadcast::channel(256);
        let terminal = Arc::new(Terminal {
            id: format!("term_{}", uuid::Uuid::new_v4().simple()),
            session_id: session_id.to_string(),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            size: Mutex::new((cols, rows)),
            backlog: Mutex::new(Backlog {
                start: 0,
                data: Vec::new(),
            }),
            live,
            exited: AtomicBool::new(false),
        });

        self.terminals
            .lock()
            .unwrap()
            .insert(terminal.id.clone(), terminal.clone());
        self.by_session
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .push(terminal.id.clone());

        // Blocking reader thread: PTY reads have no async story; the thread
        // exits when the shell does (read returns 0/Err).
        std::thread::spawn({
            let terminal = terminal.clone();
            let mut reader = reader;
            move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let chunk = bytes::Bytes::copy_from_slice(&buf[..n]);
                            // Append and broadcast under the backlog lock so a
                            // new subscriber (which snapshots + opens its
                            // receiver under the same lock) sees the chunk in
                            // exactly one of the two paths, never both.
                            let mut backlog = terminal.backlog.lock().unwrap();
                            backlog.data.extend_from_slice(&chunk);
                            if backlog.data.len() > BACKLOG_CAP {
                                let drop_n = backlog.data.len() - BACKLOG_CAP;
                                backlog.data.drain(..drop_n);
                                backlog.start += drop_n as u64;
                            }
                            let _ = terminal.live.send(chunk);
                        }
                    }
                }
                terminal.exited.store(true, Ordering::Relaxed);
                // Wake followers so they observe the exit; an empty chunk
                // is the end-of-stream sentinel.
                let _ = terminal.live.send(bytes::Bytes::new());
                let _ = terminal.child.lock().unwrap().wait();
            }
        });

        Ok(terminal)
    }

    pub fn get(&self, terminal_id: &str) -> Result<Arc<Terminal>> {
        self.terminals
            .lock()
            .unwrap()
            .get(terminal_id)
            .cloned()
            .with_context(|| format!("no terminal {terminal_id}"))
    }

    pub fn for_session(&self, session_id: &str) -> Option<Arc<Terminal>> {
        self.list_session(session_id).into_iter().next()
    }

    /// All terminals belonging to a session, in creation order.
    pub fn list_session(&self, session_id: &str) -> Vec<Arc<Terminal>> {
        let ids = self
            .by_session
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default();
        let terminals = self.terminals.lock().unwrap();
        ids.into_iter()
            .filter_map(|id| terminals.get(&id).cloned())
            .collect()
    }

    /// Kill and forget a terminal (explicit close or restart).
    pub fn remove(&self, terminal_id: &str) {
        let removed = self.terminals.lock().unwrap().remove(terminal_id);
        if let Some(terminal) = removed {
            terminal.kill();
            let mut by_session = self.by_session.lock().unwrap();
            if let Some(ids) = by_session.get_mut(&terminal.session_id) {
                ids.retain(|id| id != &terminal.id);
            }
            if by_session
                .get(&terminal.session_id)
                .is_some_and(Vec::is_empty)
            {
                by_session.remove(&terminal.session_id);
            }
        }
    }

    /// Kill all of the session's terminals and prevent a racing request from
    /// spawning another one. An unarchived session must be reopened
    /// explicitly with [`Self::reopen_session`].
    pub fn remove_session(&self, session_id: &str) {
        let _open = self.open_lock.lock().unwrap();
        self.closed_sessions
            .lock()
            .unwrap()
            .insert(session_id.to_string());
        let ids = self
            .by_session
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default();
        for id in ids {
            self.remove(&id);
        }
    }

    /// Allow terminals to be created again after a session is unarchived.
    pub fn reopen_session(&self, session_id: &str) {
        let _open = self.open_lock.lock().unwrap();
        self.closed_sessions.lock().unwrap().remove(session_id);
    }

    /// Permanently stop this manager and kill every registered shell.
    /// Idempotent so explicit server shutdown and `Drop` can both call it.
    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        let _open = self.open_lock.lock().unwrap();
        let terminals = {
            let mut terminals = self.terminals.lock().unwrap();
            std::mem::take(&mut *terminals)
        };
        self.by_session.lock().unwrap().clear();
        self.closed_sessions.lock().unwrap().clear();
        for terminal in terminals.into_values() {
            terminal.kill();
        }
    }
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn default_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shell_roundtrip_backlog_and_live() {
        let mgr = TerminalManager::default();
        let dir = tempfile::tempdir().unwrap();
        let term = mgr.open_default("s1", dir.path(), 80, 24).unwrap();
        assert!(!term.exited());

        // Re-open returns the same live terminal.
        let again = mgr.open_default("s1", dir.path(), 80, 24).unwrap();
        assert_eq!(again.id, term.id);

        term.write(b"printf 'marker-%s' 42\rexit\r").unwrap();

        // Follow output until the exit sentinel.
        let (mut offset, replay, mut rx) = term.subscribe(0);
        let mut out = replay;
        offset += out.len() as u64;
        while let Ok(Ok(chunk)) =
            tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await
        {
            if chunk.is_empty() {
                break;
            }
            out.extend_from_slice(&chunk);
            offset += chunk.len() as u64;
        }
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("marker-42"), "output: {text}");
        assert!(term.exited());
        assert!(offset > 0);

        // A dead terminal is replaced on the next open.
        let fresh = mgr.open_default("s1", dir.path(), 80, 24).unwrap();
        assert_ne!(fresh.id, term.id);

        let second = mgr.create("s1", dir.path(), 80, 24).unwrap();
        assert_ne!(fresh.id, second.id);
        assert_eq!(mgr.list_session("s1").len(), 3);
        mgr.remove(&second.id);
        assert_eq!(mgr.list_session("s1").len(), 2);
        mgr.remove_session("s1");
        assert!(mgr.for_session("s1").is_none());
        assert!(mgr.create("s1", dir.path(), 80, 24).is_err());
        mgr.reopen_session("s1");
        assert!(mgr.create("s1", dir.path(), 80, 24).is_ok());
    }

    #[test]
    fn shutdown_kills_shells_and_rejects_new_ones() {
        let mgr = TerminalManager::default();
        let dir = tempfile::tempdir().unwrap();
        let term = mgr.create("s1", dir.path(), 80, 24).unwrap();

        mgr.shutdown();

        assert!(mgr.for_session("s1").is_none());
        assert!(mgr.create("s2", dir.path(), 80, 24).is_err());
        for _ in 0..100 {
            if term.exited() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(term.exited());
    }

    #[test]
    fn drop_kills_shell_held_by_reader_thread() {
        let dir = tempfile::tempdir().unwrap();
        let term = {
            let mgr = TerminalManager::default();
            let term = mgr.create("s1", dir.path(), 80, 24).unwrap();
            drop(mgr);
            term
        };

        for _ in 0..100 {
            if term.exited() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(term.exited());
    }
}
