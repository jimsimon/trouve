//! Integrated terminal: one interactive shell per session, spawned in the
//! session's worktree over a PTY.
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

use anyhow::{anyhow, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::broadcast;

/// Retained output per terminal. Enough for a few thousand lines of
/// scrollback; older bytes are dropped (offsets keep counting).
const BACKLOG_CAP: usize = 512 * 1024;

#[derive(Default)]
pub struct TerminalManager {
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    /// session id → terminal id (one live terminal per session).
    by_session: Mutex<HashMap<String, String>>,
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
        // Order matters: open the receiver first, then snapshot; duplicates
        // at the boundary are dropped by offset bookkeeping client-side.
        let rx = self.live.subscribe();
        let backlog = self.backlog.lock().unwrap();
        let from = after.max(backlog.start);
        let skip = (from - backlog.start) as usize;
        let replay = backlog.data.get(skip..).unwrap_or_default().to_vec();
        (from, replay, rx)
    }
}

impl TerminalManager {
    /// The session's live terminal, spawning one (a login-ish shell in the
    /// worktree) if there is none or the previous shell exited.
    pub fn open(
        &self,
        session_id: &str,
        worktree: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Terminal>> {
        if let Some(existing) = self.for_session(session_id) {
            if !existing.exited() {
                return Ok(existing);
            }
            self.remove(&existing.id);
        }

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
            .insert(session_id.to_string(), terminal.id.clone());

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
                            {
                                let mut backlog = terminal.backlog.lock().unwrap();
                                backlog.data.extend_from_slice(&chunk);
                                if backlog.data.len() > BACKLOG_CAP {
                                    let drop_n = backlog.data.len() - BACKLOG_CAP;
                                    backlog.data.drain(..drop_n);
                                    backlog.start += drop_n as u64;
                                }
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
        let id = self.by_session.lock().unwrap().get(session_id).cloned()?;
        self.terminals.lock().unwrap().get(&id).cloned()
    }

    /// Kill and forget a terminal (explicit close or restart).
    pub fn remove(&self, terminal_id: &str) {
        let removed = self.terminals.lock().unwrap().remove(terminal_id);
        if let Some(terminal) = removed {
            terminal.kill();
            let mut by_session = self.by_session.lock().unwrap();
            if by_session.get(&terminal.session_id) == Some(&terminal.id) {
                by_session.remove(&terminal.session_id);
            }
        }
    }

    /// Kill the session's terminal, if any (session delete/archive).
    pub fn remove_session(&self, session_id: &str) {
        // NB: bind before the if-let — the guard would otherwise live for
        // the body, deadlocking with remove()'s own by_session lock.
        let id = self.by_session.lock().unwrap().get(session_id).cloned();
        if let Some(id) = id {
            self.remove(&id);
        }
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
        let term = mgr.open("s1", dir.path(), 80, 24).unwrap();
        assert!(!term.exited());

        // Re-open returns the same live terminal.
        let again = mgr.open("s1", dir.path(), 80, 24).unwrap();
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
        let fresh = mgr.open("s1", dir.path(), 80, 24).unwrap();
        assert_ne!(fresh.id, term.id);
        mgr.remove_session("s1");
        assert!(mgr.for_session("s1").is_none());
    }
}
