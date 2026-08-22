//! Standalone server binary: `trouve-server [--addr 127.0.0.1:7433]`.
//! Hosted and self-hosted deployments run this; the desktop app embeds the
//! same [`trouve_server::bind_local`] stack in-process (ADR 0008).

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "trouve-server",
    version,
    about = "Headless HTTP/SSE server for the trouve coding harness."
)]
struct Cli {
    /// Address on which to serve the protocol.
    #[arg(long, default_value = "127.0.0.1:7433")]
    addr: std::net::SocketAddr,
    /// Do not replace the on-disk server binary when a newer release exists.
    #[arg(long)]
    no_auto_update: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check for and install the latest stable release.
    Update {
        /// Report whether an update exists without installing it.
        #[arg(long)]
        check: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    trouve_server::install_crypto_provider();

    if let Some(Command::Update { check }) = cli.command {
        return run_update(check).await;
    }

    let automatic_update = (!cli.no_auto_update && trouve_update::auto_update_enabled())
        .then(|| trouve_update::InstallationBaseline::capture(env!("CARGO_PKG_VERSION")));

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let security = trouve_server::ServerSecurity::resolve();
    let binding = trouve_server::bind_local(cli.addr, security).await?;
    let address = binding.address();
    let Some(server) = binding.into_server() else {
        tracing::info!(%address, "a local trouve server already owns this data directory");
        return Ok(());
    };
    if let Some(automatic_update) = automatic_update {
        match automatic_update {
            Ok(baseline) => {
                tokio::spawn(async move {
                    match trouve_update::install_latest_automatically(
                        trouve_update::Component::Server,
                        baseline,
                    )
                    .await
                    {
                        Ok(trouve_update::UpdateStatus::Updated { from, to }) => {
                            tracing::info!(
                                "installed trouve-server {to} over {from}; restart the service to use it"
                            );
                        }
                        Ok(trouve_update::UpdateStatus::UpToDate { .. }) => {}
                        Err(error) => tracing::warn!("automatic update failed: {error:#}"),
                    }
                });
            }
            Err(error) => tracing::warn!("automatic update is unavailable: {error:#}"),
        }
    }
    server.await
}

async fn run_update(check_only: bool) -> anyhow::Result<()> {
    if check_only {
        let check =
            trouve_update::check(trouve_update::Component::Server, env!("CARGO_PKG_VERSION"))
                .await?;
        let Some(release) = check.update else {
            println!("trouve-server {} is up to date.", check.current);
            return Ok(());
        };
        println!(
            "trouve-server {} is available (current {}).",
            release.version, check.current
        );
        return Ok(());
    }

    match trouve_update::install_latest(trouve_update::Component::Server, env!("CARGO_PKG_VERSION"))
        .await?
    {
        trouve_update::UpdateStatus::UpToDate { version } => {
            println!("trouve-server {version} is up to date.");
        }
        trouve_update::UpdateStatus::Updated { from, to } => {
            println!(
                "Updated trouve-server from {from} to {to}. Restart running server processes to use it."
            );
        }
    }
    Ok(())
}
