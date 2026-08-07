//! Hardened gateway bootstrap for the isolated Servo qualification workspace.
//!
//! This package is deliberately unable to link trouve-server. It can only
//! connect to the explicit TROUVE_SERVER_URL supplied by the launcher, so it
//! cannot become a second owner of Trouve's SQLite database.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use trouve_client_core::{
    client::ProtocolClient, protocol_compatibility::ensure_compatible_protocol,
};
use trouve_desktop_host::{
    FrontendSource, HostCapabilities, HostGateway, HostNativeActions, HostPreferences,
    VerifiedSessionFile,
};
use trouve_protocol::PROTOCOL_VERSION;

const SERVER_URL_ENV: &str = "TROUVE_SERVER_URL";

/// Hardened loopback gateway and the runtime that owns it.
pub struct WebPreviewHost {
    gateway_origin: String,
    gateway_task: Option<JoinHandle<()>>,
    runtime: Option<Runtime>,
    _host_storage: TempDir,
}

impl WebPreviewHost {
    /// Verify the explicit protocol server and serve packaged frontend assets
    /// through the desktop gateway. No protocol server or durable store is
    /// opened by this process.
    pub fn start(frontend: FrontendSource, native_actions: HostNativeActions) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let upstream = required_server_url(std::env::var(SERVER_URL_ENV).ok())?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("creating the embedded Servo gateway runtime")?;

        let protocol = ProtocolClient::new(&upstream);
        let server_info = runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), protocol.info())
                    .await
                    .context("timed out after 5 seconds")?
            })
            .with_context(|| format!("connecting embedded Servo preview to {upstream}"))?;
        ensure_compatible_protocol(&server_info.protocol_version, PROTOCOL_VERSION)
            .with_context(|| format!("connecting embedded Servo preview to {upstream}"))?;
        let file_protocol = protocol.clone();
        let native_actions =
            native_actions.with_session_file_resolver(move |session_id, relative_path| {
                let file_protocol = file_protocol.clone();
                async move {
                    let sessions = file_protocol
                        .list_sessions()
                        .await
                        .map_err(|_| "session lookup failed".to_string())?;
                    let worktree = sessions
                        .into_iter()
                        .find(|session| session.id == session_id)
                        .map(|session| session.worktree_path)
                        .ok_or_else(|| "session is unavailable".to_string())?;
                    tokio::task::spawn_blocking(move || {
                        VerifiedSessionFile::resolve(worktree, relative_path)
                            .map_err(|_| "session file is unavailable".to_string())
                    })
                    .await
                    .map_err(|_| "session file verification was interrupted".to_string())?
                }
            });

        let host_storage =
            tempfile::tempdir().context("creating isolated desktop-host preferences")?;
        let preference_path = host_storage.path().join("web-preferences.json");
        let (gateway_address, gateway) =
            runtime.block_on(HostGateway::bind_loopback_with_actions(
                "127.0.0.1:0"
                    .parse()
                    .expect("static loopback address parses"),
                frontend,
                HostCapabilities::desktop(),
                HostPreferences::default(),
                Some(&upstream),
                Some(preference_path),
                native_actions,
            ))?;
        let gateway_task = runtime.spawn(async move {
            if let Err(error) = gateway.await {
                tracing::error!(%error, "embedded Servo desktop gateway stopped");
            }
        });

        Ok(Self {
            gateway_origin: format!("http://{gateway_address}"),
            gateway_task: Some(gateway_task),
            runtime: Some(runtime),
            _host_storage: host_storage,
        })
    }

    pub fn gateway_origin(&self) -> &str {
        &self.gateway_origin
    }

    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime
            .as_ref()
            .expect("preview runtime remains available until shutdown")
            .handle()
            .clone()
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let gateway_task = self.gateway_task.take();
        let Some(runtime) = self.runtime.take() else {
            return;
        };

        if let Some(gateway_task) = gateway_task {
            gateway_task.abort();
            let result = runtime.block_on(gateway_task);
            if let Err(error) = result
                && !error.is_cancelled()
            {
                tracing::warn!(%error, "joining embedded Servo gateway task failed");
            }
        }
        runtime.shutdown_timeout(Duration::from_secs(1));
    }
}

impl Drop for WebPreviewHost {
    fn drop(&mut self) {
        self.stop();
    }
}

fn required_server_url(value: Option<String>) -> Result<String> {
    let Some(value) = value else {
        bail!(
            "{SERVER_URL_ENV} is required for embedded Servo qualification; start or reuse an isolated trouve-server and set {SERVER_URL_ENV} to its base URL (this harness cannot open the default database)"
        );
    };
    let value = value.trim();
    if value.is_empty() {
        bail!("{SERVER_URL_ENV} cannot be empty");
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_requires_an_explicit_server_url() {
        let error = required_server_url(None).unwrap_err().to_string();
        assert!(error.contains(SERVER_URL_ENV));
        assert!(error.contains("cannot open the default database"));
    }

    #[test]
    fn preview_rejects_a_blank_server_url() {
        let error = required_server_url(Some("  \n".into()))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "TROUVE_SERVER_URL cannot be empty");
    }
}
