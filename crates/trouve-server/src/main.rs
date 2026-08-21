//! Standalone server binary: `trouve-server [--addr 127.0.0.1:7433]`.
//! Hosted and self-hosted deployments run this; the desktop app embeds the
//! same [`trouve_server::bind_local`] stack in-process (ADR 0008).

fn version_requested() -> bool {
    std::env::args_os()
        .skip(1)
        .any(|argument| argument == "--version" || argument == "-V")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if version_requested() {
        println!("trouve-server {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

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

    let security = trouve_server::ServerSecurity::resolve();
    let binding = trouve_server::bind_local(addr, security).await?;
    let address = binding.address();
    let Some(server) = binding.into_server() else {
        tracing::info!(%address, "a local trouve server already owns this data directory");
        return Ok(());
    };
    server.await
}
