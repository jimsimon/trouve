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
            // Only consume the one-shot sender when this line actually
            // holds a URL; taking it eagerly in a tuple pattern burned it
            // on the first URL-less line, dropping the real URL later.
            if url_tx.is_some()
                && let Some(url) = find_url(&line)
            {
                let _ = url_tx
                    .take()
                    .expect("checked above")
                    .send((url, find_user_code(&line)));
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
    }
}
