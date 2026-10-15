#!/usr/bin/env python3
"""窄 source/container/receipt gate 测试；mock 不是实际 Windows 编译证据。"""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "compile-windows-private-abi.py"
SPEC = importlib.util.spec_from_file_location("private_compile", SCRIPT)
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)

# 独立手写最小 COFF 容器，只用于 portable container gate，不声称有效代码/SEH。
COFF = bytes.fromhex(
    "64aa0100 00000000 00000000 00000000 0000 0000 "
    "2e74657874000000 00000000 00000000 04000000 3c000000 "
    "00000000 00000000 0000 0000 20000060 "
    "c0035fd6"
)


class SourceGateTests(unittest.TestCase):
    def test_reviewed_windows_trap_and_guide_pins_match_checkout(self):
        root = SCRIPT.parents[1]
        for name in ('h264-luma-dc-abi-windows.S', 'h264-luma-dc-abi-windows-negative.S', 'README-windows-abi.md'):
            self.assertEqual(GATE.digest(root / GATE.FIXTURE / name), GATE.SOURCE_PINS[name])

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="private-compile-source-gate-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def test_external_source_pin_rejects_mutation(self):
        name = "h264-luma-dc-clock-convert.h"
        source = self.root / GATE.FIXTURE / name
        source.parent.mkdir(parents=True)
        source.write_bytes(b"trusted reviewed fixture\n")
        pin = hashlib.sha256(b"trusted reviewed fixture\n").hexdigest()
        with mock.patch.object(GATE, "SOURCE_PINS", {name: pin}), mock.patch.object(GATE, "OWN_INPUTS", ()):
            self.assertEqual(GATE.source_snapshot(self.root)[GATE.FIXTURE + "/" + name]["sha256"], pin)
            source.write_bytes(b"mutated fixture\n")
            with self.assertRaises(GATE.GateError):
                GATE.source_snapshot(self.root)

    def test_exact_diagnostic_header_admitted_and_mutation_rejected(self):
        source_root=SCRIPT.parents[1]
        # 完整真实 source inventory，只有副本被突变；不调用任何 compiler。
        for name in [GATE.FIXTURE + '/' + n for n in GATE.SOURCE_PINS] + list(GATE.OWN_INPUTS):
            target=self.root/name; target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((source_root/name).read_bytes())
        name='h264-luma-dc-windows-diagnostics.h'
        header=self.root/GATE.FIXTURE/name
        header.write_bytes((source_root/GATE.FIXTURE/name).read_bytes())
        snapshot=GATE.source_snapshot(self.root)
        self.assertIn(GATE.FIXTURE+'/'+name, snapshot)
        header.write_bytes(header.read_bytes()+b'/* mutation */')
        with self.assertRaises(GATE.GateError): GATE.source_snapshot(self.root)

    def test_unapproved_header_and_directory_remain_refused(self):
        fixture=self.root/GATE.FIXTURE; fixture.mkdir(parents=True)
        for name, directory in [('unapproved.h', False), ('nested', True)]:
            path=fixture/name
            if directory: path.mkdir()
            else: path.write_bytes(b'unreviewed')
            with self.assertRaisesRegex(GATE.GateError, '未授权 header/子目录'):
                GATE.source_snapshot(self.root)
            if directory: path.rmdir()
            else: path.unlink()

    def test_symlink_source_cannot_supply_identical_bytes(self):
        name = "h264-luma-dc-clock-convert.h"
        source = self.root / GATE.FIXTURE / name
        source.parent.mkdir(parents=True)
        elsewhere = self.root / "elsewhere.h"
        elsewhere.write_bytes(b"same")
        source.symlink_to(elsewhere)
        with mock.patch.object(GATE, "SOURCE_PINS", {name: hashlib.sha256(b"same").hexdigest()}), mock.patch.object(GATE, "OWN_INPUTS", ()):
            with self.assertRaises(GATE.GateError):
                GATE.source_snapshot(self.root)

    def test_all_included_headers_have_explicit_reviewed_pins(self):
        self.assertEqual(len(GATE.SOURCE_PINS), 12)
        self.assertTrue({"h264-luma-dc-platform.h", "h264-luma-dc-clock-convert.h", "h264-luma-dc-windows-clock-record.h", "h264-luma-dc-windows-diagnostics.h"}.issubset(GATE.SOURCE_PINS))

    def test_shadow_windows_header_is_rejected_before_compile(self):
        fixture = self.root / GATE.FIXTURE
        fixture.mkdir(parents=True)
        (fixture / "windows.h").write_bytes(b"mock header must never enter actual compile")
        with self.assertRaises(GATE.GateError):
            GATE.source_snapshot(self.root)

    def test_cli_rejects_flags_compiler_alias_and_target_override(self):
        for extra in ("--cc=clang", "--flags=-DMOCK", "--target=x86_64", "--out=/tmp/alias", "--source-manifest=/tmp/self-written.json"):
            with self.subTest(extra=extra), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
                GATE.main(["probe", "--source-root", str(self.root), "--output", str(self.root / "target/task4c5c3b2c-unit"), extra])
            self.assertEqual(caught.exception.code, 2)

    def test_fixed_compile_vectors_keep_platform_real_headers(self):
        out = self.root / "target/task4c5c3b2c-unit"
        platform = GATE.compile_argv(self.root, out, "platform")
        self.assertEqual(platform[0], "/opt/frd-llvm-mingw-20250709/bin/aarch64-w64-mingw32-clang")
        self.assertIn("--target=aarch64-w64-mingw32", platform)
        self.assertIn("-std=c11", platform)
        self.assertIn("-Werror", platform)
        self.assertIn("-D_WIN32_WINNT=0x0A00", platform)
        self.assertFalse(any("CONTRACT" in arg or "mock" in arg.lower() for arg in platform))
        for n in range(1, 6):
            argv = GATE.compile_argv(self.root, out, "bad-unwind-%d" % n)
            self.assertIn("-DFRD_DC_WINDOWS_ABI_BAD_UNWIND=%d" % n, argv)
            self.assertIn("-DFRD_DC_WINDOWS_ABI_CONTRACT=1", argv)
        with self.assertRaises(GATE.GateError):
            GATE.compile_argv(self.root, out, "invented")

    def test_private_abi_compile_explicitly_specializes_C0_once(self):
        out=self.root/'target/task4c5c3b2c-unit'
        for case in ('abi','negative')+tuple('bad-unwind-%d'%n for n in range(1,6)):
            argv=GATE.compile_argv(self.root,out,case)
            self.assertEqual(argv.count('-DFRD_DC_WINDOWS_ABI_CONTRACT=1'),1)
            self.assertIn('--target=aarch64-w64-mingw32',argv)
            self.assertFalse(any('ARM64EC' in arg or 'authority' in arg or 'callback=' in arg for arg in argv))

    def test_wrong_checkout_and_existing_output_rejected(self):
        out = self.root / "target/task4c5c3b2c-unit"
        with self.assertRaises(GATE.GateError):
            GATE.paths(str(self.root), str(out))
        with mock.patch.object(GATE, "__file__", str(self.root / GATE.OWN_INPUTS[0])):
            GATE.paths(str(self.root), str(out))
            with self.assertRaises(FileExistsError):
                GATE.paths(str(self.root), str(out))
            with self.assertRaises(GATE.GateError):
                GATE.paths(str(self.root), str(self.root / "target/wrong-prefix"))


class ObjectGateTests(unittest.TestCase):
    def test_regular_arm64_container_and_adversarial_formats(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory).resolve() / "fixture.obj"
            path.write_bytes(COFF)
            self.assertEqual(GATE.coff_admit(path)["machine"], "0xAA64")
            for wrong in (b"\x64\x86" + COFF[2:], b"\x41\xa6" + COFF[2:],
                          b"MZ" + COFF[2:], b"\x7fELF" + COFF[4:], COFF[:19], COFF[:59], COFF[:-1]):
                with self.subTest(data=wrong[:4]):
                    path.write_bytes(wrong)
                    with self.assertRaises(GATE.GateError):
                        GATE.coff_admit(path)

    def test_readobj_requires_one_exact_file_and_arm64_machine(self):
        path = Path("/tmp/exact/abi.obj")
        positive = "File: /tmp/exact/abi.obj\nFormat: COFF-ARM64\n  Machine: IMAGE_FILE_MACHINE_ARM64 (0xAA64)\n"
        GATE.readobj_agrees(positive, path)
        for bad in (positive.replace("/tmp/exact", "/tmp/elsewhere"),
                    positive.replace("0xAA64", "0x8664"), positive.replace("COFF-ARM64", "COFF-ARM64EC"),
                    positive.replace("File: /tmp/exact/abi.obj\n", ""), positive + "File: /tmp/other.obj\n"):
            with self.subTest(text=bad), self.assertRaises(GATE.GateError):
                GATE.readobj_agrees(bad, path)

    def test_cross_macros_cannot_accept_host_or_ec(self):
        positive = "#define _WIN32 1\n#define __aarch64__ 1\n#define __SIZEOF_POINTER__ 8\n"
        GATE.macro_gate(positive)
        for bad in (positive.replace("_WIN32", "other"), positive.replace("__aarch64__", "__x86_64__"),
                    positive.replace("POINTER__ 8", "POINTER__ 4"), positive + "#define _M_ARM64EC 1\n"):
            with self.assertRaises(GATE.GateError):
                GATE.macro_gate(bad)

    def test_uninitialized_data_has_no_file_payload(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory).resolve() / "bss.obj"
            # 独立 .bss container，virtual allocation length 不当作 file bytes。
            data = bytearray(COFF[:60])
            data[20:28] = b".bss\0\0\0\0"
            data[36:40] = b"\x00\x10\x00\x00"
            data[40:44] = b"\x00\x00\x00\x00"
            data[56:60] = b"\x80\x00\x00\xc0"
            path.write_bytes(data)
            self.assertEqual(GATE.coff_admit(path)["machine"], "0xAA64")


class ReceiptGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="private-compile-mock-only-")
        self.addCleanup(self.temp.cleanup)
        self.out = Path(self.temp.name).resolve()
        self.argv = [str(GATE.CC), "--version"]
        self.expected = {"fixture": self.argv}

    def terminal(self, *, validator=lambda: None):
        return GATE.terminal(self.out, "fixture", self.argv, cwd=self.out, validator=validator, expected=self.expected)

    def test_extra_flags_and_different_executable_never_spawn(self):
        for argv in (self.argv + ["-DMOCK"], ["/usr/bin/clang", "--version"]):
            with mock.patch.object(GATE.subprocess, "Popen") as process, self.assertRaises(GATE.GateError):
                GATE.terminal(self.out, "fixture", argv, cwd=self.out, validator=lambda: None, expected=self.expected)
            process.assert_not_called()

    def test_not_started_error_has_saved_terminal(self):
        with mock.patch.object(GATE.subprocess, "Popen", side_effect=FileNotFoundError(2, "mock tool missing")):
            record = self.terminal()
        self.assertEqual(record["wait_terminal"], "not-started")
        self.assertIsNone(record["pid"])
        self.assertEqual(json.loads((self.out / "fixture.receipt.json").read_text()), record)
        with self.assertRaises(GATE.GateError):
            GATE.require_zero(record)

    def test_archive_does_not_authorize_unexpected_member(self):
        (self.out / "unexpected.bin").write_bytes(b"not an approved output")
        with self.assertRaises(GATE.GateError):
            GATE.archive_evidence(self.out)

    def test_zero_and_failing_mock_compiler_terminal_saved_before_check(self):
        for code in (0, 1):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                out = Path(directory).resolve()
                fake = mock.Mock(pid=123456, returncode=code)
                fake.communicate.return_value = (None, None)
                with mock.patch.object(GATE.subprocess, "Popen", return_value=fake):
                    record = GATE.terminal(out, "fixture", self.argv, cwd=out, validator=lambda: None, expected=self.expected)
                self.assertEqual(record["pid"], 123456)
                self.assertEqual(record["returncode"], code)
                self.assertEqual(record["wait_terminal"], "exited")
                self.assertTrue((out / "fixture.receipt.json").is_file())
                if code:
                    with self.assertRaises(GATE.GateError):
                        GATE.require_zero(record)
                else:
                    GATE.require_zero(record)

    def test_post_use_mutation_preserves_original_wait_terminal(self):
        calls = []

        def validator():
            calls.append(1)
            if len(calls) == 2:
                raise GATE.GateError("mock source mutation after use")

        fake = mock.Mock(pid=123456, returncode=1)
        with mock.patch.object(GATE.subprocess, "Popen", return_value=fake), self.assertRaises(GATE.GateError):
            self.terminal(validator=validator)
        self.assertEqual(json.loads((self.out / "fixture.receipt.json").read_text())["returncode"], 1)

    def test_rejected_candidate_is_not_native_or_broad_error_pass(self):
        specific = "error: Incorrect size for frd_h264_dc_check_abi epilogue: 40 bytes of instructions in range, but .seh directives corresponding to 44 bytes\n"
        for state, code, text, expected in (("exited", 0, "", "compiled"),
                                         ("exited", 1, specific, "rejected"),
                                         ("exited", 1, "<unknown>:0: " + specific, "rejected"),
                                         ("exited", 1, "error: mock assembler rejection\n", "error"),
                                         ("exited", 1, "unknown failure", "error"),
                                         ("exited", 1, "error: failed to write output file\n", "error"),
                                         ("exited", 1, "clang: error: unable to execute command: mock child missing\n", "error"),
                                         ("exited", 1, "clang: error: clang integrated assembler command failed with exit code 1\n", "error"),
                                         ("exited", 1, "fatal error: mock internal compiler failure\n", "error"),
                                         ("exited", 1, "error: out of memory\n", "error"),
                                         ("exited", 2, specific, "error"),
                                         ("exited", 1, specific + "error: failed to write output file\n", "error"),
                                         ("exited", 1, specific.replace("40 bytes", "44 bytes"), "error"),
                                         ("exited", 1, specific.replace("check_abi", "other_function"), "error"),
                                         ("signaled", -11, "error: crash", "error"),
                                         ("timeout-killed", -9, "error: timeout", "error")):
            self.assertEqual(GATE.candidate_outcome({"wait_terminal": state, "returncode": code, "failure": None}, text, "bad-unwind-5"), expected)
        self.assertEqual(GATE.candidate_outcome({"wait_terminal": "exited", "returncode": 1}, specific, "bad-unwind-1"), "error")
        self.assertEqual(GATE.candidate_outcome({"wait_terminal": "exited", "returncode": 1, "failure": {"errno": 5}}, specific, "bad-unwind-5"), "error")

    def test_mandatory_failure_summary_cannot_remain_compiled_only(self):
        def broken_probe(root, out, initial, installation, summary):
            summary["status"] = "compiled-only"
            summary["objects"].append({"case": "platform", "mandatory": True, "outcome": "error"})
            raise GATE.GateError("mock mandatory compile failure")

        install = self.out / "target/task4c5c3b2c-install"
        install.mkdir(parents=True)
        with mock.patch.object(GATE, "paths", return_value=(self.out, self.out)), mock.patch.object(GATE, "source_snapshot", return_value={}), mock.patch.object(GATE, "probe", side_effect=broken_probe), contextlib.redirect_stdout(io.StringIO()):
            status = GATE.main(["probe", "--source-root", str(self.out), "--output", str(self.out), "--installation", str(install)])
        self.assertEqual(status, 1)
        self.assertEqual(json.loads((self.out / "summary.json").read_text())["status"], "failed")
        self.assertTrue((self.out / "evidence.zip").is_file())

    def test_real_probe_orchestration_retains_candidate_io_fault_and_fails(self):
        """只 mock 外部工具/安装输入，真实 probe 分支、summary 和 container gate 不替换。"""
        installation = self.out / "target/task4c5c3b2c-unit-install"
        output = self.out / "target/task4c5c3b2c-unit-compile"
        installation.mkdir(parents=True)
        output.mkdir()
        GATE.json_write(installation / "extracted-inventory.json", {})
        GATE.json_write(installation / "installation.json", {
            "schema": GATE.SCHEMA + ".installation", "archive_url": GATE.ARCHIVE_URL,
            "archive_sha256": GATE.ARCHIVE_SHA, "tool_root": str(GATE.TOOL_ROOT),
            "inventory_sha256": GATE.digest(installation / "extracted-inventory.json"),
            "scope": "unit mock only; not a trusted actual Windows compiler",
        })
        (installation / GATE.ARCHIVE_NAME).write_bytes(b"unit mock archive only")
        summary = {"status": "failed", "objects": [], "failures": [], "not_attempted": list(GATE.CASES)}
        original_digest = GATE.digest
        observed = []

        def fixture_digest(path):
            if path == installation / GATE.ARCHIVE_NAME:
                return GATE.ARCHIVE_SHA  # 明确模拟 trust边界，测试不是 archive/工具认证。
            if path in (GATE.CC, GATE.READOBJ, GATE.OBJDUMP):
                return "0" * 64
            return original_digest(path)

        def fixture_terminal(out, label, argv, *, cwd, validator, expected, stdin=None):
            self.assertEqual(argv, expected[label])
            validator()
            observed.append(label)
            code, stdout, stderr = 0, "unit mock external tool\n", ""
            if label == "macros":
                stdout = "#define _WIN32 1\n#define __aarch64__ 1\n#define __SIZEOF_POINTER__ 8\n"
            elif label.endswith("-compile"):
                case = label.removesuffix("-compile")
                if case == "bad-unwind-1":
                    code, stderr = 1, "error: failed to write output file\n"
                elif case == "bad-unwind-5":
                    code, stderr = 1, "error: Incorrect size for frd_h264_dc_check_abi epilogue: 40 bytes of instructions in range, but .seh directives corresponding to 44 bytes\n"
                else:
                    (out / (case + ".obj")).write_bytes(COFF)
            elif label.endswith("-readobj"):
                stdout = "File: %s\nFormat: COFF-ARM64\n  Machine: IMAGE_FILE_MACHINE_ARM64 (0xAA64)\n" % argv[-1]
            (out / (label + ".stdout")).write_text(stdout)
            (out / (label + ".stderr")).write_text(stderr)
            record = {"argv": argv, "pid": 123456, "wait_terminal": "exited", "returncode": code, "failure": None,
                      "scope": "unit mock external tool only"}
            GATE.json_write(out / (label + ".receipt.json"), record)
            validator()
            return record

        with mock.patch.object(GATE, "host_gate"), mock.patch.object(GATE, "source_snapshot", return_value={}), \
                mock.patch.object(GATE, "archive_inventory", return_value={}), mock.patch.object(GATE, "tool_snapshot", return_value={}), \
                mock.patch.object(GATE, "digest", side_effect=fixture_digest), mock.patch.object(GATE, "terminal", side_effect=fixture_terminal), \
                self.assertRaises(GATE.GateError):
            GATE.probe(self.out, output, {}, installation, summary)
        self.assertEqual(summary["status"], "failed")
        self.assertEqual(len(summary["objects"]), 8)
        self.assertEqual(summary["not_attempted"], [])
        self.assertEqual([item["outcome"] for item in summary["objects"][:3]], ["compiled"] * 3)
        self.assertTrue(all(item["mandatory"] and not item["failure"] for item in summary["objects"][:3]))
        bad = next(item for item in summary["objects"] if item["case"] == "bad-unwind-1")
        self.assertEqual(bad["outcome"], "error")
        self.assertIsNotNone(bad["failure"])
        self.assertEqual(summary["failures"][0]["case"], "bad-unwind-1")
        self.assertEqual(summary["first_compiler_failure"]["case"], "bad-unwind-1")
        self.assertEqual((output / "bad-unwind-1-compile.stderr").read_text(), "error: failed to write output file\n")
        self.assertEqual(json.loads((output / "bad-unwind-1-compile.receipt.json").read_text())["returncode"], 1)
        self.assertIn("bad-unwind-5-compile", observed)
        self.assertEqual(summary["objects"][-1]["outcome"], "rejected")


if __name__ == "__main__":
    unittest.main()
