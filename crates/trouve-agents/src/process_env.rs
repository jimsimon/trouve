//! Child-process environment normalization for desktop-launched trouve.
//!
//! Graphical launchers commonly start the desktop app with a minimal `PATH`,
//! while a user's interactive/login shell adds language managers such as NVM.
//! Capture that shell's `PATH` once, merge it with the inherited value, and
//! use the cached result for direct child execution. The child itself is still
//! spawned without a shell, which is required for stdio protocols such as MCP.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const PATH_MARKER: &str = "__TROUVE_LOGIN_SHELL_PATH__";
const PATH_CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);
const PATH_CAPTURE_SCRIPT: &str =
    "exec /bin/sh -c 'printf \"__TROUVE_LOGIN_SHELL_PATH__%s\\n\" \"$PATH\"'";

static EFFECTIVE_PATH: OnceLock<Option<OsString>> = OnceLock::new();

/// The merged login-shell and inherited process path, captured at most once.
pub fn effective_path() -> Option<&'static OsStr> {
    EFFECTIVE_PATH
        .get_or_init(|| {
            let inherited = std::env::var_os("PATH");
            let shell = login_shell_path().and_then(|shell| {
                let captured = capture_shell_path(&shell);
                if captured.is_some() {
                    tracing::debug!(shell = %shell.display(), "captured login-shell PATH for child processes");
                } else {
                    tracing::debug!(shell = %shell.display(), "login-shell PATH capture failed; using inherited PATH");
                }
                captured
            });
            merge_paths(shell.as_deref(), inherited.as_deref())
        })
        .as_deref()
}

/// Resolve a command through the normalized child path.
pub fn find_executable(command: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(command);
    if direct.components().count() > 1 {
        return executable_file(&direct).then_some(direct);
    }
    find_executable_in_path(command, effective_path()?)
}

/// Build a Tokio command with normalized executable lookup and child `PATH`.
pub fn tokio_command(command: &str) -> tokio::process::Command {
    let mut child = tokio::process::Command::new(
        find_executable(command).unwrap_or_else(|| PathBuf::from(command)),
    );
    apply_path_to_tokio(&mut child);
    child
}

/// Build a standard-library command with normalized lookup and child `PATH`.
pub fn std_command(command: &str) -> std::process::Command {
    let mut child = std::process::Command::new(
        find_executable(command).unwrap_or_else(|| PathBuf::from(command)),
    );
    if let Some(path) = effective_path() {
        child.env("PATH", path);
    }
    child
}

/// Apply the normalized `PATH` to an existing Tokio command.
pub fn apply_path_to_tokio(command: &mut tokio::process::Command) {
    if let Some(path) = effective_path() {
        command.env("PATH", path);
    }
}

fn login_shell_path() -> Option<PathBuf> {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|shell| shell.is_file())
        .or_else(login_shell_from_passwd)
}

#[cfg(unix)]
fn login_shell_from_passwd() -> Option<PathBuf> {
    let user = std::env::var("USER").ok()?;
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let fields = line.split(':').collect::<Vec<_>>();
        (fields.len() >= 7 && fields[0] == user)
            .then(|| PathBuf::from(fields[6]))
            .filter(|shell| shell.is_file())
    })
}

#[cfg(not(unix))]
fn login_shell_from_passwd() -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn capture_shell_path(shell: &Path) -> Option<OsString> {
    let shell_name = shell.file_name()?.to_str()?;
    let mut command = std::process::Command::new(shell);
    if matches!(shell_name, "bash" | "fish" | "ksh" | "zsh") {
        command.args(["-l", "-i", "-c", PATH_CAPTURE_SCRIPT]);
    } else {
        command.args(["-l", "-c", PATH_CAPTURE_SCRIPT]);
    }
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("TERM", "dumb");
    let mut output = tempfile::tempfile().ok()?;
    command.stdout(Stdio::from(output.try_clone().ok()?));
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + PATH_CAPTURE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    status?.success().then_some(())?;
    output.rewind().ok()?;
    let mut bytes = Vec::new();
    output.take(1024 * 1024).read_to_end(&mut bytes).ok()?;
    extract_marked_path(&bytes)
}

#[cfg(not(unix))]
fn capture_shell_path(_shell: &Path) -> Option<OsString> {
    None
}

fn extract_marked_path(output: &[u8]) -> Option<OsString> {
    let text = String::from_utf8_lossy(output);
    let marked = text.rsplit_once(PATH_MARKER)?.1;
    let path = marked.lines().next()?.trim();
    (!path.is_empty()).then(|| OsString::from(path))
}

fn merge_paths(shell: Option<&OsStr>, inherited: Option<&OsStr>) -> Option<OsString> {
    let mut seen = HashSet::<OsString>::new();
    let mut paths = Vec::<PathBuf>::new();
    for value in [shell, inherited].into_iter().flatten() {
        for path in std::env::split_paths(value) {
            if seen.insert(path.as_os_str().to_os_string()) {
                paths.push(path);
            }
        }
    }
    (!paths.is_empty())
        .then(|| std::env::join_paths(paths).ok())
        .flatten()
}

fn find_executable_in_path(command: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|directory| directory.join(command))
        .find(|candidate| executable_file(candidate))
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_the_last_marked_path() {
        let output = b"shell startup noise\n__TROUVE_LOGIN_SHELL_PATH__/old\n\
                       __TROUVE_LOGIN_SHELL_PATH__/nvm/bin:/usr/bin\n";
        assert_eq!(
            extract_marked_path(output),
            Some(OsString::from("/nvm/bin:/usr/bin"))
        );
    }

    #[test]
    fn shell_paths_precede_inherited_paths_without_duplicates() {
        let shell = std::env::join_paths(["/nvm/bin", "/usr/bin"]).unwrap();
        let inherited = std::env::join_paths(["/usr/bin", "/opt/trouve/bin"]).unwrap();
        let merged = merge_paths(Some(&shell), Some(&inherited)).unwrap();
        assert_eq!(
            std::env::split_paths(&merged).collect::<Vec<_>>(),
            ["/nvm/bin", "/usr/bin", "/opt/trouve/bin"]
                .into_iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn executable_lookup_uses_the_supplied_path() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("npx");
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([directory.path()]).unwrap();
        assert_eq!(find_executable_in_path("npx", &path), Some(executable));
    }
}
