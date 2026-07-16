//! Standalone server binary: `trouve-server [--addr 127.0.0.1:7433]`.

use std::sync::Arc;

use trouve_core::Engine;
use trouve_core::config::{Config, data_dir};
use trouve_core::store::Store;

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

    let data = data_dir();
    let store = Store::open(&data.join("trouve.db"))?;
    let config = Config::load();
    let security = trouve_server::ServerSecurity::resolve(&data);
    let engine = Arc::new(
        Engine::new(store, data, &config)
            // This engine loaded the real config file, so provider changes
            // write back to it.
            .with_config_file(Some(trouve_core::config::config_path()))
            .with_index_hooks(),
    );
    trouve_server::serve(engine, addr, security).await
}
