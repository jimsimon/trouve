//! Fetch a URL and return its content as readable text.

use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

/// Raw bytes downloaded at most (guards against huge pages).
const MAX_FETCH_BYTES: usize = 4 * 1024 * 1024;
/// Text returned to the model at most; page through with `offset`.
const MAX_RETURN_CHARS: usize = 48 * 1024;
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub struct WebFetch;

#[async_trait::async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> &'static str {
        "Fetch an http(s) URL and return its content as readable text (HTML is converted to \
         markdown-ish text). Use offset to page through long pages."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Absolute http:// or https:// URL"},
                "offset": {"type": "integer", "description": "Character offset to continue from (from a previous truncated fetch)", "minimum": 0}
            },
            "required": ["url"]
        })
    }
    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, _ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(url) = args.get("url").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: url");
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return ToolResult::error("only http:// and https:// URLs are supported");
        }
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;

        let client = match reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent("trouve-agent/1.0")
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("http client: {e}")),
        };
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("fetch failed: {e}")),
        };
        let status = resp.status();
        let final_url = resp.url().to_string();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !status.is_success() {
            return ToolResult::error(format!("{status} fetching {final_url}"));
        }

        // Stream up to the byte cap instead of trusting Content-Length.
        let mut bytes: Vec<u8> = Vec::new();
        let mut resp = resp;
        while let Ok(Some(chunk)) = resp.chunk().await {
            if bytes.len() + chunk.len() > MAX_FETCH_BYTES {
                bytes.extend_from_slice(&chunk[..MAX_FETCH_BYTES - bytes.len()]);
                break;
            }
            bytes.extend_from_slice(&chunk);
        }

        let is_html = content_type.contains("text/html")
            || content_type.contains("application/xhtml")
            || (content_type.is_empty() && bytes.trim_ascii_start().starts_with(b"<"));
        let looks_texty = content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml")
            || content_type.contains("javascript")
            || content_type.is_empty();
        if !is_html && !looks_texty {
            return ToolResult::error(format!(
                "unsupported content type \"{content_type}\" (binary content)"
            ));
        }

        let text = if is_html {
            // Blocking CPU-bound parse; keep it off the async threads.
            match tokio::task::spawn_blocking(move || html2text::from_read(bytes.as_slice(), 100))
                .await
            {
                Ok(Ok(t)) => t,
                Ok(Err(e)) => return ToolResult::error(format!("cannot render HTML: {e}")),
                Err(e) => return ToolResult::error(format!("HTML render panicked: {e}")),
            }
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };

        let total_chars = text.chars().count();
        let page: String = text.chars().skip(offset).take(MAX_RETURN_CHARS).collect();
        let truncated = offset + page.chars().count() < total_chars;
        ToolResult::ok(json!({
            "url": final_url,
            "content": page,
            "truncated": truncated,
            "total_chars": total_chars,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_http_urls() {
        let ctx = ToolCtx::default();
        let res = WebFetch
            .run(&ctx, &json!({"url": "file:///etc/passwd"}))
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        let res = WebFetch.run(&ctx, &json!({"url": "ftp://x/y"})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
    }

    #[tokio::test]
    async fn fetches_html_as_text_and_pages_with_offset() {
        // A local HTTP server keeps the test hermetic.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let body = "<html><body><h1>Title</h1><p>Hello <b>world</b>.</p></body></html>";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });

        let ctx = ToolCtx::default();
        let res = WebFetch
            .run(&ctx, &json!({"url": format!("http://{addr}/")}))
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        let content = res.result["content"].as_str().unwrap();
        assert!(content.contains("Title"), "content: {content}");
        assert!(content.contains("Hello"), "content: {content}");
        assert_eq!(res.result["truncated"], false);

        // Offset past the end returns an empty page, still ok.
        let res = WebFetch
            .run(
                &ctx,
                &json!({"url": format!("http://{addr}/"), "offset": 1_000_000}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(res.result["content"], "");
    }
}
