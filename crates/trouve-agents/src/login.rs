//! Shared vendor-CLI login orchestration: spawn `<vendor> login`, scrape the
//! first URL (and short user code, when printed) from its output, and expose
//! a future that resolves when the CLI exits.

use std::process::Stdio;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::{BackendError, BackendLogin};

/// How long to wait for the vendor CLI to print its verification URL before
/// returning without one (the flow keeps running either way).
const URL_WAIT: Duration = Duration::from_secs(15);

pub async fn spawn_login(command: &str, args: &[&str]) -> Result<BackendLogin, BackendError> {
    spawn_login_inner(command, args, false).await
}

/// Start Codex's device-code flow.
///
/// Trouve may be serving a browser on a different machine from the process
/// running the CLI, so Codex's default localhost callback cannot reliably
/// reach its listener. Device auth is designed for that split and prints a
/// remote verification URL plus a one-time code.
pub async fn spawn_codex_login(command: &str) -> Result<BackendLogin, BackendError> {
    spawn_login_inner(command, &["login", "--device-auth"], true).await
}

/// Start Claude Code's subscription login in a PTY.
///
/// Claude buffers the prompt when stdout is a pipe and requires stdin when a
/// remote browser cannot reach its localhost callback. A PTY makes the URL
/// available immediately and lets the client paste that callback back in.
pub async fn spawn_claude_login(command: &str) -> Result<BackendLogin, BackendError> {
    if !crate::binary_on_path(command) {
        return Err(BackendError::NotInstalled(command.to_string()));
    }
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(pty_error)?;

    let mut cmd = CommandBuilder::new(command);
    cmd.args(["auth", "login", "--claudeai"]);
    cmd.env("TERM", "xterm-256color");
    // The review client opens the captured URL in the user's browser.
    #[cfg(unix)]
    cmd.env("BROWSER", "true");
    let mut child = pair.slave.spawn_command(cmd).map_err(pty_error)?;
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().map_err(pty_error)?;
    let mut writer = pair.master.take_writer().map_err(pty_error)?;
    drop(pair.master);

    let (line_tx, line_rx) = tokio::sync::mpsc::channel::<String>(64);
    std::thread::spawn(move || pump_blocking_lines(reader, line_tx));
    let url_rx = instruction_receiver(line_rx, false);

    let (callback_tx, mut callback_rx) = tokio::sync::mpsc::channel::<String>(1);
    std::thread::spawn(move || {
        if let Some(callback) = callback_rx.blocking_recv() {
            use std::io::Write as _;
            let _ = writer.write_all(callback.as_bytes());
            let _ = writer.write_all(b"\n");
            let _ = writer.flush();
        }
    });

    let done = Box::pin(async move {
        let status = tokio::task::spawn_blocking(move || child.wait())
            .await
            .map_err(|e| BackendError::Io(std::io::Error::other(e.to_string())))?
            .map_err(BackendError::Io)?;
        if status.success() {
            Ok(())
        } else {
            Err(BackendError::Auth(format!(
                "login command exited with code {}",
                status.exit_code()
            )))
        }
    });

    let (verification_url, user_code) = match tokio::time::timeout(URL_WAIT, url_rx).await {
        Ok(Ok((url, code))) => (Some(url), code),
        _ => (None, None),
    };

    Ok(BackendLogin {
        verification_url,
        user_code,
        callback_sender: Some(callback_tx),
        done,
    })
}

fn pty_error(error: impl ToString) -> BackendError {
    BackendError::Io(std::io::Error::other(error.to_string()))
}

async fn spawn_login_inner(
    command: &str,
    args: &[&str],
    require_user_code: bool,
) -> Result<BackendLogin, BackendError> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Vendor CLIs auto-open a browser and honor $BROWSER over the
    // desktop's default handler, which launches the wrong browser when
    // the variable points elsewhere (e.g. BROWSER=firefox on a KDE
    // session whose default is another browser). Neutralize their launch
    // — `true` swallows the URL argument — and let the client open the
    // scraped verification URL once, through the desktop default
    // (xdg-open / open).
    #[cfg(unix)]
    cmd.env("BROWSER", "true");
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => BackendError::NotInstalled(command.to_string()),
        _ => BackendError::Io(e),
    })?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let (line_tx, line_rx) = tokio::sync::mpsc::channel::<String>(64);

    // Detached readers; they end when the pipes close.
    tokio::spawn(pump_lines(BufReader::new(stdout), line_tx.clone()));
    tokio::spawn(pump_lines(BufReader::new(stderr), line_tx));

    let url_rx = instruction_receiver(line_rx, require_user_code);
    let done = Box::pin(async move {
        let status = child.wait().await.map_err(BackendError::Io)?;
        if status.success() {
            Ok(())
        } else {
            Err(BackendError::Auth(format!(
                "login command exited with {status}"
            )))
        }
    });

    let (verification_url, user_code) = match tokio::time::timeout(URL_WAIT, url_rx).await {
        Ok(Ok((url, code))) => (Some(url), code),
        _ => (None, None),
    };

    Ok(BackendLogin {
        verification_url,
        user_code,
        callback_sender: None,
        done,
    })
}

fn instruction_receiver(
    mut line_rx: tokio::sync::mpsc::Receiver<String>,
    require_user_code: bool,
) -> tokio::sync::oneshot::Receiver<(String, Option<String>)> {
    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<(String, Option<String>)>();
    tokio::spawn(async move {
        let mut url_tx = Some(url_tx);
        let mut instructions = LoginInstructions::default();
        // Keep draining after the URL is found so the child never blocks on
        // a full pipe.
        while let Some(line) = line_rx.recv().await {
            instructions.observe(&line);
            // Codex's device prompt prints the URL and code on separate
            // lines, so wait for both. Other vendor flows only require a URL.
            if let Some((url, code)) = instructions.ready(require_user_code)
                && let Some(tx) = url_tx.take()
            {
                let _ = tx.send((url, code));
            }
        }
    });
    url_rx
}

#[derive(Default)]
struct LoginInstructions {
    verification_url: Option<String>,
    user_code: Option<String>,
    code_on_next_line: bool,
}

impl LoginInstructions {
    fn observe(&mut self, line: &str) {
        let line = strip_ansi(line);
        if self.verification_url.is_none() {
            self.verification_url = find_url(&line);
        }
        if self.user_code.is_none() {
            self.user_code = find_user_code(&line);
            if self.user_code.is_none() && self.code_on_next_line {
                self.user_code = find_standalone_user_code(&line);
            }
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("enter this one-time code") || lower.contains("enter code") {
            self.code_on_next_line = true;
        }
    }

    fn ready(&self, require_user_code: bool) -> Option<(String, Option<String>)> {
        let url = self.verification_url.clone()?;
        if require_user_code && self.user_code.is_none() {
            return None;
        }
        Some((url, self.user_code.clone()))
    }
}

async fn pump_lines<R: tokio::io::AsyncRead + Unpin>(
    reader: BufReader<R>,
    tx: tokio::sync::mpsc::Sender<String>,
) {
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if tx.send(line).await.is_err() {
            break;
        }
    }
}

fn pump_blocking_lines(
    reader: Box<dyn std::io::Read + Send>,
    tx: tokio::sync::mpsc::Sender<String>,
) {
    use std::io::BufRead as _;

    let lines = std::io::BufReader::new(reader).lines();
    for line in lines.map_while(Result::ok) {
        if tx.blocking_send(line).is_err() {
            break;
        }
    }
}

fn find_url(line: &str) -> Option<String> {
    let line = strip_ansi(line);
    let start = line.find("https://").or_else(|| line.find("http://"))?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\'' && *c != '\u{1b}')
        .collect();
    // Trim trailing punctuation that often follows URLs in prose.
    let url = url.trim_end_matches(['.', ',', ')', ']']);
    // Loopback URLs are the CLI's own redirect listener, not the page the
    // user must visit (codex prints "Starting local login server on
    // http://localhost:1455." before the real auth URL; opening it renders
    // "Not found"). Skip them and keep scanning for a remote URL.
    let host = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or("");
    if matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]") {
        return None;
    }
    Some(url.to_string())
}

/// Device-flow style codes ("Enter code: ABCD-1234").
fn find_user_code(line: &str) -> Option<String> {
    let line = strip_ansi(line);
    // Find "code" case-insensitively on the original string, so the byte
    // offset stays valid on `line` (to_lowercase can change byte length for
    // non-ASCII, which would misindex or panic the slice).
    let lower = line.to_lowercase();
    let match_at = lower.find("code")?;
    // Map the lowercase byte offset back to a char count, then to a byte
    // offset in the original string.
    let char_idx = lower[..match_at].chars().count();
    let idx: usize = line
        .char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let rest = &line[idx..];
    // Skip the matched "code" (4 ASCII chars) via the char iterator.
    let code: String = rest
        .chars()
        .skip(4)
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    valid_user_code(&code).then_some(code)
}

fn find_standalone_user_code(line: &str) -> Option<String> {
    let line = strip_ansi(line);
    let code = line.trim();
    valid_user_code(code).then(|| code.to_string())
}

fn valid_user_code(code: &str) -> bool {
    code.len() >= 6
        && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && code.chars().any(|c| c.is_ascii_digit() || c == '-')
}

/// Remove ANSI control-sequence introducer escapes from vendor CLI output.
fn strip_ansi(line: &str) -> String {
    let mut clean = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for part in chars.by_ref() {
                if ('@'..='~').contains(&part) {
                    break;
                }
            }
        } else {
            clean.push(c);
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn extracts_urls() {
        assert_eq!(
            find_url("Open https://auth.example.com/x?y=1 to continue."),
            Some("https://auth.example.com/x?y=1".to_string())
        );
        assert_eq!(find_url("no url here"), None);
    }

    #[test]
    fn skips_loopback_urls() {
        // codex prints its redirect listener before the real auth URL.
        assert_eq!(
            find_url("Starting local login server on http://localhost:1455."),
            None
        );
        assert_eq!(find_url("listening on http://127.0.0.1:8080/cb"), None);
        // A localhost redirect_uri inside the query must not disqualify
        // the remote auth URL that carries it.
        assert_eq!(
            find_url(
                "https://auth.openai.com/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&state=x"
            ),
            Some(
                "https://auth.openai.com/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&state=x"
                    .to_string()
            )
        );
    }

    #[test]
    fn extracts_user_codes() {
        assert_eq!(
            find_user_code("Enter code: ABCD-1234"),
            Some("ABCD-1234".to_string())
        );
        assert_eq!(find_user_code("decode this"), None);
        // Non-ASCII before the match must not panic the slice.
        assert_eq!(
            find_user_code("café — code: WXYZ-99"),
            Some("WXYZ-99".to_string())
        );
        assert_eq!(
            find_user_code("2. Enter this one-time code (expires in 15 minutes)"),
            None
        );
    }

    #[test]
    fn collects_codex_device_instructions_across_ansi_lines() {
        let mut instructions = LoginInstructions::default();
        instructions.observe("1. Open this link in your browser and sign in to your account");
        instructions.observe("   \u{1b}[94mhttps://auth.openai.com/codex/device\u{1b}[0m");
        assert_eq!(instructions.ready(true), None);

        instructions
            .observe("2. Enter this one-time code \u{1b}[90m(expires in 15 minutes)\u{1b}[0m");
        instructions.observe("   \u{1b}[94mABCD-1234\u{1b}[0m");
        assert_eq!(
            instructions.ready(true),
            Some((
                "https://auth.openai.com/codex/device".to_string(),
                Some("ABCD-1234".to_string())
            ))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_login_uses_pty_and_forwards_browser_callback() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("fake-claude");
        std::fs::write(
            &command,
            r#"#!/bin/sh
if [ "$1" != "auth" ] || [ "$2" != "login" ] || [ "$3" != "--claudeai" ]; then
  exit 2
fi
printf 'If the browser did not open, visit: https://claude.example.test/oauth\n'
printf 'Paste code here if prompted > '
IFS= read -r callback
if [ "$callback" = "http://localhost:54545/callback?code=test-code&state=test-state" ]; then
  exit 0
fi
exit 3
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&command).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command, permissions).unwrap();

        let login = spawn_claude_login(command.to_str().unwrap()).await.unwrap();
        assert_eq!(
            login.verification_url.as_deref(),
            Some("https://claude.example.test/oauth")
        );
        login
            .callback_sender
            .unwrap()
            .send("http://localhost:54545/callback?code=test-code&state=test-state".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), login.done)
            .await
            .expect("fake Claude login should exit")
            .expect("fake Claude login should accept the callback");
    }
}
