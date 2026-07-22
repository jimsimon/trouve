from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import sync_versions  # noqa: E402


def write(path: Path, contents: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(contents, encoding="utf-8")


def write_json(path: Path, payload: dict) -> None:
    write(path, json.dumps(payload, indent=2) + "\n")


class SyncVersionsTests(unittest.TestCase):
    def test_workspace_version_and_member_inheritance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write(
                root / "Cargo.toml",
                """[workspace]
members = ["crates/*"]

[workspace.package]
version = "3.0.0"
""",
            )
            write(
                root / "crates/trouve-good/Cargo.toml",
                """[package]
name = "trouve-good"
version.workspace = true
""",
            )
            write(
                root / "crates/trouve-stale/Cargo.toml",
                """[package]
name = "trouve-stale"
version = "2.1.0"
""",
            )

            self.assertEqual(sync_versions.workspace_version(root), "3.0.0")
            errors = sync_versions.cargo_inheritance_errors(root, "3.0.0")
            self.assertEqual(len(errors), 1)
            self.assertIn("trouve-stale", errors[0])
            self.assertIn("version.workspace = true", errors[0])

    def test_workspace_dependency_pins_are_checked_and_fixed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "Cargo.toml"
            write(
                manifest,
                """[workspace]
members = ["crates/*"]

[workspace.package]
version = "3.0.0"

[workspace.dependencies]
trouve-child = { path = "crates/trouve-child", version = "2.1.0" }
""",
            )
            write(
                root / "crates/trouve-child/Cargo.toml",
                """[package]
name = "trouve-child"
version.workspace = true
""",
            )

            before = manifest.read_text(encoding="utf-8")
            changes = sync_versions.sync_workspace_dependency_pins(
                root, "3.0.0", check=True
            )
            self.assertEqual(len(changes), 1)
            self.assertEqual(manifest.read_text(encoding="utf-8"), before)

            changes = sync_versions.sync_workspace_dependency_pins(
                root, "3.0.0", check=False
            )
            self.assertEqual(len(changes), 1)
            self.assertIn('version = "3.0.0"', manifest.read_text(encoding="utf-8"))

    def test_member_internal_dependencies_must_inherit_workspace_pins(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write(
                root / "Cargo.toml",
                """[workspace]
members = ["crates/*"]

[workspace.package]
version = "3.0.0"

[workspace.dependencies]
trouve-child = { path = "crates/trouve-child", version = "3.0.0" }
""",
            )
            write(
                root / "crates/trouve-child/Cargo.toml",
                """[package]
name = "trouve-child"
version.workspace = true
""",
            )
            write(
                root / "crates/trouve-good/Cargo.toml",
                """[package]
name = "trouve-good"
version.workspace = true

[dependencies]
trouve-child = { workspace = true, features = ["example"] }
""",
            )
            write(
                root / "crates/trouve-stale/Cargo.toml",
                """[package]
name = "trouve-stale"
version.workspace = true

[dependencies]
trouve-child = { path = "../trouve-child", version = "2.1.0" }
""",
            )

            errors = sync_versions.cargo_dependency_inheritance_errors(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("trouve-stale", errors[0])
            self.assertIn(".workspace = true", errors[0])

    def test_cargo_lock_local_records_are_checked_and_fixed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write(
                root / "Cargo.toml",
                """[workspace]
members = ["crates/*"]

[workspace.package]
version = "3.0.0"
""",
            )
            for name in ("trouve-one", "trouve-two"):
                write(
                    root / f"crates/{name}/Cargo.toml",
                    f"""[package]
name = "{name}"
version.workspace = true
""",
                )
            write(
                root / "Cargo.lock",
                """version = 4

[[package]]
name = "trouve-one"
version = "0.1.0"

[[package]]
name = "trouve-two"
version = "2.1.0"
""",
            )

            before = (root / "Cargo.lock").read_text(encoding="utf-8")
            changes = sync_versions.sync_cargo_lock(root, "3.0.0", check=True)
            self.assertEqual(len(changes), 2)
            self.assertEqual((root / "Cargo.lock").read_text(encoding="utf-8"), before)

            changes = sync_versions.sync_cargo_lock(root, "3.0.0", check=False)
            self.assertEqual(len(changes), 2)
            lock = sync_versions.load_toml(root / "Cargo.lock")
            self.assertEqual(
                {record["version"] for record in lock["package"]}, {"3.0.0"}
            )

    def test_cargo_lock_missing_workspace_record_is_reported_and_refreshed(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write(
                root / "Cargo.toml",
                """[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
version = "3.0.0"
""",
            )
            for name in ("trouve-one", "trouve-two"):
                write(
                    root / f"crates/{name}/Cargo.toml",
                    f"""[package]
name = "{name}"
version.workspace = true
edition = "2021"
""",
                )
                write(root / f"crates/{name}/src/lib.rs", "")
            write(
                root / "Cargo.lock",
                """version = 4

[[package]]
name = "trouve-one"
version = "2.1.0"
""",
            )

            with self.assertRaisesRegex(
                sync_versions.VersionSyncError,
                "Cargo.lock is missing workspace packages: trouve-two",
            ):
                sync_versions.sync_cargo_lock(root, "3.0.0", check=True)

            changes = sync_versions.sync_cargo_lock(root, "3.0.0", check=False)
            self.assertGreaterEqual(len(changes), 1)
            lock = sync_versions.load_toml(root / "Cargo.lock")
            versions = {
                record["name"]: record["version"] for record in lock["package"]
            }
            self.assertEqual(
                versions,
                {"trouve-one": "3.0.0", "trouve-two": "3.0.0"},
            )

    def test_json_sync_is_recursive_complete_and_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package_path = root / "npm/package.json"
            review_path = root / "web/review-ui/package.json"
            lock_path = root / "npm/package-lock.json"
            claude_path = root / "npm/plugin/.claude-plugin/plugin.json"
            codex_path = root / "npm/plugin/.codex-plugin/plugin.json"

            write_json(
                package_path,
                {
                    "name": "@trouve-ai/workspace",
                    "private": True,
                    "dependencies": {"@trouve-ai/runtime": "2.1.0"},
                    "devDependencies": {"@trouve-ai/dev": "2.1.0"},
                    "optionalDependencies": {"@trouve-ai/optional": "2.1.0"},
                    "peerDependencies": {"@trouve-ai/peer": "2.1.0"},
                },
            )
            write_json(
                review_path,
                {"name": "@trouve-ai/review-ui", "version": "0.1.0"},
            )
            write_json(
                lock_path,
                {
                    "name": "@trouve-ai/workspace",
                    "packages": {
                        "": {"name": "@trouve-ai/workspace"},
                        "search-core": {
                            "name": "@trouve-ai/search-core",
                            "version": "2.1.0",
                            "dependencies": {"@trouve-ai/runtime": "2.1.0"},
                        },
                        "node_modules/@trouve-ai/search-core": {
                            "name": "@trouve-ai/search-core",
                            "version": "9.9.9",
                            "resolved": "https://registry.example/search-core.tgz",
                        },
                    },
                },
            )
            write_json(
                claude_path,
                {"name": "trouve-search", "version": "2.1.0"},
            )
            write_json(
                codex_path,
                {"name": "trouve-search", "version": "2.1.0"},
            )

            before = {path: path.read_text(encoding="utf-8") for path in (
                package_path,
                review_path,
                lock_path,
                claude_path,
                codex_path,
            )}
            changes = sync_versions.sync_json_artifacts(root, "3.0.0", check=True)
            self.assertGreaterEqual(len(changes), 10)
            for path, contents in before.items():
                self.assertEqual(path.read_text(encoding="utf-8"), contents)

            sync_versions.sync_json_artifacts(root, "3.0.0", check=False)
            package = json.loads(package_path.read_text(encoding="utf-8"))
            self.assertEqual(package["version"], "3.0.0")
            for field in sync_versions.DEPENDENCY_FIELDS:
                self.assertEqual(next(iter(package[field].values())), "3.0.0")

            review = json.loads(review_path.read_text(encoding="utf-8"))
            self.assertEqual(review["version"], "3.0.0")
            lock = json.loads(lock_path.read_text(encoding="utf-8"))
            self.assertEqual(lock["version"], "3.0.0")
            self.assertEqual(lock["packages"][""]["version"], "3.0.0")
            self.assertEqual(lock["packages"]["search-core"]["version"], "3.0.0")
            self.assertEqual(
                lock["packages"]["search-core"]["dependencies"][
                    "@trouve-ai/runtime"
                ],
                "3.0.0",
            )
            self.assertEqual(
                lock["packages"]["node_modules/@trouve-ai/search-core"]["version"],
                "9.9.9",
            )
            self.assertEqual(
                json.loads(claude_path.read_text(encoding="utf-8"))["version"],
                "3.0.0",
            )
            self.assertEqual(
                json.loads(codex_path.read_text(encoding="utf-8"))["version"],
                "3.0.0",
            )
            self.assertEqual(
                sync_versions.sync_json_artifacts(root, "3.0.0", check=False), []
            )


if __name__ == "__main__":
    unittest.main()
