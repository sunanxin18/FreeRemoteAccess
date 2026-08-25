import importlib.util
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPO_ROOT / "packaging" / "linux" / "verify_runtime_libraries.py"


def load_verifier():
    spec = importlib.util.spec_from_file_location("verify_runtime_libraries", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class LinuxRuntimeLibraryTests(unittest.TestCase):
    def setUp(self):
        self.verifier = load_verifier()
        self.binary = "/tmp/freeremoteaccess"
        self.ldconfig = """
            libX11.so.6 (libc6,x86-64) => /lib/libX11.so.6
            libasound.so.2 (libc6,x86-64) => /lib/libasound.so.2
        """
        self.ldd = {
            self.binary: (0, "libasound.so.2 => /lib/libasound.so.2"),
            "/lib/libX11.so.6": (0, "libc.so.6 => /lib/libc.so.6"),
            "/lib/libasound.so.2": (0, "libc.so.6 => /lib/libc.so.6"),
        }

    def verify(self, **overrides):
        arguments = {
            "binary_path": self.binary,
            "declared": {"libX11.so.6", "libasound.so.2"},
            "needed": {"libasound.so.2", "libc.so.6"},
            "binary_bytes": b"prefix libX11.so.6libX11.so suffix",
            "ldconfig_output": self.ldconfig,
            "ldd_results": self.ldd,
        }
        arguments.update(overrides)
        return self.verifier.validate_runtime_model(**arguments)

    def test_accepts_direct_needed_and_exact_dlopen_soname(self):
        self.verify()

    def test_parses_direct_needed_entries_from_readelf(self):
        output = """
         0x0000000000000001 (NEEDED) Shared library: [libwayland-client.so.0]
         0x0000000000000001 (NEEDED) Shared library: [libz.so.1]
        """
        self.assertEqual(
            {"libwayland-client.so.0", "libz.so.1"},
            self.verifier.parse_needed(output),
        )

    def test_rejects_stale_declaration_not_needed_or_embedded(self):
        with self.assertRaisesRegex(ValueError, "runtime_library_not_used:libXrandr.so.2"):
            self.verify(declared={"libX11.so.6", "libasound.so.2", "libXrandr.so.2"})

    def test_rejects_basename_only_as_dlopen_evidence(self):
        with self.assertRaisesRegex(ValueError, "runtime_library_not_used:libX11.so.6"):
            self.verify(binary_bytes=b"libX11 diagnostic only")

    def test_rejects_undeclared_non_system_needed_library(self):
        with self.assertRaisesRegex(ValueError, "runtime_library_undeclared:libz.so.1"):
            self.verify(needed={"libasound.so.2", "libz.so.1", "libc.so.6"})

    def test_rejects_declared_library_missing_from_loader_cache(self):
        with self.assertRaisesRegex(ValueError, "runtime_library_missing:libX11.so.6"):
            self.verify(ldconfig_output="libasound.so.2 => /lib/libasound.so.2")

    def test_rejects_failed_ldd_closure(self):
        broken = dict(self.ldd)
        broken["/lib/libX11.so.6"] = (0, "libxcb.so.1 => not found")
        with self.assertRaisesRegex(ValueError, "elf_dependency_missing:/lib/libX11.so.6"):
            self.verify(ldd_results=broken)

    def test_rejects_ldd_command_failure(self):
        broken = dict(self.ldd)
        broken[self.binary] = (1, "not a dynamic executable")
        with self.assertRaisesRegex(ValueError, "ldd_failed:/tmp/freeremoteaccess"):
            self.verify(ldd_results=broken)

    def test_repository_declarations_remove_dead_xrandr_and_cover_real_elf_roots(self):
        declarations = self.verifier.read_declarations(
            REPO_ROOT / "packaging" / "linux" / "runtime-libraries.txt"
        )
        self.assertNotIn("libXrandr.so.2", declarations)
        self.assertIn("libxcb.so.1", declarations)
        self.assertIn("libz.so.1", declarations)
        cargo = (REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8")
        lock = (REPO_ROOT / "packaging/linux/ubuntu-jammy-packages.lock").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("libxrandr", cargo.lower())
        self.assertNotIn("libxrandr", lock.lower())


if __name__ == "__main__":
    unittest.main()
