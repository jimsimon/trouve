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

    #[test]
    fn accepts_the_exact_required_protocol() {
        ensure_compatible_protocol("5.0", "5.0").unwrap();
    }

    #[test]
    fn rejects_older_newer_other_major_and_malformed_protocols() {
        for server in ["3.36", "4.0", "4.2", "5.1", "unknown", "5.0.1"] {
            let error = ensure_compatible_protocol(server, "5.0")
                .unwrap_err()
                .to_string();
            assert!(error.contains(server));
            assert!(error.contains("expected exactly 5.0"));
        }
    }
}
