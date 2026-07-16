//! Pins the OpenAPI schema (invariant 5): any protocol change must land
//! together with a deliberate snapshot update (and a protocol version bump
//! for breaking changes).
//!
//! Update with: TROUVE_UPDATE_OPENAPI=1 cargo test -p trouve-server openapi

use std::path::PathBuf;

#[test]
fn openapi_schema_matches_snapshot() {
    let doc = trouve_server::openapi_json();
    let current = serde_json::to_string_pretty(&doc).unwrap() + "\n";

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/openapi.json");
    if std::env::var("TROUVE_UPDATE_OPENAPI").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &current).unwrap();
        return;
    }
    let snapshot = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing snapshot {}; run TROUVE_UPDATE_OPENAPI=1 cargo test -p trouve-server openapi",
            path.display()
        )
    });
    assert_eq!(
        snapshot, current,
        "OpenAPI schema changed. If intentional, bump PROTOCOL_VERSION as needed and re-run \
         with TROUVE_UPDATE_OPENAPI=1."
    );
}
