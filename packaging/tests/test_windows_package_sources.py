import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class WindowsPackageSourceTests(unittest.TestCase):
    def read(self, relative: str) -> str:
        return (REPO_ROOT / relative).read_text(encoding="utf-8")

    def test_separate_gui_subsystem_launcher_preserves_console_cli(self):
        cargo = self.read("Cargo.toml")
        launcher = self.read("src/bin/freeremoteaccess-gui.rs")
        cli = self.read("src/main.rs")
        build = self.read("build.rs")

        self.assertIn('name = "freeremoteaccess-gui"', cargo)
        self.assertIn('windows_subsystem = "windows"', launcher)
        self.assertNotIn('windows_subsystem = "windows"', cli)
        self.assertIn("MessageBoxW", launcher)
        self.assertIn("CARGO_PKG_VERSION", build)
        self.assertIn("windows_app.manifest", build)
        self.assertIn("set_icon", build)

    def test_wix_uses_injected_version_stable_upgrade_and_installs_support_bundle(self):
        wix = self.read("packaging/windows/wix/main.wxs")
        self.assertIn('Version="$(PackageVersion)"', wix)
        self.assertIn('UpgradeCode="94803D29-B9FA-47D7-9C43-C9665ABAC5B4"', wix)
        self.assertIn("<MajorUpgrade", wix)
        self.assertIn("StartMenuShortcut", wix)
        self.assertIn("aac\\NOTICE", wix)
        self.assertIn("$(FdkArchiveName)", wix)

    def test_windows_builder_uses_canonical_version_and_embeds_support_in_zip_and_msi(self):
        builder = self.read("packaging/windows/build-msi.ps1")
        version = __import__("tomllib").loads(self.read("Cargo.toml"))["package"]["version"]
        self.assertNotIn(version, builder)
        self.assertIn("package_manifest.py", builder)
        self.assertIn("msi-version", builder)
        self.assertIn("prepare-fdk", builder)
        self.assertIn("FdkInfo.source_archive", builder)
        self.assertIn("THIRD_PARTY", builder)
        self.assertIn("Compress-Archive", builder)
        self.assertIn("artifact-manifest.json", builder)

    def test_windows_cleanup_is_exact_and_reparse_safe(self):
        builder = self.read("packaging/windows/build-msi.ps1")
        verifier = self.read("packaging/windows/verify-package.ps1")
        self.assertNotIn("GetRelativePath", builder)
        self.assertNotIn("GetRelativePath", verifier)
        self.assertIn("DirectorySeparatorChar", builder)
        self.assertIn("Substring($RootPrefix.Length)", builder)
        self.assertIn("ReparsePoint", builder)
        self.assertNotIn("StartsWith($RepoRoot", builder)
        self.assertIn("Assert-SafeCleanupRoot", verifier)
        self.assertIn("ReparsePoint", verifier)

    def test_native_verifier_checks_pe_zip_msi_lifecycle_and_cleans_on_failure(self):
        verifier = self.read("packaging/windows/verify-package.ps1")
        for required in (
            "Get-PeSubsystem",
            "FindResource",
            "VersionInfo",
            "--help",
            "Expand-Archive",
            "msi-admin",
            "install-previous.log",
            "upgrade-current.log",
            "uninstall-current.log",
            "cleanup-after-failure.log",
            "MainWindowHandle",
            "msi_install_directory_remained_after_uninstall",
            "Assert-ExecutableSet",
            "Get-FileHash",
            "Start-Process -FilePath $Shortcut",
            "msi_reboot_required_not_allowed",
            "Assert-LifecycleRemoved",
        ):
            self.assertIn(required, verifier)
        self.assertNotIn("Win32_Product", verifier)
        self.assertNotIn("Start-Process -FilePath $ShortcutInfo.TargetPath", verifier)


if __name__ == "__main__":
    unittest.main()
