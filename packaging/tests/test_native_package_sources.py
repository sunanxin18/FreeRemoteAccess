import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class NativePackageSourceTests(unittest.TestCase):
    def read(self, relative: str) -> str:
        return (REPO_ROOT / relative).read_text(encoding="utf-8")

    def test_workflow_pins_actions_and_security_tools_and_runs_native_verifiers(self):
        workflow = self.read(".github/workflows/build-desktop-installers.yml")
        clean_runtime = self.read("packaging/linux/verify-clean-runtime.sh")
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
        self.assertIn("python-version: '3.13'", workflow)
        self.assertIn("runs-on: ubuntu-22.04", workflow)
        self.assertIn("xvfb-run", clean_runtime)
        self.assertIn("weston", workflow)
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
