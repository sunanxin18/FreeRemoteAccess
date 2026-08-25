import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class MacLinuxPackageSourceTests(unittest.TestCase):
    def read(self, relative: str) -> str:
        return (REPO_ROOT / relative).read_text(encoding="utf-8")

    def test_mac_builder_derives_version_embeds_support_and_builds_archive_and_dmg(self):
        builder = self.read("packaging/macos/build-packages.sh")
        plist = self.read("packaging/macos/Info.plist")
        version = __import__("tomllib").loads(self.read("Cargo.toml"))["package"]["version"]
        self.assertNotIn(version, builder)
        self.assertNotIn(version, plist)
        for required in (
            "package_manifest.py",
            "prepare-fdk",
            "THIRD_PARTY",
            "MACOSX_DEPLOYMENT_TARGET=12.0",
            "-app.zip",
            "hdiutil create",
            "verify-package.sh",
        ):
            self.assertIn(required, builder)
        self.assertIn("@PACKAGE_VERSION@", plist)

    def test_mac_native_verifier_checks_bundle_arch_dmg_unsigned_and_gui_survival(self):
        verifier = self.read("packaging/macos/verify-package.sh")
        for required in (
            "plutil -extract CFBundleShortVersionString",
            "lipo -archs",
            "otool -arch",
            "hdiutil verify",
            "hdiutil attach -readonly",
            "codesign -dvvv",
            "spctl --assess",
            "macos_windowserver_launch_unavailable",
            "macos_gui_did_not_survive",
            "package_fdk_source_hash_mismatch",
            "package_fdk_notice_mismatch",
        ):
            self.assertIn(required, verifier)

    def test_linux_builder_pins_tool_and_runtime_retains_appdir_and_embeds_support(self):
        builder = self.read("packaging/linux/build-packages.sh")
        libraries = self.read("packaging/linux/runtime-libraries.txt")
        cargo = self.read("Cargo.toml")
        self.assertNotIn("/continuous/", builder)
        for required in (
            "appimagetool-1.9.1",
            "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0",
            "type2-runtime/releases/download/20251108/runtime-x86_64",
            "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d",
            "--runtime-file",
            "prepare-fdk",
            "THIRD_PARTY",
            "tar --zstd",
            "dpkg-deb",
            '--directory "appdir=$appdir"',
        ):
            self.assertIn(required, builder)
        self.assertNotIn('rm -rf -- "$appdir"', builder)
        for soname in (
            "libX11.so.6",
            "libX11-xcb.so.1",
            "libxkbcommon-x11.so.0",
            "libwayland-client.so.0",
            "libvulkan.so.1",
            "libasound.so.2",
        ):
            self.assertIn(soname, libraries)
        for package in (
            "libx11-6",
            "libx11-xcb1",
            "libxkbcommon-x11-0",
            "libwayland-client0",
            "libvulkan1",
            "libasound2",
        ):
            self.assertIn(package, cargo)

    def test_linux_native_verifier_extracts_every_package_and_enforces_runtime_and_abi(self):
        verifier = self.read("packaging/linux/verify-package.sh")
        for required in (
            "tar --zstd -xf",
            "dpkg-deb -x",
            "--appimage-extract",
            "assert_support",
            "readlink",
            "ldconfig -p",
            "strings",
            "ldd",
            "objdump -T",
            "2.35",
            "xvfb-run",
            "LIBGL_ALWAYS_SOFTWARE=1",
            "WGPU_BACKEND=vulkan",
            '"$work_dir/x11-gui.log"',
        ):
            self.assertIn(required, verifier)
        self.assertNotIn("APPIMAGE_EXTRACT_AND_RUN=1 \"$appimage\" --appimage-extract", verifier)
        self.assertNotIn("ldconfig -p | grep -Fq", verifier)
        self.assertNotIn("strings \"$binary\" | grep -Fiq", verifier)

    def test_clean_ubuntu_runtime_verifier_launches_x11_and_wayland(self):
        verifier = self.read("packaging/linux/verify-clean-runtime.sh")
        for required in (
            "xvfb-run",
            "weston --backend=headless-backend.so",
            "WAYLAND_DISPLAY=wayland-frd",
            "XDG_RUNTIME_DIR",
            "WGPU_BACKEND=vulkan",
            "x11-gui.log",
            "wayland-gui.log",
        ):
            self.assertIn(required, verifier)


if __name__ == "__main__":
    unittest.main()
