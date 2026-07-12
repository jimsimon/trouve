//! Fetch a URL and return its content as readable text.

use std::net::{IpAddr, SocketAddr};

use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

/// Raw bytes downloaded at most (guards against huge pages).
const MAX_FETCH_BYTES: usize = 4 * 1024 * 1024;
/// Text returned to the model at most; page through with `offset`.
const MAX_RETURN_CHARS: usize = 48 * 1024;
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Redirects are followed manually so every hop is re-validated.
const MAX_REDIRECTS: usize = 5;

#[derive(Default)]
pub struct WebFetch {
    /// Test hook: hermetic tests fetch from 127.0.0.1, which the SSRF guard
    /// refuses by default.
    pub allow_private: bool,
}

/// Whether an address is publicly routable. The agent must not be able to
/// reach loopback services, cloud metadata endpoints (169.254.169.254), or
/// anything on the private network (SSRF).
fn ip_is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            let cgnat = octets[0] == 100 && (octets[1] & 0xc0) == 64; // 100.64/10
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || cgnat
                || octets[0] == 0)
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return ip_is_public(IpAddr::V4(mapped));
            }
            let unique_local = (v6.segments()[0] & 0xfe00) == 0xfc00; // fc00::/7
            let link_local = (v6.segments()[0] & 0xffc0) == 0xfe80; // fe80::/10
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || unique_local
                || link_local)
        }
    }
}

/// Resolve the URL's host and return the validated socket addresses to pin
/// the connection to, or an error naming the offending address.
async fn checked_addrs(url: &reqwest::Url, allow_private: bool) -> Result<Vec<SocketAddr>, String> {
    let Some(host) = url.host_str() else {
        return Err("URL has no host".to_string());
    };
    let port = url.port_or_known_default().unwrap_or(80);
    // IPv6 literals appear bracketed in host_str.
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = if let Ok(ip) = literal.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        match tokio::net::lookup_host((host, port)).await {
            Ok(iter) => iter.collect(),
            Err(e) => return Err(format!("cannot resolve {host}: {e}")),
        }
    };
    if addrs.is_empty() {
        return Err("host resolved to no addresses".to_string());
    }
    if !allow_private && let Some(bad) = addrs.iter().find(|a| !ip_is_public(a.ip())) {
        return Err(format!(
            "{} resolves to non-public address {}; refusing to fetch",
            url.host_str().unwrap_or("host"),
            bad.ip()
        ));
    }
    Ok(addrs)
}

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

        // Follow redirects manually, re-validating the target of every hop:
        // reqwest's automatic redirects would happily hop from a public URL
        // to 127.0.0.1 or the cloud metadata service. Connections are pinned
        // to the validated addresses so DNS can't rebind between the check
        // and the connect.
        let mut current = match reqwest::Url::parse(url) {
            Ok(u) => u,
            Err(e) => return ToolResult::error(format!("invalid URL: {e}")),
        };
        let mut redirects = 0;
        let resp = loop {
            if !matches!(current.scheme(), "http" | "https") {
                return ToolResult::error("only http:// and https:// URLs are supported");
            }
            let addrs = match checked_addrs(&current, self.allow_private).await {
                Ok(a) => a,
                Err(e) => return ToolResult::error(e),
            };
            let mut builder = reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .user_agent("trouve-agent/1.0")
                .redirect(reqwest::redirect::Policy::none());
            if let Some(host) = current.host_str() {
                builder = builder.resolve_to_addrs(host, &addrs);
            }
            let client = match builder.build() {
                Ok(c) => c,
                Err(e) => return ToolResult::error(format!("http client: {e}")),
            };
            let resp = match client.get(current.clone()).send().await {
                Ok(r) => r,
                Err(e) => return ToolResult::error(format!("fetch failed: {e}")),
            };
            if resp.status().is_redirection() {
                redirects += 1;
                if redirects > MAX_REDIRECTS {
                    return ToolResult::error("too many redirects");
                }
                let Some(location) = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                else {
                    return ToolResult::error(format!(
                        "{} redirect without a Location header",
                        resp.status()
                    ));
                };
                current = match current.join(location) {
                    Ok(u) => u,
                    Err(e) => return ToolResult::error(format!("invalid redirect target: {e}")),
                };
                continue;
            }
            break resp;
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
        let tool = WebFetch::default();
        let res = tool.run(&ctx, &json!({"url": "file:///etc/passwd"})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        let res = tool.run(&ctx, &json!({"url": "ftp://x/y"})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
    }

    #[tokio::test]
    async fn refuses_non_public_addresses_by_default() {
        let ctx = ToolCtx::default();
        let tool = WebFetch::default();
        for url in [
            "http://127.0.0.1/",
            "http://localhost/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://100.64.0.1/",
            "http://[::1]/",
            "http://[fd00::1]/",
        ] {
            let res = tool.run(&ctx, &json!({ "url": url })).await;
            assert_eq!(
                res.status,
                trouve_protocol::ToolStatus::Error,
                "expected refusal for {url}: {:?}",
                res.result
            );
            let msg = res.result["error"].as_str().unwrap_or_default();
            assert!(
                msg.contains("non-public") || msg.contains("cannot resolve"),
                "unexpected error for {url}: {msg}"
            );
        }
    }

    #[test]
    fn public_ip_classification() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd12::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!ip_is_public(bad.parse().unwrap()), "{bad} must be private");
        }
        for good in ["93.184.216.34", "8.8.8.8", "2606:2800:220:1::1"] {
            assert!(ip_is_public(good.parse().unwrap()), "{good} must be public");
        }
    }

    /// Serve one canned response per connection on an ephemeral port.
    async fn serve(resp_for_path: impl Fn(&str) -> String + Send + Sync + 'static) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let resp_for_path = std::sync::Arc::new(resp_for_path);
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let resp_for_path = resp_for_path.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let _ = sock.write_all(resp_for_path(&path).as_bytes()).await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn fetches_html_as_text_and_pages_with_offset() {
        let addr = serve(|_| {
            let body = "<html><body><h1>Title</h1><p>Hello <b>world</b>.</p></body></html>";
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
        })
        .await;

        let ctx = ToolCtx::default();
        let tool = WebFetch {
            allow_private: true,
        };
        let res = tool
            .run(&ctx, &json!({"url": format!("http://{addr}/")}))
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        let content = res.result["content"].as_str().unwrap();
        assert!(content.contains("Title"), "content: {content}");
        assert!(content.contains("Hello"), "content: {content}");
        assert_eq!(res.result["truncated"], false);

        // Offset past the end returns an empty page, still ok.
        let res = tool
            .run(
                &ctx,
                &json!({"url": format!("http://{addr}/"), "offset": 1_000_000}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(res.result["content"], "");
    }

    #[tokio::test]
    async fn follows_and_validates_redirects() {
        let addr = serve(|path| {
            if path == "/start" {
                "HTTP/1.1 302 Found\r\nlocation: /end\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_string()
            } else {
                let body = "made it";
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
            }
        })
        .await;

        let ctx = ToolCtx::default();
        let tool = WebFetch {
            allow_private: true,
        };
        let res = tool
            .run(&ctx, &json!({"url": format!("http://{addr}/start")}))
            .await;
        assert_eq!(
            res.status,
            trouve_protocol::ToolStatus::Ok,
            "{:?}",
            res.result
        );
        assert_eq!(res.result["content"], "made it");

        // A redirect loop trips the hop limit rather than spinning.
        let loop_addr = serve(|_| {
            "HTTP/1.1 302 Found\r\nlocation: /again\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                .to_string()
        })
        .await;
        let res = tool
            .run(&ctx, &json!({"url": format!("http://{loop_addr}/")}))
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        assert!(
            res.result["error"]
                .as_str()
                .unwrap_or_default()
                .contains("too many redirects")
        );
    }
}
