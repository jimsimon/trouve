import importlib.util
import json
import pathlib
import subprocess
import unittest
from unittest import mock


MODULE_PATH = pathlib.Path(__file__).with_name("generate_rust_third_party_notices.py")
SPEC = importlib.util.spec_from_file_location("rust_notices", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
rust_notices = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(rust_notices)


def metadata(license_name: str = "Apache-2.0 / MIT") -> dict[str, object]:
    return {
        "workspace_members": ["path+file:///repo/app#trouve-app@3.7.0"],
        "packages": [
            {
                "id": "path+file:///repo/app#trouve-app@3.7.0",
                "name": "trouve-app",
                "version": "3.7.0",
                "source": None,
                "manifest_path": "/repo/app/Cargo.toml",
            },
            {
                "id": "registry+example#dependency@1.0.0",
                "name": "dependency",
                "version": "1.0.0",
                "source": "registry+https://example.invalid/index",
                "license": license_name,
            },
        ],
    }


class RustSbomTests(unittest.TestCase):
    @mock.patch.object(rust_notices.subprocess, "run")
    def test_cargo_metadata_surfaces_cargo_stderr(self, run):
        run.return_value = subprocess.CompletedProcess(
            args=["cargo", "metadata"],
            returncode=101,
            stdout="",
            stderr="error: the lock file needs to be updated\n",
        )

        with self.assertRaisesRegex(
            SystemExit,
            "(?s)cargo metadata .* failed:.*lock file needs to be updated",
        ):
            rust_notices.cargo_metadata(pathlib.Path("nested/Cargo.toml"))

    def test_nested_notice_commands_and_frontend_link_target_the_nested_graph(self):
        manifest = pathlib.Path("crates/trouve-isolated-preview/Cargo.toml")
        notice = pathlib.Path("crates/trouve-isolated-preview/THIRD_PARTY_NOTICES.md")
        generated = rust_notices.generate(
            metadata(),
            str(manifest),
            manifest,
            notice,
        )

        command = (
            "python3 scripts/generate_rust_third_party_notices.py "
            "--manifest-path crates/trouve-isolated-preview/Cargo.toml "
            "--notice crates/trouve-isolated-preview/THIRD_PARTY_NOTICES.md"
        )
        self.assertIn(f"`{command}`", generated)
        self.assertIn(f"`{command} --check`", generated)
        self.assertIn(
            "(../../web/app-ui/THIRD_PARTY_NOTICES.md)",
            generated,
        )

    def test_sbom_has_product_version_and_normalized_spdx(self):
        document = json.loads(rust_notices.generate_sbom(metadata()))
        product = document["metadata"]["component"]
        self.assertEqual(product["version"], "3.7.0")
        self.assertEqual(product["purl"], "pkg:cargo/trouve@3.7.0")
        self.assertEqual(
            document["components"][0]["licenses"],
            [{"expression": "Apache-2.0 OR MIT"}],
        )

    def test_nested_sbom_uses_the_selected_workspace_package_identity(self):
        nested = metadata()
        manifest = pathlib.Path("/repo/app/Cargo.toml")
        product_name = rust_notices.sbom_product_name(nested, manifest)
        document = json.loads(rust_notices.generate_sbom(nested, product_name))

        product = document["metadata"]["component"]
        self.assertEqual(product["name"], "trouve-app")
        self.assertEqual(product["purl"], "pkg:cargo/trouve-app@3.7.0")

    def test_workspace_version_must_be_unambiguous(self):
        broken = metadata()
        broken["workspace_members"].append("path+file:///repo/other#other@4.0.0")
        broken["packages"].append(
            {
                "id": "path+file:///repo/other#other@4.0.0",
                "name": "other",
                "version": "4.0.0",
                "source": None,
            }
        )
        with self.assertRaisesRegex(SystemExit, "one product version"):
            rust_notices.generate_sbom(broken)


if __name__ == "__main__":
    unittest.main()
