//! Shared vendor-CLI login orchestration: spawn `<vendor> login`, scrape the
//! first URL (and short user code, when printed) from its output, and expose
//! a future that resolves when the CLI exits.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::{BackendError, BackendLogin};

/// How long to wait for the vendor CLI to print its verification URL before
/// returning without one (the flow keeps running either way).
const URL_WAIT: Duration = Duration::from_secs(15);

pub async fn spawn_login(command: &str, args: &[&str]) -> Result<BackendLogin, BackendError> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotInstalled(command.to_string()),
            _ => BackendError::Io(e),
        })?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(64);

    // Detached readers; they end when the pipes close.
    tokio::spawn(pump_lines(BufReader::new(stdout), line_tx.clone()));
    tokio::spawn(pump_lines(BufReader::new(stderr), line_tx));

    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<(String, Option<String>)>();
    tokio::spawn(async move {
        let mut url_tx = Some(url_tx);
        // Keep draining after the URL is found so the child never blocks on
        // a full pipe.
        while let Some(line) = line_rx.recv().await {
            tracing::debug!(target: "trouve_agents::login", "{line}");
            if let (Some(url), Some(tx)) = (find_url(&line), url_tx.take()) {
                let _ = tx.send((url, find_user_code(&line)));
            }
        }
    });

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
        done,
    })
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

fn find_url(line: &str) -> Option<String> {
    let start = line.find("https://").or_else(|| line.find("http://"))?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
        .collect();
    // Trim trailing punctuation that often follows URLs in prose.
    Some(url.trim_end_matches(['.', ',', ')', ']']).to_string())
}

/// Device-flow style codes ("Enter code: ABCD-1234").
fn find_user_code(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    let idx = lower.find("code")?;
    let rest = &line[idx + 4..];
    let code: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (code.len() >= 6).then_some(code)
}

#[cfg(test)]
mod tests {
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
    fn extracts_user_codes() {
        assert_eq!(
            find_user_code("Enter code: ABCD-1234"),
            Some("ABCD-1234".to_string())
        );
        assert_eq!(find_user_code("decode this"), None);
    }
}
