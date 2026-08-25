import copy
import datetime
import importlib.util
import inspect
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class NativePackageSourceTests(unittest.TestCase):
    def read(self, relative: str) -> str:
        return (REPO_ROOT / relative).read_text(encoding="utf-8")

    def test_workflow_pins_actions_and_security_tools_and_runs_native_verifiers(self):
        workflow = self.read(".github/workflows/build-desktop-installers.yml")
        clean_runtime = self.read("packaging/linux/verify-clean-runtime.sh")
        jammy_lock = self.read("packaging/linux/ubuntu-jammy-packages.lock")
        for immutable in (
            "fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09",
            "330a01c490aca151604b8cf639adc76d48f6c5d4",
            "6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772",
            "ece7cb06caefa5fff74198d8649806c4678c61a1",
        ):
            self.assertIn(immutable, workflow)
        for pinned_tool in (
            "cargo-audit --version 0.22.2",
            "cargo-deny --version 0.20.2",
            "cargo-deb --version 3.7.0",
            "cargo-generate-rpm --version 0.21.0",
            "wix --version).Trim() -ne '4.0.6'",
        ):
            self.assertIn(pinned_tool, workflow)
        self.assertIn("cargo audit", workflow)
        self.assertIn("cargo deny check", workflow)
        self.assertIn("verify-package", workflow)
        self.assertIn("python-version: '3.13.14'", workflow)
        self.assertIn("runs-on: ubuntu-22.04", workflow)
        self.assertIn("xvfb-run", clean_runtime)
        self.assertIn("weston=", jammy_lock)
        self.assertIn("LIBGL_ALWAYS_SOFTWARE", clean_runtime)
        self.assertIn("verify-clean-runtime.sh", workflow)
        self.assertIn("if: always()", workflow)
        self.assertIn("permissions:\n  contents: read", workflow)
        self.assertGreaterEqual(workflow.count("persist-credentials: false"), 4)
        self.assertGreaterEqual(workflow.count("timeout-minutes:"), 4)
        self.assertGreaterEqual(workflow.count("needs: verify"), 3)
        self.assertIn(
            "ubuntu@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc",
            workflow,
        )
        self.assertNotIn("path: dist/windows/*", workflow)
        self.assertNotIn("path: dist/macos/*", workflow)
        self.assertNotIn("path: dist/linux/*", workflow)
        self.assertNotIn("-SkipLifecycle", workflow)

    def test_rsa_advisory_exception_is_exact_scoped_and_time_bounded(self):
        workflow = self.read(".github/workflows/build-desktop-installers.yml")
        deny = self.read("deny.toml")
        readme = self.read("README.md")
        self.assertEqual(workflow.count("--ignore RUSTSEC-2023-0071"), 1)
        self.assertEqual(deny.count('"RUSTSEC-2023-0071"'), 1)
        self.assertIn("packaging/check_rsa_usage.py --repo .", workflow)
        self.assertIn("packaging/check_rsa_dependency_boundary.py --repo .", workflow)
        self.assertIn("RUSTSEC-2023-0071", readme)
        self.assertIn("RsaPublicKey", readme)
        self.assertIn("2026-11-30", readme)
        self.assertIn("一旦上游提供 patched version", readme)

    def test_rsa_usage_guard_is_strict_allowlist_and_excludes_test_mock(self):
        guard = REPO_ROOT / "packaging" / "check_rsa_usage.py"
        self.assertTrue(guard.is_file(), "production RSA usage guard is missing")
        repository = subprocess.run(
            [sys.executable, str(guard), "--repo", str(REPO_ROOT)],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(repository.returncode, 0, repository.stderr)
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            source = fixture / "src" / "lib.rs"
            source.parent.mkdir()
            source.write_text(
                "#[cfg(test)]\nmod tests {\n"
                "use rsa::RsaPrivateKey;\nfn mock(k: RsaPrivateKey) { let _ = k.decrypt(); }\n}\n",
                encoding="utf-8",
            )
            allowed = subprocess.run(
                [sys.executable, str(guard), "--repo", str(fixture)],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(allowed.returncode, 0, allowed.stderr)

            bypasses = (
                "use rsa::RsaPrivateKey;\nfn production(k: RsaPrivateKey) { let _ = k.decrypt(); }\n",
                "use rsa::pkcs1v15::DecryptingKey;\nfn production() {}\n",
                "use rsa::traits::RandomizedDecryptor;\nfn production<T: RandomizedDecryptor>(k: T) { let _ = k.decrypt_with_rng(); }\n",
                "fn production<T>(k: T) { let _ = rsa::traits::Decryptor::decrypt(&k); }\n",
                "use ::rsa as x;\nfn production() { let _ = x::hazmat::rsa_decrypt(); }\n",
                # Even a new public-only use outside the single approved owner must fail.
                "use rsa::RsaPublicKey;\nfn production(_: RsaPublicKey) {}\n",
            )
            for bypass in bypasses:
                with self.subTest(bypass=bypass.splitlines()[0]):
                    source.write_text(bypass, encoding="utf-8")
                    rejected = subprocess.run(
                        [sys.executable, str(guard), "--repo", str(fixture)],
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    self.assertEqual(rejected.returncode, 2)
                    self.assertIn("production_rsa_api_not_allowlisted", rejected.stderr)

    def test_rsa_advisory_guard_fails_closed_on_review_expiry(self):
        guard_path = REPO_ROOT / "packaging" / "check_rsa_usage.py"
        spec = importlib.util.spec_from_file_location("check_rsa_usage", guard_path)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        self.assertIn("today", inspect.signature(guard.validate_repository).parameters)
        with self.assertRaisesRegex(ValueError, "rsa_advisory_exception_review_expired"):
            guard.validate_repository(REPO_ROOT, today=datetime.date(2026, 11, 30))

    def test_rsa_dependency_boundary_locks_graph_and_product_api(self):
        guard_path = REPO_ROOT / "packaging" / "check_rsa_dependency_boundary.py"
        self.assertTrue(guard_path.is_file(), "RSA dependency boundary guard is missing")
        spec = importlib.util.spec_from_file_location("check_rsa_dependency_boundary", guard_path)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)

        metadata = guard.load_metadata(REPO_ROOT)
        guard.validate_metadata(metadata, REPO_ROOT)
        guard.validate_product_api(REPO_ROOT)

        def rejected(mutator, error):
            fixture = copy.deepcopy(metadata)
            mutator(fixture)
            with self.assertRaisesRegex(ValueError, error):
                guard.validate_metadata(fixture, REPO_ROOT)

        rejected(
            lambda value: value["packages"].append(
                {
                    "id": "registry+https://github.com/rust-lang/crates.io-index#rsa@9.9.9",
                    "name": "rsa",
                    "version": "9.9.9",
                    "source": "registry+https://github.com/rust-lang/crates.io-index",
                    "manifest_path": "registry/rsa/Cargo.toml",
                }
            ),
            "rsa_dependency_set_changed",
        )

        def add_workspace_member(value):
            value["workspace_members"].append("path+file:///outside#rogue@1.0.0")

        rejected(add_workspace_member, "workspace_member_set_changed")

        def add_rsa_feature(value):
            rsa_id = next(
                package["id"]
                for package in value["packages"]
                if package["name"] == "rsa" and package["version"] == "0.10.0-rc.18"
            )
            next(node for node in value["resolve"]["nodes"] if node["id"] == rsa_id)[
                "features"
            ].append("new-private-api")

        rejected(add_rsa_feature, "rsa_feature_set_changed")

        def change_rsa_source(value):
            next(
                package
                for package in value["packages"]
                if package["name"] == "rsa" and package["version"] == "0.9.10"
            )["source"] = "git+https://example.invalid/rsa"

        rejected(change_rsa_source, "rsa_dependency_source_changed")

        def add_rsa_parent(value):
            rsa_id = next(
                package["id"]
                for package in value["packages"]
                if package["name"] == "rsa" and package["version"] == "0.10.0-rc.18"
            )
            root_id = value["resolve"]["root"]
            next(node for node in value["resolve"]["nodes"] if node["id"] == root_id)[
                "deps"
            ].append({"name": "rsa", "pkg": rsa_id, "dep_kinds": []})

        rejected(add_rsa_parent, "rsa_reverse_dependency_set_changed")

        def add_boundary_feature(value):
            package_id = next(
                package["id"]
                for package in value["packages"]
                if package["name"] == "sspi" and package["version"] == "0.21.3"
            )
            next(node for node in value["resolve"]["nodes"] if node["id"] == package_id)[
                "features"
            ].append("new-credential-kind")

        rejected(add_boundary_feature, "rsa_boundary_feature_set_changed")

        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            controlled = fixture / "src" / "lib.rs"
            connector = fixture / "vendor" / "ironrdp-client" / "src" / "config.rs"
            controlled.parent.mkdir(parents=True)
            connector.parent.mkdir(parents=True)
            controlled.write_text('include!("outside.rs");\n', encoding="utf-8")
            connector.write_text(
                "let credentials = Credentials::UsernamePassword(user, password);\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "product_rust_source_escape"):
                guard.validate_product_api(fixture)
            controlled.write_text("fn ok() {}\n", encoding="utf-8")
            connector.write_text(
                "let credentials = Credentials::SmartCard(identity);\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "transitive_private_api_reachable"):
                guard.validate_product_api(fixture)

    def test_ubuntu_dependencies_use_one_immutable_snapshot_and_exact_lock(self):
        workflow = self.read(".github/workflows/build-desktop-installers.yml")
        installer_path = REPO_ROOT / "packaging/linux/install-ubuntu-jammy-snapshot.sh"
        package_lock_path = REPO_ROOT / "packaging/linux/ubuntu-jammy-packages.lock"
        self.assertTrue(installer_path.is_file(), "snapshot installer is missing")
        self.assertTrue(package_lock_path.is_file(), "snapshot package lock is missing")
        installer = installer_path.read_text(encoding="utf-8")
        fetcher = self.read("packaging/linux/fetch-ca-bootstrap.py")
        package_lock = package_lock_path.read_text(encoding="utf-8")
        self.assertNotIn("apt-get update", workflow)
        self.assertNotIn("apt-get install", workflow)
        self.assertGreaterEqual(
            workflow.count("install-ubuntu-jammy-snapshot.sh"), 3
        )
        self.assertIn("snapshot=20260810T000000Z", installer)
        self.assertIn("apt-get --version", installer)
        self.assertIn("2.4.11", installer)
        self.assertIn("--no-install-recommends", installer)
        self.assertIn("APT::Update::Error-Mode=any", installer)
        self.assertIn("Acquire::https::CaInfo", installer)
        self.assertIn("dpkg-deb", installer)
        self.assertIn("indextargets", installer)
        self.assertIn("snapshot_candidate_mismatch", installer)
        self.assertIn("ca-certificates_20260601~22.04.1_all.deb", fetcher)
        self.assertIn(
            "6e8cdcc8c86103acd4fc14649eac62ff2037108389074a7b167567af33c32245",
            fetcher,
        )
        self.assertIn("fetch-ca-bootstrap.py", workflow)
        self.assertIn("--ca-bootstrap-deb", workflow)
        self.assertIn("test ! -s /etc/ssl/certs/ca-certificates.crt", workflow)
        self.assertIn("test -s /etc/ssl/certs/ca-certificates.crt", workflow)
        self.assertNotIn("latest", installer.lower())
        locked_lines = [
            line for line in package_lock.splitlines() if line and not line.startswith("#")
        ]
        self.assertGreaterEqual(len(locked_lines), 20)
        self.assertTrue(all("=" in line for line in locked_lines))

    def test_supply_chain_policy_and_locked_fetch_are_explicit(self):
        deny = self.read("deny.toml")
        self.assertIn('unknown-git = "deny"', deny)
        self.assertIn('unknown-registry = "deny"', deny)
        self.assertIn('yanked = "deny"', deny)
        for script in (
            "packaging/windows/build-msi.ps1",
            "packaging/macos/build-packages.sh",
            "packaging/linux/build-packages.sh",
        ):
            self.assertIn("cargo fetch --locked", self.read(script))

    def test_readme_keeps_unsigned_patent_and_artifact_set_gate_visible(self):
        readme = self.read("README.md")
        self.assertIn("UNSIGNED", readme)
        self.assertIn("NOT FOR PUBLIC DISTRIBUTION", readme)
        self.assertIn("不授予 AAC 专利许可", readme)
        self.assertIn("不能脱离同一 canonical artifact set 单独分发", readme)


if __name__ == "__main__":
    unittest.main()
