//! Shared bootstrap and lifecycle for desktop webview qualification hosts.
//!
//! Preview hosts are clients of an already-running `trouve-server`. They must
//! never silently create a second engine against the default data directory:
//! two engines would contend for SQLite's single WAL writer and would have
//! independent in-memory schedulers and event broadcasts for the same durable
//! state.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use trouve_client_core::{
    client::ProtocolClient, protocol_compatibility::ensure_compatible_protocol,
};
use trouve_desktop_host::{
    FrontendSource, HostCapabilities, HostGateway, HostNativeActions, HostPreferences,
    HostPreferencesHandle, VerifiedSessionFile,
};
use trouve_protocol::PROTOCOL_VERSION;

const SERVER_URL_ENV: &str = "TROUVE_SERVER_URL";

/// Hardened loopback gateway and the runtime that owns it.
///
/// Keep this value alive for the webview's full lifetime. Calling
/// [`Self::shutdown`] is preferred; `Drop` provides the same cancellation as
/// a fallback for host-startup errors after the gateway has been bound.
pub struct WebPreviewHost {
    gateway_origin: String,
    gateway_task: Option<JoinHandle<()>>,
    runtime: Option<Runtime>,
    #[allow(dead_code)]
    initial_preferences: HostPreferences,
    #[allow(dead_code)]
    preferences: HostPreferencesHandle,
}

impl WebPreviewHost {
    /// Connect to the explicitly configured protocol server, verify it is
    /// responsive, and serve the packaged frontend through the desktop
    /// gateway. No protocol server or durable store is opened here.
    // Used by the external Servo qualification binary; this sibling module is
    // also compiled independently for the Wry binary.
    #[allow(dead_code)]
    pub fn start(frontend: FrontendSource) -> Result<Self> {
        Self::start_with_actions(frontend, native_actions())
    }

    /// Start the gateway with an application-owned, event-loop-backed
    /// directory picker. The callback itself is async and never blocks the
    /// gateway runtime while the operating system dialog is open.
    // Used by the Wry qualification binary; this sibling module is also
    // compiled independently for the external Servo runner.
    #[allow(dead_code)]
    pub fn start_with_directory_picker<F, Fut>(frontend: FrontendSource, picker: F) -> Result<Self>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<PathBuf>, String>> + Send + 'static,
    {
        Self::start_with_actions(frontend, native_actions().with_directory_picker(picker))
    }

    /// Start with the complete, explicitly app-owned native action set.
    #[allow(dead_code)]
    pub fn start_with_native_actions(
        frontend: FrontendSource,
        native_actions: HostNativeActions,
    ) -> Result<Self> {
        Self::start_with_actions(frontend, native_actions)
    }

    fn start_with_actions(
        frontend: FrontendSource,
        native_actions: HostNativeActions,
    ) -> Result<Self> {
        trouve_server::install_crypto_provider();
        let upstream = required_server_url(std::env::var(SERVER_URL_ENV).ok())?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("creating the desktop preview runtime")?;

        let protocol = ProtocolClient::new(&upstream);
        let server_info = runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), protocol.info())
                    .await
                    .context("timed out after 5 seconds")?
            })
            .with_context(|| format!("connecting desktop preview to {upstream}"))?;
        ensure_compatible_protocol(&server_info.protocol_version, PROTOCOL_VERSION)
            .with_context(|| format!("connecting desktop preview to {upstream}"))?;
        // Resolve a requested file through the public protocol on every
        // action, then canonicalize it beneath that session's worktree. The
        // gateway advertises this adapter only for a loopback protocol server.
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

        let preference_path = dirs::config_dir()
            .map(|directory| directory.join("trouve").join("web-preferences.json"));
        let (gateway_address, gateway, preferences) =
            runtime.block_on(HostGateway::bind_loopback_with_actions_and_preferences(
                "127.0.0.1:0"
                    .parse()
                    .expect("static loopback address parses"),
                frontend,
                HostCapabilities::desktop(),
                HostPreferences::default(),
                Some(&upstream),
                preference_path,
                native_actions,
            ))?;
        let initial_preferences = runtime.block_on(preferences.snapshot());
        let gateway_task = runtime.spawn(async move {
            if let Err(error) = gateway.await {
                tracing::error!(%error, "desktop frontend gateway stopped");
            }
        });

        Ok(Self {
            gateway_origin: format!("http://{gateway_address}"),
            gateway_task: Some(gateway_task),
            runtime: Some(runtime),
            initial_preferences,
            preferences,
        })
    }

    pub fn gateway_origin(&self) -> &str {
        &self.gateway_origin
    }

    #[allow(dead_code)]
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime
            .as_ref()
            .expect("preview runtime remains available until shutdown")
            .handle()
            .clone()
    }

    #[allow(dead_code)]
    pub fn initial_preferences(&self) -> &HostPreferences {
        &self.initial_preferences
    }

    #[allow(dead_code)]
    pub fn preferences_handle(&self) -> HostPreferencesHandle {
        self.preferences.clone()
    }

    #[allow(dead_code)]
    pub fn persist_window_geometry(
        &self,
        geometry: trouve_desktop_host::WindowGeometry,
    ) -> Result<()> {
        self.runtime
            .as_ref()
            .expect("preview runtime remains available until shutdown")
            .block_on(self.preferences.update_window_geometry(geometry))
            .context("persisting desktop window geometry")
    }

    /// Stop accepting gateway traffic and tear down its runtime before the
    /// preview process exits.
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
                tracing::warn!(%error, "joining desktop frontend gateway task failed");
            }
        }
        runtime.shutdown_timeout(Duration::from_secs(1));
    }
}

fn native_actions() -> HostNativeActions {
    HostNativeActions::default().with_external_https_opener(|url| {
        crate::opener::open(url.as_url().as_str());
        Ok(())
    })
}

impl Drop for WebPreviewHost {
    fn drop(&mut self) {
        self.stop();
    }
}

fn required_server_url(value: Option<String>) -> Result<String> {
    let Some(value) = value else {
        bail!(
            "{SERVER_URL_ENV} is required for desktop web previews; start or reuse a trouve-server and set {SERVER_URL_ENV} to its base URL (preview hosts never open the default database)"
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
        assert!(error.contains("never open the default database"));
    }

    #[test]
    fn preview_rejects_a_blank_server_url() {
        let error = required_server_url(Some("  \n".into()))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "TROUVE_SERVER_URL cannot be empty");
    }

    #[test]
    fn preview_trims_its_explicit_server_url() {
        assert_eq!(
            required_server_url(Some("  http://127.0.0.1:7433  ".into())).unwrap(),
            "http://127.0.0.1:7433"
        );
    }
}
