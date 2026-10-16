"""Linux shipping旁路纯host合成fixture；不编译、链接或执行产品。"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import struct
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("linux_shipping", ROOT / "tools/collect-linux-publication-shipping.py")
COL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COL)


class ShippingFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="frd-shipping-host-")
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        for name in [name for name, _ in COL.SOURCES] + list(COL.CONFIGS):
            dest = self.repo / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, dest)
        self.env = mock.patch.dict(os.environ, {"FRD_LINUX_PUBLICATION_SHIPPING": "1", "GITHUB_ACTIONS": "true"}, clear=True)
        self.env.start()
        self.selected = "linux-x86_64"
        self.calls = []
        self.exit_code = 0
        self.no_map = False
        self.partial = False
        self.linked = False
        tools = self.repo / "fixture-tools"
        tools.mkdir()
        self.compiler = tools / "rustc"
        self.compiler.write_bytes(b"host synthetic compiler identity; never executed")
        self.cargo = tools / "cargo"
        self.cargo.write_bytes(b"host synthetic cargo identity; never executed")
        self.lookup = mock.patch.object(COL.shutil, "which", side_effect=lambda name: str(tools / name))
        self.lookup.start()
        self.capture = mock.patch.object(COL, "load_capture", return_value=self.synthetic_build)
        self.capture.start()

    def tearDown(self):
        self.capture.stop()
        self.lookup.stop()
        self.env.stop()
        self.temp.cleanup()

    def paths(self):
        return COL.locations(self.repo, self.selected)

    def synthetic_build(self, stage, argv, repo, env, evidence):
        self.assertEqual(stage, "build")
        self.assertEqual(repo, self.repo)
        self.calls.append(argv)
        paths = self.paths()
        paths["built"].parent.mkdir(parents=True, exist_ok=True)
        data = bytearray(64)
        data[:7] = b"\x7fELF" + bytes((COL.PROFILES[self.selected][1], 1, 1))
        struct.pack_into("<H", data, 18, COL.PROFILES[self.selected][2])
        paths["built"].write_bytes(data + b"host fixture, not Rust output")
        paths["built"].chmod(0o755)
        if self.linked:
            os.link(paths["built"], paths["built"].with_name("ordinary-deps-alias"))
        if not self.no_map:
            paths["map"].write_bytes(b"host synthetic map; no linker executed\n")
        args = [str(self.compiler), "--crate-name", COL.CRATE, "apps/freeremotedesk-linux/src/main.rs", "--crate-type", "bin", "--emit=dep-info,link",
                "--target", COL.PROFILES[self.selected][0], "--cfg", 'feature="default"', "--cfg", 'feature="gtk-shell"',
                "-C", "opt-level=3", "-C", "lto=thin", "-C", argv[-1]]
        (evidence / "build.stderr").write_text("     Running `" + shlex.join(args) + "`\n")
        (evidence / "build.stdout").write_bytes(b"host fixture only\n")
        return dict(stage="build", argv=argv, started=True, pid=None, terminal_observed=True, exit_code=self.exit_code,
                    capture_complete=not self.partial, stdout_eof=not self.partial, stderr_eof=True, files_closed=not self.partial,
                    failure_exit=self.exit_code or (1 if self.partial else 0), first_failure="host fixture partial/native failure" if self.exit_code or self.partial else None)

    def build(self):
        return COL.build(self.repo, self.selected, self.paths())

    def staged(self):
        paths = self.paths()
        paths["staged"].parent.mkdir(parents=True)
        shutil.copyfile(paths["built"], paths["staged"])
        paths["staged"].chmod(0o755)

    def identity(self):
        return json.loads((self.paths()["evidence"] / "identity.json").read_bytes())

    def test_both_targets_one_ordinary_build_and_final_same_bytes(self):
        for selected in COL.PROFILES:
            self.selected = selected
            with self.subTest(selected=selected):
                self.assertEqual(self.build(), 0)
                self.assertTrue(self.identity()["build_complete"])
                self.assertFalse(self.identity()["collection_complete"])
                self.staged()
                self.assertEqual(COL.finalize(self.repo, selected, self.paths()), 0)
                result = self.identity()
                self.assertTrue(result["collection_complete"])
                self.assertEqual(result["built_elf"], result["staged_elf"])
                self.assertEqual(result["saved_elf"], result["staged_elf"])
                self.assertEqual(result["link_map"], result["final_link_map"])
                self.assertEqual(result["runtime"], "NOT_RUN")
                names = {p.name for p in self.paths()["evidence"].iterdir()}
                self.assertEqual(names, {COL.APPLICATION, COL.APPLICATION + ".map", "build.stdout", "build.stderr", "identity.json"} | {snapshot for _, snapshot in COL.SOURCES})
        self.assertEqual(len(self.calls), 2)
        for args in self.calls:
            self.assertEqual(args[:9], [str(self.cargo), "rustc", "--locked", "--release", "-v", "-p", COL.APPLICATION, "--bin", COL.APPLICATION])
            self.assertNotIn("--test", args)
            self.assertNotIn("--no-default-features", args)
            self.assertEqual(args.count("--target"), 1)
            self.assertTrue(args[-1].startswith("link-arg=-Wl,-Map,"))

    def test_cargo_hardlinked_ordinary_elf_is_only_read_and_copied(self):
        self.linked = True
        self.assertEqual(self.build(), 0)
        original = self.paths()["built"].read_bytes()
        self.assertEqual(self.paths()["built"].stat().st_nlink, 2)
        self.staged()
        self.assertEqual(COL.finalize(self.repo, self.selected, self.paths()), 0)
        self.assertEqual(self.paths()["built"].read_bytes(), original)

    def test_missing_map_native0_is_partial(self):
        self.no_map = True
        self.assertEqual(self.build(), 1)
        result = self.identity()
        self.assertEqual(result["build"]["exit_code"], 0)
        self.assertFalse(result["build_complete"])
        self.assertIn("FileNotFoundError", result["first_failure"])

    def test_native_and_unknown_eof_not_pass_or_overwritten(self):
        self.exit_code = 77
        self.partial = True
        self.assertEqual(self.build(), 77)
        self.assertEqual(COL.finalize(self.repo, self.selected, self.paths()), 77)
        self.assertEqual(self.identity()["failure_exit"], 77)
        self.assertFalse(self.identity()["collection_complete"])

    def test_changed_build_map_source_and_raw_each_rejected(self):
        for changed in ("built", "map", "source", "raw"):
            with self.subTest(changed=changed):
                self.assertEqual(self.build(), 0)
                self.staged()
                paths = self.paths()
                path = paths["built"] if changed == "built" else paths["map"] if changed == "map" else self.repo / COL.CONFIGS[0] if changed == "source" else paths["evidence"] / "build.stderr"
                original = path.read_bytes()
                path.write_bytes(original + b"changed")
                self.assertNotEqual(COL.finalize(self.repo, self.selected, paths), 0)
                self.assertFalse(self.identity()["collection_complete"])
                path.write_bytes(original)
                shutil.rmtree(paths["evidence"])
                shutil.rmtree(paths["staged"].parent)

    def test_staged_change_refuses_snapshot(self):
        self.assertEqual(self.build(), 0)
        self.staged()
        self.paths()["staged"].write_bytes(self.paths()["staged"].read_bytes() + b"changed")
        self.assertNotEqual(COL.finalize(self.repo, self.selected, self.paths()), 0)
        self.assertFalse((self.paths()["evidence"] / COL.APPLICATION).exists())

    def test_old_package_without_identity_is_not_shipping_evidence(self):
        paths = self.paths()
        paths["evidence"].mkdir(parents=True)
        # 旧stage即使已有普通包文件，也没有同次map/identity，不能补判PASS。
        paths["staged"].parent.mkdir(parents=True)
        paths["staged"].write_bytes(b"old package fixture")
        with self.assertRaises(FileNotFoundError):
            COL.finalize(self.repo, self.selected, paths)
        self.assertFalse((paths["evidence"] / "identity.json").exists())

    def test_missing_staged_elf_stays_pending(self):
        self.assertEqual(self.build(), 0)
        self.assertNotEqual(COL.finalize(self.repo, self.selected, self.paths()), 0)
        self.assertFalse(self.identity()["staged_complete"])

    def test_relative_and_absolute_cargo_target_dir_match_stage(self):
        for text in ("custom output/cargo", str(self.repo.parent / "absolute output")):
            with self.subTest(text=text), mock.patch.dict(os.environ, CARGO_TARGET_DIR=text):
                paths = self.paths()
                expected = Path(text) if Path(text).is_absolute() else self.repo / text
                self.assertEqual(paths["cargo_root"], expected)
                self.assertEqual(self.build(), 0)
                self.staged()
                self.assertEqual(COL.finalize(self.repo, self.selected, paths), 0)
                shutil.rmtree(paths["evidence"])
        with mock.patch.dict(os.environ, CARGO_TARGET_DIR="ambiguous/../out"):
            with self.assertRaises(ValueError):
                self.paths()

    def test_target_dir_change_after_build_not_rebound(self):
        self.assertEqual(self.build(), 0)
        with mock.patch.dict(os.environ, CARGO_TARGET_DIR="different"):
            self.assertNotEqual(COL.finalize(self.repo, self.selected, self.paths()), 0)
        self.assertIn("CARGO_TARGET_DIR", self.identity()["first_failure"])

    def test_existing_evidence_not_overwritten(self):
        paths = self.paths()
        paths["evidence"].mkdir(parents=True)
        marker = paths["evidence"] / "marker"
        marker.write_bytes(b"preserve")
        with self.assertRaises(ValueError):
            self.build()
        self.assertEqual(marker.read_bytes(), b"preserve")
        self.assertEqual(self.calls, [])

    def test_illegal_evidence_parent_symlink_refused(self):
        elsewhere = self.repo / "elsewhere"
        elsewhere.mkdir()
        (self.repo / "target").symlink_to(elsewhere, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.build()
        self.assertEqual(list(elsewhere.iterdir()), [])

    def test_fresh_setup_failure_has_partial_identity_no_native0(self):
        (self.repo / COL.SOURCES[0][0]).unlink()
        self.assertEqual(self.build(), 1)
        self.assertIsNone(self.identity()["build"])
        self.assertFalse(self.identity()["collection_complete"])
        self.assertEqual(self.calls, [])

    def test_caps_and_changed_helper_never_execute_source(self):
        self.capture.stop()
        path = self.repo / COL.HELPER
        path.write_bytes(b"raise AssertionError('must never execute')")
        with mock.patch("builtins.compile") as compiler:
            with self.assertRaises(ValueError):
                COL.load_capture(self.repo)
            compiler.assert_not_called()
        with mock.patch.object(COL, "SOURCE_CAP", 1):
            self.assertEqual(self.build(), 1)
        self.assertEqual(self.calls, [])

    def test_bound_source_load_does_not_execute_helper_main_or_cache(self):
        self.capture.stop()
        capture = COL.load_capture(self.repo)
        self.assertEqual(capture.__globals__["__name__"], "frd_bound_shipping_capture")
        self.assertEqual(hashlib.sha256((self.repo / COL.HELPER).read_bytes()).hexdigest(), COL.HELPER_SHA)
        self.assertFalse(list(self.repo.rglob("*.pyc")))

    def test_cli_profile_platform_and_optin_closed(self):
        with mock.patch.object(COL.platform, "system", return_value="Linux"), mock.patch.object(COL, "build") as build:
            for args in (("build", "linux-aarch64", "release"), ("build", "linux-x86", "debug"), ("run", "linux-x86", "release")):
                with self.assertRaises(ValueError):
                    COL.run(*args)
            for name, value in (("GITHUB_ACTIONS", "false"), ("FRD_LINUX_PUBLICATION_SHIPPING", "0")):
                with mock.patch.dict(os.environ, {name: value}), self.assertRaises(ValueError):
                    COL.run("build", "linux-x86", "release")
            build.assert_not_called()

    def test_real_rustc_gtk_profile_and_target_checks(self):
        self.assertEqual(self.build(), 0)
        raw = (self.paths()["evidence"] / "build.stderr").read_bytes()
        for old, new in [(b'gtk-shell', b'winit-only'), (b'lto=thin', b'lto=fat'), (b'opt-level=3', b'opt-level=0'),
                         (b'x86_64-unknown-linux-gnu', b'aarch64-unknown-linux-gnu'), (b'--emit=dep-info,link', b'--test')]:
            with self.subTest(old=old), self.assertRaises(ValueError):
                COL.rustc_invocation(raw.replace(old, new, 1), self.selected, self.paths()["map"])


class ShellContract(unittest.TestCase):
    def test_opted_in_stage_records_same_link_map_and_final_elf_identity(self):
        source = (ROOT / "tools/stage-linux-package.sh").read_text()
        for mode in ("build", "finalize"):
            self.assertEqual(source.count('collect-linux-publication-shipping.py" ' + mode + ' "$platform" "$profile"'), 1)
        self.assertGreater(source.index('collect-linux-publication-shipping.py" finalize'), source.index('mv "$package" "$output"'))
        default = '  build_args=(build --locked -p freeremotedesk-linux --target "$rust_target")\n  [[ "$profile" != release ]] || build_args+=(--release)\n  cargo "${build_args[@]}"'
        self.assertIn(default, source)
        self.assertIn('debug/ARM64不可用', source)


if __name__ == "__main__":
    unittest.main()
