"""Regression tests for the release notice source inventory."""

from __future__ import annotations

import importlib.util
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("build-third-party-notices.py")
SPEC = importlib.util.spec_from_file_location("build_third_party_notices", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
notices = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notices)


class RepositoryNoticesTests(unittest.TestCase):
    def test_only_tracked_notice_files_are_listed(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            (root / "assets").mkdir()
            (root / "docs" / "images").mkdir(parents=True)
            (root / "apps" / "codex-plus-manager" / "src" / "assets").mkdir(parents=True)
            (root / "dist" / "windows" / "app").mkdir(parents=True)
            (root / "node_modules" / "example").mkdir(parents=True)
            (root / "LICENSE").write_text("project license", encoding="utf-8")
            (root / "assets" / "NOTICE.txt").write_text("asset notice", encoding="utf-8")
            with zipfile.ZipFile(root / "assets" / "bundle.zip", "w") as bundle:
                bundle.writestr("nested/LICENSE.txt", "embedded notice")
                bundle.writestr("nested/README.md", "other file")
            (root / "docs" / "images" / "embedded.png").write_bytes(b"tracked image")
            (root / "apps" / "codex-plus-manager" / "src" / "assets" / "logo.png").write_bytes(b"tracked logo")
            (root / "assets" / "untracked.png").write_bytes(b"untracked image")
            (root / "README.md").write_text("not a notice", encoding="utf-8")
            (root / "COPYING").write_text("staged after source commit", encoding="utf-8")
            (root / "dist" / "windows" / "app" / "LICENSE").write_text(
                "generated copy", encoding="utf-8"
            )
            (root / "node_modules" / "example" / "LICENSE").write_text(
                "installed dependency", encoding="utf-8"
            )

            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(
                [
                    "git", "add", "--", "LICENSE", "assets/NOTICE.txt", "README.md",
                    "assets/bundle.zip", "docs/images/embedded.png",
                    "apps/codex-plus-manager/src/assets/logo.png",
                ],
                cwd=root,
                check=True,
                capture_output=True,
            )
            subprocess.run(
                [
                    "git", "-c", "user.name=Release Test", "-c", "user.email=release-test@example.invalid",
                    "commit", "-qm", "source snapshot",
                ],
                cwd=root,
                check=True,
                capture_output=True,
            )
            subprocess.run(["git", "add", "--", "COPYING"], cwd=root, check=True, capture_output=True)

            with patch.object(notices, "ROOT", root):
                found = [path.relative_to(root).as_posix() for path in notices.repository_notices()]
                candidates = [
                    path.relative_to(root).as_posix()
                    for path in notices.repository_asset_candidates()
                ]
                embedded_notices = notices.embedded_archive_notices()
                (root / "LICENSE").unlink()
                with self.assertRaisesRegex(FileNotFoundError, "tracked notice missing"):
                    notices.repository_notices()
                (root / "assets" / "bundle.zip").unlink()
                with self.assertRaisesRegex(FileNotFoundError, "tracked asset missing"):
                    notices.repository_asset_candidates()

            self.assertEqual(found, ["LICENSE", "assets/NOTICE.txt"])
            self.assertEqual(
                candidates,
                [
                    "apps/codex-plus-manager/src/assets/logo.png",
                    "assets/bundle.zip",
                    "docs/images/embedded.png",
                ],
            )
            self.assertEqual(embedded_notices, ["assets/bundle.zip!/nested/LICENSE.txt"])


if __name__ == "__main__":
    unittest.main()
