import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPO_ROOT / "packaging" / "package_manifest.py"
SPEC = importlib.util.spec_from_file_location("frd_package_manifest", MODULE_PATH)
package_manifest = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(package_manifest)


class PackageManifestTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.identity = package_manifest.resolve_locked_identity(REPO_ROOT)

    def test_locked_identity_comes_from_cargo_metadata_and_lock(self):
        cargo_toml = (REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8")
        self.assertIn(f'version = "{self.identity.version}"', cargo_toml)
        self.assertEqual(self.identity.fdk_name, "fdk-aac-sys")
        self.assertRegex(self.identity.fdk_version, r"^\d+\.\d+\.\d+$")
        self.assertTrue(self.identity.fdk_source.startswith("registry+"))
        self.assertRegex(self.identity.fdk_checksum, r"^[0-9a-f]{64}$")

    def test_prepare_fdk_bundle_verifies_and_copies_exact_registry_archive(self):
        with tempfile.TemporaryDirectory() as temporary:
            support = Path(temporary) / "THIRD_PARTY"
            copied = package_manifest.prepare_fdk_bundle(
                REPO_ROOT, support, self.identity
            )

            self.assertEqual(
                package_manifest.sha256_file(copied.source_archive),
                self.identity.fdk_checksum,
            )
            notice = copied.notice.read_text(encoding="utf-8")
            self.assertIn("Software License for The Fraunhofer FDK AAC Codec", notice)
            self.assertIn("NO EXPRESS OR IMPLIED LICENSES TO ANY PATENT CLAIMS", notice)
            self.assertGreater(copied.source_archive.stat().st_size, 1_000_000)

    def test_manifest_requires_versioned_windows_artifacts_and_all_sidecars(self):
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            prefix = f"FreeRemoteAccess-{self.identity.version}-windows-x64"
            artifacts = []
            for kind, suffix in (
                ("gui-exe", ".exe"),
                ("portable-zip", "-portable.zip"),
                ("msi", ".msi"),
            ):
                path = dist / f"{prefix}{suffix}"
                path.write_bytes(kind.encode("ascii"))
                artifacts.append((kind, path))
            support = package_manifest.prepare_fdk_bundle(
                REPO_ROOT, dist / "THIRD_PARTY", self.identity
            )

            manifest_path = package_manifest.write_manifest(
                REPO_ROOT,
                dist,
                "windows",
                "x64",
                artifacts,
                support,
            )
            package_manifest.verify_manifest(REPO_ROOT, manifest_path)

            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            self.assertEqual(manifest["schema"], "FRDPKG01")
            self.assertEqual(manifest["release_status"], [
                "UNSIGNED",
                "NOT FOR PUBLIC DISTRIBUTION",
            ])
            self.assertEqual(
                {entry["kind"] for entry in manifest["artifacts"]},
                {"gui-exe", "portable-zip", "msi"},
            )
            for entry in manifest["files"]:
                self.assertTrue((dist / f'{entry["path"]}.sha256').is_file())
            self.assertTrue(Path(f"{manifest_path}.sha256").is_file())

    def test_manifest_verification_fails_closed_on_missing_or_tampered_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            prefix = f"FreeRemoteAccess-{self.identity.version}-linux-x86_64"
            artifacts = []
            for kind, suffix in (
                ("appdir-archive", "-AppDir.tar.zst"),
                ("deb", ".deb"),
                ("appimage", ".AppImage"),
            ):
                path = dist / f"{prefix}{suffix}"
                path.write_bytes(kind.encode("ascii"))
                artifacts.append((kind, path))
            support = package_manifest.prepare_fdk_bundle(
                REPO_ROOT, dist / "THIRD_PARTY", self.identity
            )
            manifest_path = package_manifest.write_manifest(
                REPO_ROOT, dist, "linux", "x86_64", artifacts, support
            )

            original_size = artifacts[0][1].stat().st_size
            artifacts[0][1].write_bytes(b"x" * original_size)
            with self.assertRaisesRegex(ValueError, "artifact_hash_mismatch"):
                package_manifest.verify_manifest(REPO_ROOT, manifest_path)

            artifacts[0][1].unlink()
            with self.assertRaisesRegex(ValueError, "artifact_missing"):
                package_manifest.verify_manifest(REPO_ROOT, manifest_path)

    def test_manifest_rejects_unlisted_distributable_and_missing_sidecar(self):
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            prefix = f"FreeRemoteAccess-{self.identity.version}-macos-universal"
            artifacts = []
            for kind, suffix in (("app-archive", "-app.zip"), ("dmg", ".dmg")):
                path = dist / f"{prefix}{suffix}"
                path.write_bytes(kind.encode("ascii"))
                artifacts.append((kind, path))
            support = package_manifest.prepare_fdk_bundle(
                REPO_ROOT, dist / "THIRD_PARTY", self.identity
            )
            manifest_path = package_manifest.write_manifest(
                REPO_ROOT, dist, "macos", "universal", artifacts, support
            )

            Path(f"{artifacts[0][1]}.sha256").unlink()
            with self.assertRaisesRegex(ValueError, "artifact_sidecar_missing"):
                package_manifest.verify_manifest(REPO_ROOT, manifest_path)
            package_manifest.write_sha256_sidecar(artifacts[0][1])

            (dist / "FreeRemoteAccess-9.9.9-macos-universal.dmg").write_bytes(b"stale")
            with self.assertRaisesRegex(ValueError, "artifact_unlisted_file"):
                package_manifest.verify_manifest(REPO_ROOT, manifest_path)

    def test_manifest_rejects_unknown_schema_fields_duplicate_paths_and_escape(self):
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            prefix = f"FreeRemoteAccess-{self.identity.version}-windows-x64"
            artifacts = []
            for kind, suffix in (
                ("gui-exe", ".exe"),
                ("portable-zip", "-portable.zip"),
                ("msi", ".msi"),
            ):
                path = dist / f"{prefix}{suffix}"
                path.write_bytes(kind.encode("ascii"))
                artifacts.append((kind, path))
            support = package_manifest.prepare_fdk_bundle(
                REPO_ROOT, dist / "THIRD_PARTY", self.identity
            )
            manifest_path = package_manifest.write_manifest(
                REPO_ROOT, dist, "windows", "x64", artifacts, support
            )
            original = json.loads(manifest_path.read_text(encoding="utf-8"))

            unknown = dict(original)
            unknown["unreviewed"] = True
            self._rewrite_manifest(manifest_path, unknown)
            with self.assertRaisesRegex(ValueError, "artifact_manifest_unknown_field"):
                package_manifest.verify_manifest(REPO_ROOT, manifest_path)

            duplicate = json.loads(json.dumps(original))
            duplicate["artifacts"][1]["path"] = duplicate["artifacts"][0]["path"]
            self._rewrite_manifest(manifest_path, duplicate)
            with self.assertRaisesRegex(ValueError, "artifact_path_duplicate"):
                package_manifest.verify_manifest(REPO_ROOT, manifest_path)

            escaped = json.loads(json.dumps(original))
            escaped["artifacts"][0]["path"] = "../../outside.exe"
            self._rewrite_manifest(manifest_path, escaped)
            with self.assertRaisesRegex(ValueError, "artifact_manifest_path_invalid"):
                package_manifest.verify_manifest(REPO_ROOT, manifest_path)

    def test_delivered_notice_must_match_notice_inside_exact_crate_byte_for_byte(self):
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            prefix = f"FreeRemoteAccess-{self.identity.version}-macos-universal"
            artifacts = []
            for kind, suffix in (("app-archive", "-app.zip"), ("dmg", ".dmg")):
                path = dist / f"{prefix}{suffix}"
                path.write_bytes(kind.encode("ascii"))
                artifacts.append((kind, path))
            support = package_manifest.prepare_fdk_bundle(
                REPO_ROOT, dist / "THIRD_PARTY", self.identity
            )
            manifest_path = package_manifest.write_manifest(
                REPO_ROOT, dist, "macos", "universal", artifacts, support
            )
            notice = support.notice.read_bytes()
            support.notice.write_bytes(b"X" + notice[1:])
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            notice_relative = support.notice.relative_to(dist).as_posix()
            for entry in manifest["files"]:
                if entry["path"] == notice_relative:
                    entry["sha256"] = package_manifest.sha256_file(support.notice)
            package_manifest.write_sha256_sidecar(support.notice)
            self._rewrite_manifest(manifest_path, manifest)

            with self.assertRaisesRegex(ValueError, "fdk_notice_copy_mismatch"):
                package_manifest.verify_manifest(REPO_ROOT, manifest_path)

    @staticmethod
    def _rewrite_manifest(path: Path, manifest: dict):
        path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        package_manifest.write_sha256_sidecar(path)

if __name__ == "__main__":
    unittest.main()
