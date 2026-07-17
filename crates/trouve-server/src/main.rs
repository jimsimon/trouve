//! Standalone server binary: `trouve-server [--addr 127.0.0.1:7433]`.
//! Hosted and self-hosted deployments run this; the desktop app embeds the
//! same [`trouve_server::bind_local`] stack in-process (ADR 0008).

use trouve_core::config::data_dir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let addr = std::env::args()
        .skip_while(|a| a != "--addr")
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:7433".into())
        .parse()?;

    let security = trouve_server::ServerSecurity::resolve(&data_dir());
    let (_, server) = trouve_server::bind_local(addr, security).await?;
    server.await
}
