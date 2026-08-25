import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
HELPER = REPO_ROOT / "packaging" / "safe_cleanup.py"


class SafeCleanupTests(unittest.TestCase):
    def invoke(self, repo: Path, target: Path, expected: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [
                sys.executable,
                str(HELPER),
                "--repo",
                str(repo),
                "--target",
                str(target),
                "--expected",
                expected,
            ],
            check=False,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )

    def test_accepts_exact_repository_owned_package_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary) / "repo"
            target = repo / "dist" / "linux"
            target.mkdir(parents=True)
            result = self.invoke(repo, target, "dist/linux")
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_symlink_ancestor_without_touching_external_data(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = root / "repo"
            outside = root / "outside"
            repo.mkdir()
            outside.mkdir()
            sentinel = outside / "sentinel"
            sentinel.write_text("keep", encoding="utf-8")
            try:
                os.symlink(outside, repo / "dist", target_is_directory=True)
            except OSError as error:
                self.skipTest(f"directory symlinks unavailable: {error}")

            result = self.invoke(repo, repo / "dist" / "linux", "dist/linux")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("package_cleanup_symlink_rejected", result.stderr)
            self.assertEqual(sentinel.read_text(encoding="utf-8"), "keep")


if __name__ == "__main__":
    unittest.main()
