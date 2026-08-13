//! Pins the native-host bridge independently from the public agent protocol.
//! Update with:
//! TROUVE_UPDATE_HOST_OPENAPI=1 cargo test -p trouve-desktop-host openapi

use std::path::PathBuf;

#[test]
fn host_openapi_schema_matches_snapshot() {
    let current =
        serde_json::to_string_pretty(&trouve_desktop_host::host_openapi_json()).unwrap() + "\n";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/openapi.json");
    if std::env::var("TROUVE_UPDATE_HOST_OPENAPI").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, current).unwrap();
        return;
    }
    let snapshot = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing snapshot {}; run TROUVE_UPDATE_HOST_OPENAPI=1 cargo test -p trouve-desktop-host openapi",
            path.display()
        )
    });
    assert_eq!(snapshot, current, "native-host OpenAPI snapshot drifted");
}
