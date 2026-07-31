from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import verify_release_assets as assets  # noqa: E402


TARGETS = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]


def checksums(names: set[str]) -> str:
    return "".join(f"{'ab' * 32}  {name}\n" for name in sorted(names))


class VerifyReleaseAssetsTests(unittest.TestCase):
    def test_expected_assets_cover_each_component_and_archive_format(self) -> None:
        expected = assets.expected_assets("v3.7.0", targets=TARGETS)
        self.assertIn(
            "trouve-v3.7.0-x86_64-unknown-linux-gnu.tar.gz", expected
        )
        self.assertIn(
            "trouve-server-v3.7.0-aarch64-unknown-linux-musl.tar.gz", expected
        )
        self.assertNotIn(
            "trouve-v3.7.0-aarch64-unknown-linux-musl.tar.gz", expected
        )
        self.assertIn(
            "trouve-search-v3.7.0-x86_64-pc-windows-msvc.zip", expected
        )

    def test_complete_checksum_set_passes(self) -> None:
        expected = assets.expected_assets("v3.7.0", targets=TARGETS)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "SHA256SUMS"
            path.write_text(checksums(expected), encoding="utf-8")
            assets.verify(path, "v3.7.0", targets=TARGETS)

    def test_missing_asset_fails_with_its_exact_name(self) -> None:
        expected = assets.expected_assets("v3.7.0", targets=TARGETS)
        missing = "trouve-server-v3.7.0-aarch64-unknown-linux-musl.tar.gz"
        expected.remove(missing)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "SHA256SUMS"
            path.write_text(checksums(expected), encoding="utf-8")
            with self.assertRaisesRegex(assets.AssetError, missing):
                assets.verify(path, "v3.7.0", targets=TARGETS)

    def test_backfill_may_omit_search_but_not_app_or_server(self) -> None:
        expected = assets.expected_assets(
            "v3.7.0", allow_missing_search=True, targets=TARGETS
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "SHA256SUMS"
            path.write_text(checksums(expected), encoding="utf-8")
            assets.verify(
                path,
                "v3.7.0",
                allow_missing_search=True,
                targets=TARGETS,
            )

    def test_rejects_noncanonical_tag_and_duplicate_checksum(self) -> None:
        with self.assertRaisesRegex(assets.AssetError, "canonical"):
            assets.expected_assets("release-3.7.0", targets=TARGETS)
        duplicate = f"{'ab' * 32}  asset\n{'cd' * 32}  asset\n"
        with self.assertRaisesRegex(assets.AssetError, "duplicate"):
            assets.checksum_assets(duplicate)


if __name__ == "__main__":
    unittest.main()
