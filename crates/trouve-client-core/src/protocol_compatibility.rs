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
            "4.0", "5.2", "5.4", "6.1", "7.0", "7.1", "7.2", "7.3", "7.4", "7.5", "7.6", "7.7",
            "7.8", "7.9", "7.10", "7.11", "7.12", "7.13", "8.0", "unknown", "7.14.1",
        ] {
            let error = ensure_compatible_protocol(server, PROTOCOL_VERSION)
                .unwrap_err()
                .to_string();
            assert!(error.contains(server));
            assert!(error.contains(&format!("expected exactly {PROTOCOL_VERSION}")));
        }
    }
}
