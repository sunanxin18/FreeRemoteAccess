import copy
import datetime
import importlib.util
import inspect
import json
import os
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
                "fn mock() {}\n}\n",
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
                "#[cfg(test)]\nmod tests { use rsa::RsaPrivateKey; }\n",
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

            guard_spec = importlib.util.spec_from_file_location("rsa_all_units", guard)
            self.assertIsNotNone(guard_spec)
            self.assertIsNotNone(guard_spec.loader)
            guard_module = importlib.util.module_from_spec(guard_spec)
            guard_spec.loader.exec_module(guard_module)
            manifest = fixture / "Cargo.toml"
            manifest.write_text("[package]\nname='fixture'\nversion='0.1.0'\n", encoding="utf-8")
            build_script = fixture / "build.rs"
            outside_target = fixture / "outside.rs"
            fixture_metadata = {
                "packages": [
                    {
                        "source": None,
                        "manifest_path": str(manifest),
                        "targets": [
                            {"src_path": str(source)},
                            {"src_path": str(build_script)},
                            {"src_path": str(outside_target)},
                        ],
                    }
                ]
            }
            source.write_text("fn clean() {}\n", encoding="utf-8")
            outside_target.write_text("fn clean() {}\n", encoding="utf-8")
            build_script.write_text(
                "use rsa::RsaPrivateKey;\nfn main() {}\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(ValueError, "production_rsa_api_not_allowlisted"):
                guard_module.validate_repository(fixture, metadata=fixture_metadata)
            helper = fixture / "helper.rs"
            build_script.write_text("mod helper;\nfn main() {}\n", encoding="utf-8")
            outside_target.write_text("mod helper;\nfn main() {}\n", encoding="utf-8")
            helper.write_text(
                "use rsa::RsaPrivateKey;\nfn helper() {}\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(
                ValueError, "product_nonstandard_target_modules_unsupported"
            ):
                guard_module.validate_repository(fixture, metadata=fixture_metadata)
            inline_secret = fixture / "inline" / "secret.rs"
            inline_secret.parent.mkdir()
            inline_secret.write_text(
                "use rsa::RsaPrivateKey;\nfn secret() {}\n", encoding="utf-8"
            )
            build_script.write_text(
                "mod inline { mod secret; }\nfn main() {}\n", encoding="utf-8"
            )
            outside_target.write_text("fn clean() {}\n", encoding="utf-8")
            with self.assertRaisesRegex(
                ValueError, "product_nonstandard_target_modules_unsupported"
            ):
                guard_module.validate_repository(fixture, metadata=fixture_metadata)

            outer_secret = fixture / "outer" / "nested" / "secret.rs"
            outer_secret.parent.mkdir(parents=True)
            outer_secret.write_text(
                "use rsa::RsaPrivateKey;\nfn secret() {}\n", encoding="utf-8"
            )
            (fixture / "outer.rs").write_text(
                "mod nested { mod secret; }\n", encoding="utf-8"
            )
            build_script.write_text("mod outer;\nfn main() {}\n", encoding="utf-8")
            with self.assertRaisesRegex(
                ValueError, "product_nonstandard_target_modules_unsupported"
            ):
                guard_module.validate_repository(fixture, metadata=fixture_metadata)

            build_script.write_text(
                '// mod hidden;\nconst TEXT: &str = "mod hidden;";\nfn main() {}\n',
                encoding="utf-8",
            )
            outside_target.write_text("fn clean() {}\n", encoding="utf-8")
            guard_module.validate_repository(fixture, metadata=fixture_metadata)

            build_script.write_text("fn main() {}\n", encoding="utf-8")
            outside_target.write_text(
                "use rsa::hazmat::rsa_decrypt;\nfn main() {}\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(ValueError, "production_rsa_api_not_allowlisted"):
                guard_module.validate_repository(fixture, metadata=fixture_metadata)

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

        original_run = guard.subprocess.run
        metadata_command = []

        def capture_metadata_command(command, **_kwargs):
            metadata_command.extend(command)
            return subprocess.CompletedProcess(command, 0, json.dumps(metadata).encode(), b"")

        guard.subprocess.run = capture_metadata_command
        try:
            guard.load_metadata(REPO_ROOT)
        finally:
            guard.subprocess.run = original_run
        self.assertIn("--all-features", metadata_command)

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

        def add_transitive_wrapper(value):
            wrapper_id = (
                "registry+https://github.com/rust-lang/crates.io-index#wrapper@1.0.0"
            )
            value["packages"].append(
                {
                    "id": wrapper_id,
                    "name": "wrapper",
                    "version": "1.0.0",
                    "source": "registry+https://github.com/rust-lang/crates.io-index",
                    "manifest_path": "registry/wrapper/Cargo.toml",
                    "targets": [],
                }
            )
            picky_id = next(
                package["id"]
                for package in value["packages"]
                if package["name"] == "picky" and package["version"] == "7.0.0-rc.25"
            )
            value["resolve"]["nodes"].append(
                {
                    "id": wrapper_id,
                    "features": [],
                    "deps": [{"name": "picky", "pkg": picky_id, "dep_kinds": []}],
                }
            )
            root_id = value["resolve"]["root"]
            next(node for node in value["resolve"]["nodes"] if node["id"] == root_id)[
                "deps"
            ].append({"name": "wrapper", "pkg": wrapper_id, "dep_kinds": []})

        rejected(add_transitive_wrapper, "rsa_reverse_closure_changed")

        def rename_root_rsa_edge(value):
            rsa_id = next(
                package["id"]
                for package in value["packages"]
                if package["name"] == "rsa" and package["version"] == "0.9.10"
            )
            root_id = value["resolve"]["root"]
            dependency = next(
                dependency
                for dependency in next(
                    node for node in value["resolve"]["nodes"] if node["id"] == root_id
                )["deps"]
                if dependency["pkg"] == rsa_id
            )
            dependency["name"] = "crypto"

        rejected(rename_root_rsa_edge, "rsa_reverse_closure_changed")

        def change_root_rsa_edge_kind(value):
            rsa_id = next(
                package["id"]
                for package in value["packages"]
                if package["name"] == "rsa" and package["version"] == "0.9.10"
            )
            root_id = value["resolve"]["root"]
            dependency = next(
                dependency
                for dependency in next(
                    node for node in value["resolve"]["nodes"] if node["id"] == root_id
                )["deps"]
                if dependency["pkg"] == rsa_id
            )
            dependency["dep_kinds"] = [{"kind": "dev", "target": None}]

        rejected(change_root_rsa_edge_kind, "rsa_reverse_closure_changed")

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

        def add_external_target(value):
            root = next(
                package
                for package in value["packages"]
                if package["id"] == value["resolve"]["root"]
            )
            root["targets"].append(
                {
                    "name": "outside",
                    "kind": ["bin"],
                    "crate_types": ["bin"],
                    "src_path": str(REPO_ROOT.parent / "outside.rs"),
                }
            )

        rejected(add_external_target, "product_target_outside_repository")

        with tempfile.TemporaryDirectory(dir=REPO_ROOT) as temporary_target:
            hidden_root = Path(temporary_target)
            hidden_target = hidden_root / "hidden.rs"
            hidden_target.write_text("mod credential_alias;\n", encoding="utf-8")
            (hidden_root / "credential_alias.rs").write_text(
                "use ironrdp_connector::Credentials as C;\n"
                "fn hidden() { let _ = C::SmartCard; }\n",
                encoding="utf-8",
            )
            hidden_metadata = copy.deepcopy(metadata)
            hidden_root_package = next(
                package
                for package in hidden_metadata["packages"]
                if package["id"] == hidden_metadata["resolve"]["root"]
            )
            hidden_root_package["targets"].append(
                {
                    "name": "hidden_feature_target",
                    "kind": ["bin"],
                    "crate_types": ["bin"],
                    "src_path": str(hidden_target),
                }
            )
            with self.assertRaisesRegex(ValueError, "product_target_set_changed"):
                guard.validate_metadata(hidden_metadata, REPO_ROOT)
            with self.assertRaisesRegex(
                ValueError, "product_nonstandard_target_modules_unsupported"
            ):
                guard.validate_product_api(REPO_ROOT, hidden_metadata)

        with tempfile.TemporaryDirectory() as temporary:
            fixture = Path(temporary)
            controlled = fixture / "src" / "lib.rs"
            connector = fixture / "vendor" / "ironrdp-client" / "src" / "config.rs"
            controlled.parent.mkdir(parents=True)
            connector.parent.mkdir(parents=True)
            controlled.write_text('include!("outside.rs");\n', encoding="utf-8")
            connector.write_text(
                "let connector = ironrdp_connector::Config {\n"
                "credentials: ironrdp_connector::Credentials::UsernamePassword {\n"
                "username: self.username.unwrap_or_default(),\n"
                "password: self.password.unwrap_or_default(),\n"
                "},\n"
                "domain: self.domain,\n",
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

            credential_alias_bypasses = (
                "use ironrdp_connector::Credentials as C;\nfn bypass() { let _ = C::SmartCard; }\n",
                "use ironrdp_connector::Credentials::{SmartCard as Card};\nfn bypass() { let _ = Card; }\n",
                "use ironrdp_connector::Credentials::*;\nfn bypass() {}\n",
                "use ironrdp_connector::Credentials as C;\nfn bypass() { let _ = C::UsernamePassword; }\n",
            )
            exact = (
                "let connector = ironrdp_connector::Config {\n"
                "credentials: ironrdp_connector::Credentials::UsernamePassword {\n"
                "username: self.username.unwrap_or_default(),\n"
                "password: self.password.unwrap_or_default(),\n"
                "},\n"
                "domain: self.domain,\n"
            )
            for bypass in credential_alias_bypasses:
                with self.subTest(bypass=bypass.splitlines()[0]):
                    connector.write_text(exact + bypass, encoding="utf-8")
                    with self.assertRaisesRegex(
                        ValueError, "transitive_private_api_reachable"
                    ):
                        guard.validate_product_api(fixture)

            controlled.write_text("mod child;\n", encoding="utf-8")
            child = controlled.parent / "child.rs"
            child.write_text("fn child() {}\n", encoding="utf-8")
            connector.write_text(exact, encoding="utf-8")
            guard.validate_product_api(fixture)

            controlled.write_text(
                '#[cfg_attr(feature = "escape", path = "../outside.rs")]\n'
                "mod escaped;\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "product_rust_source_escape"):
                guard.validate_product_api(fixture)

            real_module = fixture / "real-module"
            real_module.mkdir()
            (real_module / "child.rs").write_text("fn child() {}\n", encoding="utf-8")
            child.unlink()
            try:
                if os.name == "nt":
                    linked = subprocess.run(
                        [
                            "cmd",
                            "/c",
                            "mklink",
                            "/J",
                            str(controlled.parent / "child"),
                            str(real_module),
                        ],
                        capture_output=True,
                        check=False,
                    )
                    if linked.returncode != 0:
                        self.skipTest("directory junction unavailable")
                    controlled.write_text("mod child { mod child; }\n", encoding="utf-8")
                else:
                    (controlled.parent / "child.rs").symlink_to(real_module / "child.rs")
                with self.assertRaisesRegex(ValueError, "product_source_symlink_ancestor"):
                    guard.validate_product_api(fixture)
            finally:
                if os.name == "nt" and (controlled.parent / "child").exists():
                    os.rmdir(controlled.parent / "child")

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
        self.assertIn("'Identifier: Packages'", installer)
        self.assertIn("$(IDENTIFIER)|$(CREATED_BY)|$(SITE)", installer)
        self.assertIn("verify-snapshot-resolution.sh", installer)
        self.assertIn("--simulate install", installer)
        self.assertIn("--print-uris --yes install", installer)
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

    def test_snapshot_resolution_verifier_uses_actual_snapshot_uris(self):
        verifier_path = (
            REPO_ROOT / "packaging" / "linux" / "verify-snapshot-resolution.sh"
        )
        self.assertTrue(verifier_path.is_file(), "snapshot resolution verifier missing")
        snapshot = "20260810T000000Z"
        github_runner_index_lines = []
        for release in ("jammy", "jammy-updates", "jammy-security"):
            logical_site = (
                "http://security.ubuntu.com/ubuntu"
                if release == "jammy-security"
                else "http://archive.ubuntu.com/ubuntu"
            )
            for component in ("main", "universe", "restricted", "multiverse"):
                for site in (
                    logical_site,
                    f"https://snapshot.ubuntu.com/ubuntu/{snapshot}",
                ):
                    github_runner_index_lines.extend(
                        (
                            f"Packages|Packages|{site}|{release}|{component}",
                            f"Translations|Translations|{site}|{release}|{component}",
                            f"CNF|CNF|{site}|{release}|{component}",
                        )
                    )
        package_index_lines = [
            line
            for line in github_runner_index_lines
            if line.startswith("Packages|Packages|")
        ]
        self.assertEqual(len(package_index_lines), 24)
        indices = "\n".join(package_index_lines) + "\n"
        unfiltered_indices = "\n".join(github_runner_index_lines) + "\n"
        plan = (
            "Inst ca-certificates (20260601~22.04.1 Ubuntu:22.04/jammy-updates [all])\n"
            "Inst libx11-6 (2:1.7.5-1ubuntu0.3 Ubuntu:22.04/jammy-updates, "
            "Ubuntu:22.04/jammy-security [amd64])\n"
        )
        uris = (
            f"'https://snapshot.ubuntu.com/ubuntu/{snapshot}/pool/main/c/ca-certificates/"
            "ca-certificates_20260601%7e22.04.1_all.deb' "
            "ca-certificates_20260601~22.04.1_all.deb 1 SHA256:00\n"
            f"'https://snapshot.ubuntu.com/ubuntu/{snapshot}/pool/main/libx/libx11/"
            "libx11-6_1.7.5-1ubuntu0.3_amd64.deb' "
            "libx11-6_2%3a1.7.5-1ubuntu0.3_amd64.deb 666946 MD5Sum:00\n"
        )
        fixture_root = REPO_ROOT / "target" / "package"
        fixture_root.mkdir(parents=True, exist_ok=True)

        def verify(index_text, plan_text, uri_text):
            with tempfile.TemporaryDirectory(dir=fixture_root) as temporary:
                fixture = Path(temporary)
                (fixture / "indices").write_text(
                    index_text, encoding="utf-8", newline="\n"
                )
                (fixture / "plan").write_text(
                    plan_text, encoding="utf-8", newline="\n"
                )
                (fixture / "uris").write_text(
                    uri_text, encoding="utf-8", newline="\n"
                )
                relative = fixture.relative_to(REPO_ROOT)
                return subprocess.run(
                    [
                        "bash",
                        "packaging/linux/verify-snapshot-resolution.sh",
                        snapshot,
                        (relative / "indices").as_posix(),
                        (relative / "plan").as_posix(),
                        (relative / "uris").as_posix(),
                    ],
                    cwd=REPO_ROOT,
                    capture_output=True,
                    encoding="utf-8",
                    errors="replace",
                    check=False,
                )

        accepted = verify(indices, plan, uris)
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        rejected = verify(unfiltered_indices, plan, uris)
        self.assertIn("snapshot_index_target_kind_mismatch", rejected.stderr)
        rejected = verify(indices.replace(snapshot, "20260809T000000Z", 1), plan, uris)
        self.assertIn("snapshot_index_pair_mismatch", rejected.stderr)
        rejected = verify(indices, plan, uris.replace(snapshot, "20260809T000000Z", 1))
        self.assertIn("snapshot_uri_mismatch", rejected.stderr)
        rejected = verify(
            indices,
            plan.replace("libx11-6 (2:1.7.5", "libx11-6 (3:1.7.5"),
            uris,
        )
        self.assertIn("snapshot_resolution_mismatch", rejected.stderr)
        rejected = verify(
            indices,
            plan,
            uris.replace(
                "libx11-6_1.7.5-1ubuntu0.3_amd64.deb'",
                "libx11-6_1.7.5-1ubuntu0.2_amd64.deb'",
            ),
        )
        self.assertIn("snapshot_uri_filename_mismatch", rejected.stderr)

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
