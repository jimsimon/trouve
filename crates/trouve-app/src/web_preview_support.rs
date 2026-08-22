//! Shared bootstrap and lifecycle for desktop Wry hosts.
//!
//! The first product host owns one embedded `trouve-server`; later product
//! windows attach to that elected owner unless `TROUVE_SERVER_URL` explicitly
//! selects another process. Comparison hosts always require that explicit
//! URL so they cannot silently become an owner of the default database.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use trouve_client_core::{
    client::ProtocolClient, protocol_compatibility::ensure_compatible_protocol,
};
use trouve_desktop_host::{
    FrontendSource, HostCapabilities, HostGateway, HostNativeActions, HostPreferences,
    HostPreferencesHandle, ProtocolUpstreamOwnership, VerifiedSessionFile,
};
use trouve_protocol::PROTOCOL_VERSION;

const SERVER_URL_ENV: &str = "TROUVE_SERVER_URL";

#[cfg(any(target_os = "linux", test))]
const BACKGROUND_NICE_INCREMENT: i32 = 5;

#[cfg(any(target_os = "linux", test))]
fn background_nice_value(current: i32) -> i32 {
    current.saturating_add(BACKGROUND_NICE_INCREMENT).min(19)
}

#[cfg(any(target_os = "linux", test))]
fn priority_result(value: i32, errno: i32) -> std::io::Result<i32> {
    if value == -1 && errno != 0 {
        Err(std::io::Error::from_raw_os_error(errno))
    } else {
        Ok(value)
    }
}

#[cfg(target_os = "linux")]
fn current_nice_value() -> std::io::Result<i32> {
    // Linux exposes errno through thread-local storage. Clear it immediately
    // before getpriority because -1 is both a valid niceness and its error
    // sentinel, then snapshot it before another call can overwrite it.
    // SAFETY: __errno_location returns the calling thread's errno slot.
    let errno = unsafe { libc::__errno_location() };
    unsafe {
        *errno = 0;
    }
    // SAFETY: getpriority reads scheduler state for the calling Linux thread.
    let current = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    // SAFETY: `errno` still points to this thread's valid errno slot.
    let error_code = unsafe { *errno };
    priority_result(current, error_code)
}

/// Lower only the runtime thread that invokes this hook. Linux inherits a
/// thread's niceness when it spawns child processes, so agent subprocesses
/// yield to the unchanged Tao/Wry event-loop thread under CPU contention.
#[cfg(target_os = "linux")]
fn deprioritize_background_thread() {
    let current = match current_nice_value() {
        Ok(current) => current,
        Err(error) => {
            tracing::warn!(%error, "could not read desktop runtime CPU priority");
            return;
        }
    };
    let target = background_nice_value(current);
    if target == current {
        return;
    }
    // SAFETY: setpriority updates the calling Linux thread when `who` is
    // zero and retains no Rust pointers.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, target) } != 0 {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "could not lower desktop runtime CPU priority"
        );
    }
}

/// Hardened loopback gateway and the runtime that owns it.
///
/// Keep this value alive for the webview's full lifetime. Calling
/// [`Self::shutdown`] is preferred; `Drop` provides the same cancellation as
/// a fallback for host-startup errors after the gateway has been bound.
pub struct WebPreviewHost {
    gateway_origin: String,
    gateway_task: Option<JoinHandle<()>>,
    embedded_server_task: Option<JoinHandle<()>>,
    runtime: Option<Runtime>,
    #[allow(dead_code)]
    initial_preferences: HostPreferences,
    #[allow(dead_code)]
    preferences: HostPreferencesHandle,
}

impl WebPreviewHost {
    /// Start with the complete, explicitly app-owned native action set.
    #[allow(dead_code)]
    pub fn start_with_native_actions(
        frontend: FrontendSource,
        native_actions: HostNativeActions,
    ) -> Result<Self> {
        Self::start_with_actions(frontend, native_actions, false)
    }

    /// Start the shipping desktop host. With no configured upstream, this
    /// process claims the local server/database or attaches to their owner.
    // The shared support module is also compiled into the comparison binary.
    #[allow(dead_code)]
    pub fn start_product_with_native_actions(
        frontend: FrontendSource,
        native_actions: HostNativeActions,
    ) -> Result<Self> {
        Self::start_with_actions(frontend, native_actions, true)
    }

    fn start_with_actions(
        frontend: FrontendSource,
        native_actions: HostNativeActions,
        allow_embedded_server: bool,
    ) -> Result<Self> {
        trouve_server::install_crypto_provider();
        let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
        // The gateway, embedded server, and notification dispatcher are
        // I/O-bound; four workers avoid per-core allocator arena growth.
        runtime_builder.worker_threads(4);
        #[cfg(target_os = "linux")]
        runtime_builder.on_thread_start(deprioritize_background_thread);
        let runtime = runtime_builder
            .enable_all()
            .build()
            .context("creating the desktop host runtime")?;

        let configured_upstream =
            configured_server_url(std::env::var(SERVER_URL_ENV).ok(), allow_embedded_server)?;
        let upstream_ownership = configured_upstream_ownership(configured_upstream.as_deref());
        let (upstream, embedded_server_task) = match configured_upstream {
            Some(upstream) => (upstream, None),
            None => {
                let binding = runtime
                    .block_on(trouve_server::bind_local(
                        "127.0.0.1:0"
                            .parse()
                            .expect("static loopback address parses"),
                        trouve_server::ServerSecurity::loopback(),
                    ))
                    .context("binding the embedded trouve server")?;
                let address = binding.address();
                let server_task = binding.into_server().map(|server| {
                    runtime.spawn(async move {
                        if let Err(error) = server.await {
                            tracing::error!(%error, "embedded trouve server stopped");
                        }
                    })
                });
                (format!("http://{address}"), server_task)
            }
        };

        let protocol = ProtocolClient::new(&upstream);
        let server_info = runtime
            .block_on(wait_for_server_info(&protocol))
            .with_context(|| format!("connecting desktop host to {upstream}"))?;
        ensure_compatible_protocol(&server_info.protocol_version, PROTOCOL_VERSION)
            .with_context(|| format!("connecting desktop host to {upstream}"))?;
        // Resolve local paths only for the app-owned embedded/elected server,
        // whose session worktrees are known to share this filesystem. An
        // explicit URL can be a loopback tunnel or container port-forward.
        let native_actions = if upstream_ownership == ProtocolUpstreamOwnership::AppOwned {
            let file_protocol = protocol.clone();
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
            })
        } else {
            native_actions
        };

        let preference_path = dirs::config_dir()
            .map(|directory| directory.join("trouve").join("web-preferences.json"));
        let (gateway_address, gateway, preferences) = runtime.block_on(
            HostGateway::bind_loopback_with_protocol_ownership_and_preferences(
                "127.0.0.1:0"
                    .parse()
                    .expect("static loopback address parses"),
                frontend,
                HostCapabilities::desktop(),
                HostPreferences::default(),
                Some(&upstream),
                upstream_ownership,
                preference_path,
                native_actions,
            ),
        )?;
        let initial_preferences = runtime.block_on(preferences.snapshot());
        let gateway_task = runtime.spawn(async move {
            if let Err(error) = gateway.await {
                tracing::error!(%error, "desktop frontend gateway stopped");
            }
        });

        Ok(Self {
            gateway_origin: format!("http://{gateway_address}"),
            gateway_task: Some(gateway_task),
            embedded_server_task,
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
        if let Some(server_task) = self.embedded_server_task.take() {
            server_task.abort();
            let result = runtime.block_on(server_task);
            if let Err(error) = result
                && !error.is_cancelled()
            {
                tracing::warn!(%error, "joining embedded trouve server task failed");
            }
        }
        runtime.shutdown_timeout(Duration::from_secs(1));
    }
}

async fn wait_for_server_info(protocol: &ProtocolClient) -> Result<trouve_protocol::ServerInfo> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match protocol.info().await {
                Ok(info) => return Ok(info),
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .context("timed out after 5 seconds")?
}

impl Drop for WebPreviewHost {
    fn drop(&mut self) {
        self.stop();
    }
}

fn configured_server_url(value: Option<String>, allow_embedded: bool) -> Result<Option<String>> {
    let Some(value) = value else {
        if allow_embedded {
            return Ok(None);
        }
        bail!(
            "{SERVER_URL_ENV} is required for desktop web previews; start or reuse a trouve-server and set {SERVER_URL_ENV} to its base URL (preview hosts never open the default database)"
        );
    };
    let value = value.trim();
    if value.is_empty() {
        bail!("{SERVER_URL_ENV} cannot be empty");
    }
    Ok(Some(value.to_owned()))
}

fn configured_upstream_ownership(value: Option<&str>) -> ProtocolUpstreamOwnership {
    match value {
        Some(_) => ProtocolUpstreamOwnership::Explicit,
        None => ProtocolUpstreamOwnership::AppOwned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_requires_an_explicit_server_url() {
        let error = configured_server_url(None, false).unwrap_err().to_string();
        assert!(error.contains(SERVER_URL_ENV));
        assert!(error.contains("never open the default database"));
    }

    #[test]
    fn background_niceness_yields_without_exceeding_linux_limit() {
        assert_eq!(background_nice_value(0), 5);
        assert_eq!(background_nice_value(10), 15);
        assert_eq!(background_nice_value(18), 19);
        assert_eq!(background_nice_value(19), 19);
    }

    #[test]
    fn failed_priority_reads_are_not_treated_as_valid_negative_niceness() {
        const TEST_ERRNO: i32 = 5;
        assert_eq!(priority_result(-1, 0).unwrap(), -1);
        let error = priority_result(-1, TEST_ERRNO).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(TEST_ERRNO));
    }

    #[test]
    fn product_uses_an_embedded_server_by_default() {
        assert_eq!(configured_server_url(None, true).unwrap(), None);
        assert_eq!(
            configured_upstream_ownership(None),
            ProtocolUpstreamOwnership::AppOwned
        );
    }

    #[test]
    fn preview_rejects_a_blank_server_url() {
        let error = configured_server_url(Some("  \n".into()), false)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "TROUVE_SERVER_URL cannot be empty");
    }

    #[test]
    fn preview_trims_its_explicit_server_url() {
        assert_eq!(
            configured_server_url(Some("  http://127.0.0.1:7433  ".into()), true).unwrap(),
            Some("http://127.0.0.1:7433".into())
        );
        assert_eq!(
            configured_upstream_ownership(Some("http://127.0.0.1:7433")),
            ProtocolUpstreamOwnership::Explicit
        );
    }
}
