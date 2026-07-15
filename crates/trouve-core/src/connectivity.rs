//! Internet-reachability probing for the engine.
//!
//! The server is the process that talks to model vendors, so it owns the
//! online/offline state (clients read it from `ServerInfo.online` and the
//! `server.connectivity_changed` event). Probing is opt-in: an engine
//! without a probe always reports online, which keeps `cargo test` and
//! embedded engines offline-safe. The standalone server binary wires
//! [`system_probe`] in.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

/// An async check answering "can we reach the internet right now?".
pub type Probe = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;

/// Overall probe deadline. Generous enough for a slow handshake, short
/// enough that a cold offline start doesn't stall the server noticeably.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Re-probe interval while online (detects going offline).
pub const ONLINE_POLL: Duration = Duration::from_secs(30);
/// Re-probe interval while offline (detects recovery quickly — the UI
/// re-enables prompt entry off this signal).
pub const OFFLINE_POLL: Duration = Duration::from_secs(5);

/// Real connectivity probe: concurrent TCP dials to well-known anycast
/// resolvers plus a DNS lookup (covers networks where direct-to-IP egress
/// is blocked but a resolver/proxy path works). Any success means online.
pub fn system_probe() -> Probe {
    Arc::new(|| Box::pin(probe_system()))
}

async fn probe_system() -> bool {
    async fn tcp(addr: &'static str) -> Result<(), ()> {
        tokio::net::TcpStream::connect(addr)
            .await
            .map(|_| ())
            .map_err(|_| ())
    }
    async fn dns() -> Result<(), ()> {
        match tokio::net::lookup_host("api.openai.com:443").await {
            Ok(mut addrs) => addrs.next().map(|_| ()).ok_or(()),
            Err(_) => Err(()),
        }
    }
    let checks: Vec<BoxFuture<'static, Result<(), ()>>> = vec![
        Box::pin(tcp("1.1.1.1:443")),
        Box::pin(tcp("8.8.8.8:443")),
        Box::pin(tcp("9.9.9.9:443")),
        Box::pin(dns()),
    ];
    tokio::time::timeout(PROBE_TIMEOUT, futures::future::select_ok(checks))
        .await
        .map(|first_ok| first_ok.is_ok())
        .unwrap_or(false)
}
