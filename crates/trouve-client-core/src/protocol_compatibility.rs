//! Shared client-side protocol-version compatibility checks.

use anyhow::{Result, bail};

/// Require the server to use the same major protocol version and at least the
/// client's required additive minor version.
pub fn ensure_compatible_protocol(server: &str, required: &str) -> Result<()> {
    fn parse(version: &str) -> Option<(u64, u64)> {
        let (major, minor) = version.split_once('.')?;
        if minor.contains('.') {
            return None;
        }
        Some((major.parse().ok()?, minor.parse().ok()?))
    }

    let compatible = match (parse(server), parse(required)) {
        (Some((server_major, server_minor)), Some((required_major, required_minor))) => {
            server_major == required_major && server_minor >= required_minor
        }
        _ => false,
    };
    if !compatible {
        bail!(
            "server protocol {server} is incompatible; expected {required} or newer {required_major}.x",
            required_major =
                parse(required).map_or("unknown".to_owned(), |(major, _)| major.to_string())
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_current_and_newer_compatible_protocols() {
        ensure_compatible_protocol("3.14", "3.14").unwrap();
        ensure_compatible_protocol("3.99", "3.14").unwrap();
    }

    #[test]
    fn rejects_older_other_major_and_malformed_protocols() {
        for server in ["3.13", "2.99", "4.0", "unknown", "3.14.1"] {
            let error = ensure_compatible_protocol(server, "3.14")
                .unwrap_err()
                .to_string();
            assert!(error.contains(server));
            assert!(error.contains("3.14 or newer 3.x"));
        }
    }
}
