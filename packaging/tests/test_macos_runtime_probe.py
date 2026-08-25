import importlib.util
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class MacOsRuntimeProbeTests(unittest.TestCase):
    def load_probe(self):
        probe_path = REPO_ROOT / "packaging" / "macos" / "probe_macho_launch.py"
        self.assertTrue(probe_path.is_file(), "bounded Mach-O probe is missing")
        spec = importlib.util.spec_from_file_location("probe_macho_launch", probe_path)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_windowserver_unavailable_is_the_only_accepted_early_exit(self):
        probe = self.load_probe()
        accepted_messages = (
            "Unable to connect to the WindowServer.",
            "CGSConnectionByID: No window server",
            "FAILED TO establish the default connection to the WindowServer",
        )
        for message in accepted_messages:
            with self.subTest(message=message):
                self.assertEqual(
                    probe.classify_early_exit(1, message),
                    "windowserver-unavailable",
                )

        rejected_messages = (
            "dyld[42]: Library not loaded: @rpath/libMissing.dylib",
            "Symbol not found: _missing_symbol",
            "Segmentation fault: 11",
            "thread 'main' panicked at unknown startup failure",
            "",
        )
        for message in rejected_messages:
            with self.subTest(message=message):
                with self.assertRaisesRegex(ValueError, "macos_macho_direct_launch_failed"):
                    probe.classify_early_exit(1, message)
        with self.assertRaisesRegex(ValueError, "macos_macho_direct_launch_crashed"):
            probe.classify_early_exit(-6, "Unable to connect to the WindowServer")

    def test_verifier_keeps_full_aqua_path_and_bounded_direct_probe(self):
        probe = self.load_probe()
        verifier = (
            REPO_ROOT / "packaging" / "macos" / "verify-package.sh"
        ).read_text(encoding="utf-8")
        self.assertEqual(probe.PROBE_TIMEOUT_SECONDS, 10)
        for required in (
            'launchctl print "gui/$(id -u)"',
            "stat -f '%Su' /dev/console",
            "pgrep -x WindowServer",
            "open -n",
            "macos-gui-runtime-verification: full-aqua-windowserver-survival",
            "probe_macho_launch.py",
            "macos-gui-runtime-verification: limited-no-aqua-direct-macho",
        ):
            self.assertIn(required, verifier)
        self.assertNotIn("macos_windowserver_launch_unavailable' >&2\n  exit 1", verifier)


if __name__ == "__main__":
    unittest.main()
