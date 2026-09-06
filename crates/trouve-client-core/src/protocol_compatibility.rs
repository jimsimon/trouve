//! Shared client-side protocol-version compatibility checks.

use anyhow::{Result, bail};

/// Require the server to use the client's exact protocol version.
///
/// Generated clients contain closed enums and discriminated unions, so an
/// otherwise additive schema revision can still add a value they cannot
/// deserialize. Compatibility must therefore be declared by shipping a client
/// for that exact schema, not inferred from a shared major version.
pub fn ensure_compatible_protocol(server: &str, required: &str) -> Result<()> {
    if server != required {
        bail!("server protocol {server} is incompatible; expected exactly {required}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use trouve_protocol::PROTOCOL_VERSION;

    #[test]
    fn accepts_the_exact_required_protocol() {
        ensure_compatible_protocol(PROTOCOL_VERSION, PROTOCOL_VERSION).unwrap();
    }

    #[test]
    fn rejects_older_newer_other_major_and_malformed_protocols() {
        for server in [
            "4.0", "5.2", "6.1", "7.0", "7.1", "7.2", "7.3", "7.4", "7.5", "7.6", "7.7", "7.8",
            "7.9", "7.10", "7.11", "7.12", "7.13", "7.14", "7.15", "7.16", "7.17", "7.18", "7.19",
            "7.20", "7.21", "7.22", "7.23", "7.24", "7.25", "7.26", "7.27", "7.28", "7.28.1",
            "7.29", "7.29.1", "7.30", "7.30.1", "7.31", "7.31.1", "7.32", "8.0", "8.1", "8.2",
            "8.4", "9.0", "unknown",
        ] {
            let error = ensure_compatible_protocol(server, PROTOCOL_VERSION)
                .unwrap_err()
                .to_string();
            assert!(error.contains(server));
            assert!(error.contains(&format!("expected exactly {PROTOCOL_VERSION}")));
        }
    }
}
