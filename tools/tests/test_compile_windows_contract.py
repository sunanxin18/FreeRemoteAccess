#!/usr/bin/env python3
"""窄 parser/provenance/orchestration 控制；手写容器和 mock 从不证明 native Windows。"""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import struct
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / "compile-windows-contract.py"
SPEC = importlib.util.spec_from_file_location("contract_compile", SCRIPT)
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)

# 一条 ret 的 container fixture；只验证 parser，不是可运行 ABI/SEH fixture。
COFF = bytes.fromhex(
    "64aa0100 00000000 00000000 00000000 0000 0000 "
    "2e74657874000000 00000000 00000000 04000000 3c000000 "
    "00000000 00000000 0000 0000 20000060 c0035fd6"
)


CONTRACT_APIS = ("RtlCaptureContext", "RtlLookupFunctionEntry", "RtlVirtualUnwind")
DEFAULT_IMPORTS = (("KERNEL32.dll", CONTRACT_APIS), ("msvcrt.dll", ("__C_specific_handler",)))


def container_pe(imports=DEFAULT_IMPORTS):
    """独立手写 PE32+ 数据表，没有程序/native 成功语义。"""
    data = bytearray(0xC00)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x84, 0xAA64, 1, 0, 0, 0, 240, 2)
    opt = 0x98
    struct.pack_into("<H", data, opt, 0x20B)
    struct.pack_into("<Q", data, opt + 24, 0x140000000)
    struct.pack_into("<II", data, opt + 56, 0x2000, 0x200)
    struct.pack_into("<H", data, opt + 68, 3)
    struct.pack_into("<I", data, opt + 108, 16)
    struct.pack_into("<II", data, opt + 112 + 8, 0x1200, (len(imports) + 1) * 20)
    struct.pack_into("<II", data, opt + 112 + 24, 0x1000, 8)
    struct.pack_into("<8sIIIIIIHHI", data, 0x188, b".text\0\0\0", 0xA00, 0x1000, 0xA00, 0x200, 0, 0, 0, 0, 0x60000020)
    struct.pack_into("<II", data, 0x200, 0x1100, 0x1120)
    struct.pack_into("<II", data, 0x320, 4 | (1 << 27), 0xE4)
    hint_name = 0x1500
    for index, (dll, symbols) in enumerate(imports):
        dll = dll.encode("ascii") + b"\0"
        name = 0x1280 + index * 32
        thunk = 0x1300 + index * 64
        struct.pack_into("<IIIII", data, 0x400 + index * 20, thunk, 0, 0, name, thunk)
        data[name - 0xE00:name - 0xE00 + len(dll)] = dll
        for position, symbol in enumerate(symbols):
            if isinstance(symbol, int):
                entry = (1 << 63) | symbol
            else:
                encoded = symbol.encode("ascii") + b"\0"
                entry = hint_name
                struct.pack_into("<H", data, hint_name - 0xE00, 0)
                data[hint_name - 0xE00 + 2:hint_name - 0xE00 + 2 + len(encoded)] = encoded
                hint_name += 2 + len(encoded)
            struct.pack_into("<Q", data, thunk - 0xE00 + position * 8, entry)
        struct.pack_into("<Q", data, thunk - 0xE00 + len(symbols) * 8, 0)
    return data


class TempTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="contract-container-only-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()


class SourceAndPathTests(TempTests):
    def test_driver_pin_drift_and_symlink_rejected(self):
        source = self.root / GATE.FIXTURE / "driver.c"
        source.parent.mkdir(parents=True)
        source.write_bytes(b"reviewed")
        support_path = self.root / GATE.SUPPORT
        support_path.parent.mkdir(parents=True)
        support_path.write_bytes(b"support")
        support = SimpleNamespace(source_snapshot=lambda root: {})
        with mock.patch.object(GATE, "PINS", {"driver.c": GATE.sha(source)}), mock.patch.object(GATE, "OWN", ()), mock.patch.object(GATE, "SUPPORT_SHA", GATE.sha(support_path)):
            GATE.source_snapshot(self.root, support)
            source.write_bytes(b"changed")
            with self.assertRaises(GATE.GateError):
                GATE.source_snapshot(self.root, support)
            elsewhere = self.root / "elsewhere"
            elsewhere.write_bytes(b"reviewed")
            source.unlink()
            source.symlink_to(elsewhere)
            with self.assertRaises(GATE.GateError):
                GATE.source_snapshot(self.root, support)

    def test_support_drift_fails_before_import(self):
        path = self.root / GATE.SUPPORT
        path.parent.mkdir(parents=True)
        path.write_bytes(b"unreviewed")
        with mock.patch.object(GATE.importlib.util, "spec_from_file_location") as loader:
            with self.assertRaises(GATE.GateError):
                GATE.pinned_support(self.root)
            loader.assert_not_called()

    def test_fixed_same_run_relationships_no_alias_or_reuse(self):
        source = self.root / "target/task4c5c3b2c-compile-11-2"
        install = self.root / "target/task4c5c3b2c-install-11-2"
        out = self.root / "target/task4c5c3b2e-contract-11-2"
        source.mkdir(parents=True)
        install.mkdir()
        with mock.patch.dict(os.environ, {"GITHUB_RUN_ID": "11", "GITHUB_RUN_ATTEMPT": "2"}), mock.patch.object(GATE, "__file__", str(self.root / GATE.OWN[0])):
            for bad_input, bad_install, bad_out in ((source, source, out), (install, install, out),
                                                   (source, install, source), (source, install, self.root / "target/task4c5c3b2e-contract-11-1"),
                                                   (source, install, self.root / "target/../task4c5c3b2e-contract-11-2")):
                with self.assertRaises(GATE.GateError):
                    GATE.paths(str(self.root), str(bad_input), str(bad_install), str(bad_out))
            GATE.paths(str(self.root), str(source), str(install), str(out))
            with self.assertRaises(FileExistsError):
                GATE.paths(str(self.root), str(source), str(install), str(out))
            out.rmdir()
            out.symlink_to(source, target_is_directory=True)
            with self.assertRaises(FileExistsError):
                GATE.paths(str(self.root), str(source), str(install), str(out))

    def test_cli_has_no_compiler_flags_source_manifest_or_mode_override(self):
        required = ["--source-root", str(self.root), "--compile-input", str(self.root),
                    "--installation", str(self.root), "--output", str(self.root)]
        for extra in ("--compiler=clang", "--flags=-Wno-error", "--target=x86_64", "--source-manifest=/tmp/untrusted", "--out=/tmp/alias"):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                GATE.main(required + [extra])
            self.assertEqual(error.exception.code, 2)

    def test_five_vectors_have_matching_objects_and_no_bad5(self):
        support = SimpleNamespace(CC=Path("/opt/tool/clang"), READOBJ=Path("/opt/tool/readobj"), TARGET="aarch64-w64-mingw32")
        commands = GATE.commands(self.root, self.root / "input", self.root / "output", support)
        for variant in range(5):
            compile_argv = commands["variant-%d-compile" % variant]
            link = commands["variant-%d-link" % variant]
            self.assertIn("-DFRD_CONTRACT_VARIANT=%d" % variant, compile_argv)
            for flag in ("-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-fno-omit-frame-pointer", "-fno-optimize-sibling-calls", "-DWINVER=0x0A00", "-D_WIN32_WINNT=0x0A00"):
                self.assertIn(flag, compile_argv)
            helper = "abi.obj" if variant == 0 else "bad-unwind-%d.obj" % variant
            self.assertEqual(link[2:5], [str(self.root / "output" / ("driver-%d.obj" % variant)), str(self.root / "input" / helper), str(self.root / "input/negative.obj")])
        self.assertFalse(any("bad-unwind-5" in argument for argv in commands.values() for argument in argv))


class BinaryAndRecordTests(TempTests):
    def test_same_run_success_is_not_old_run_or_target_authority(self):
        support = SimpleNamespace(SCHEMA="old.schema", TARGET="aarch64-w64-mingw32")
        keys = ("GITHUB_SHA", "GITHUB_REF", "GITHUB_EVENT_NAME", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "RUNNER_OS", "RUNNER_ARCH")
        environ = {key: "fixed-" + key for key in keys}
        value = {"schema": support.SCHEMA, "mode": "probe", "status": "compiled-only", "source_root": str(self.root),
                 "build_target": support.TARGET, "failures": [], "host": {"system": "Linux", "machine": "x86_64"}, "github": environ}
        with mock.patch.dict(os.environ, environ):
            GATE.same_run(value, "probe", "compiled-only", self.root, support)
            for field, replacement in (("status", "failed"), ("build_target", "x86_64"), ("source_root", "/other"), ("failures", [{"failure": "real"}])):
                changed = {**value, field: replacement}
                with self.assertRaises(GATE.GateError):
                    GATE.same_run(changed, "probe", "compiled-only", self.root, support)
            changed = {**value, "github": {**environ, "GITHUB_RUN_ID": "old-run"}}
            with self.assertRaises(GATE.GateError):
                GATE.same_run(changed, "probe", "compiled-only", self.root, support)

    def test_pe_container_only_and_wrong_machine_optional_native_image(self):
        path = self.root / "container.exe"
        baseline = container_pe()
        path.write_bytes(baseline)
        parsed = GATE.pe_admit(path)
        self.assertEqual(parsed["machine"], "0xAA64")
        self.assertEqual(parsed["pdata"][0]["length"], 16)
        self.assertEqual({entry["dll"] for entry in parsed["imports"]}, {"KERNEL32.dll", "msvcrt.dll"})
        self.assertEqual(parsed["imports"][0]["symbols"], [{"name": name} for name in CONTRACT_APIS])
        for offset, fmt, value in ((0x84, "<H", 0x8664), (0x84, "<H", 0xA641),
                                    (0x98, "<H", 0x10B), (0x98 + 68, "<H", 1),
                                    (0x84 + 18, "<H", 0x2002), (0x98 + 108, "<I", 15)):
            data = bytearray(baseline)
            struct.pack_into(fmt, data, offset, value)
            path.write_bytes(data)
            with self.assertRaises(GATE.GateError):
                GATE.pe_admit(path)
        for truncated in (baseline[:63], baseline[:0x97], baseline[:0x500]):
            path.write_bytes(truncated)
            with self.assertRaises(GATE.GateError):
                GATE.pe_admit(path)

    def test_chpe_clr_and_changed_import_closure_rejected(self):
        path = self.root / "container.exe"
        for kind in ("chpe", "clr", "crt"):
            data = container_pe()
            if kind == "chpe":
                struct.pack_into("<II", data, 0x98 + 112 + 80, 0x1400, 208)
                struct.pack_into("<I", data, 0x600, 208)
                struct.pack_into("<Q", data, 0x600 + 200, 1)
            elif kind == "clr":
                struct.pack_into("<II", data, 0x98 + 112 + 112, 0x1400, 72)
            else:
                data = container_pe((DEFAULT_IMPORTS[0], ("other.dll", DEFAULT_IMPORTS[1][1])))
            path.write_bytes(data)
            with self.assertRaises(GATE.GateError):
                GATE.pe_admit(path)

    def test_exact_kernel32_named_apis_and_case_insensitive_dlls(self):
        path = self.root / "container.exe"
        for kernel, crt in (("KERNEL32.dll", "msvcrt.dll"), ("KeRnEl32.DlL", "MSVCRT.DLL")):
            with self.subTest(kernel=kernel, crt=crt):
                path.write_bytes(container_pe(((kernel, CONTRACT_APIS), (crt, ("__C_specific_handler",)))))
                parsed = GATE.pe_admit(path)
                self.assertEqual(parsed["imports"][0]["symbols"], [{"name": name} for name in CONTRACT_APIS])

    def test_each_required_api_missing_changed_spelling_or_ordinal_rejected(self):
        path = self.root / "container.exe"
        for index, api in enumerate(CONTRACT_APIS):
            for kind in ("missing", "spelling", "ordinal"):
                symbols = list(CONTRACT_APIS)
                if kind == "missing":
                    del symbols[index]
                else:
                    symbols[index] = api.lower() if kind == "spelling" else 1
                with self.subTest(api=api, kind=kind):
                    path.write_bytes(container_pe((("KERNEL32.dll", symbols), DEFAULT_IMPORTS[1])))
                    with self.assertRaisesRegex(GATE.GateError, "KERNEL32 缺少精确命名"):
                        GATE.pe_admit(path)

    def test_each_required_api_from_wrong_provider_rejected_even_with_correct_duplicate(self):
        path = self.root / "container.exe"
        for api in CONTRACT_APIS:
            for duplicate in (False, True):
                symbols = CONTRACT_APIS if duplicate else tuple(name for name in CONTRACT_APIS if name != api)
                with self.subTest(api=api, duplicate=duplicate):
                    path.write_bytes(container_pe((("KERNEL32.dll", symbols), ("msvcrt.dll", ("__C_specific_handler", api)))))
                    error = "Rtl API 必须由 KERNEL32 提供" if duplicate else "KERNEL32 缺少精确命名"
                    with self.assertRaisesRegex(GATE.GateError, error):
                        GATE.pe_admit(path)

    def test_unexpected_missing_or_duplicate_dll_closure_rejected(self):
        path = self.root / "container.exe"
        for imports in (DEFAULT_IMPORTS + (("NTDLL.dll", CONTRACT_APIS),),
                        DEFAULT_IMPORTS + (("other.dll", ("OtherFunction",)),),
                        (DEFAULT_IMPORTS[0],), (DEFAULT_IMPORTS[1],),
                        (("other.dll", CONTRACT_APIS), DEFAULT_IMPORTS[1]),
                        DEFAULT_IMPORTS + (("kernel32.DLL", ("OtherFunction",)),)):
            with self.subTest(dlls=[item[0] for item in imports]):
                path.write_bytes(container_pe(imports))
                with self.assertRaisesRegex(GATE.GateError, "actual import closure 必须恰为"):
                    GATE.pe_admit(path)

    def test_actual_input_missing_mutated_or_malformed_not_receipt_success(self):
        # trusted support COFF parser，手写容器只测试 bytes gate。
        support = GATE.pinned_support(SCRIPT.parents[1])
        path = self.root / "abi.obj"
        path.write_bytes(COFF)
        item = {"outcome": "compiled", "object": {"path": str(path), "sha256": GATE.sha(path), "size": len(COFF)}}
        GATE.actual_object(self.root, "abi", item, support)
        path.write_bytes(COFF[:-4] + b"xxxx")
        with self.assertRaises(GATE.GateError):
            GATE.actual_object(self.root, "abi", item, support)
        path.write_bytes(b"MZ" + COFF[2:])
        with self.assertRaises(support.GateError):
            GATE.actual_object(self.root, "abi", item, support)
        path.unlink()
        with self.assertRaises(OSError):
            GATE.actual_object(self.root, "abi", item, support)

    def test_receipt_argv_is_only_data(self):
        support = SimpleNamespace(TOOL_ROOT=Path("/opt/fixed"))
        argv = ["/opt/fixed/bin/clang", "--version"]
        for stream in ("stdout", "stderr"):
            (self.root / ("probe." + stream)).write_bytes(b"")
        value = {"argv": ["/bin/sh", "-c", "untrusted"], "cwd": str(self.root),
                 "environment": {"PATH": "/opt/fixed/bin:/usr/bin:/bin", "LC_ALL": "C", "LANG": "C", "HOME": "/tmp"},
                 "pid": 123, "elapsed_ns": 1, "wait_terminal": "exited", "returncode": 0,
                 "stdout": {"name": "probe.stdout", "sha256": GATE.sha(self.root / "probe.stdout")},
                 "stderr": {"name": "probe.stderr", "sha256": GATE.sha(self.root / "probe.stderr")}}
        GATE.write_json(self.root / "probe.receipt.json", value)
        with self.assertRaises(GATE.GateError):
            GATE.receipt(self.root, "probe", argv, self.root, support)
        value["argv"] = argv
        (self.root / "probe.receipt.json").unlink()
        GATE.write_json(self.root / "probe.receipt.json", value)
        self.assertEqual(GATE.receipt(self.root, "probe", argv, self.root, support)["argv"], argv)


class ArchiveTests(TempTests):
    def test_zip_strict_members_extra_tree_duplicate_and_hash_change(self):
        for variant in ("positive", "extra", "duplicate", "changed", "symlink"):
            folder = self.root / variant
            folder.mkdir()
            (folder / "raw.txt").write_bytes(b"actual raw bytes")
            allowed = {"raw.txt"}
            evidence = GATE.Evidence(folder, allowed)
            evidence.register(folder / "raw.txt")
            GATE.artifact(folder, allowed, evidence)
            if variant == "positive":
                GATE.verify_bundle(folder, {"raw.txt"}, GATE.SCHEMA)
                continue
            if variant == "extra":
                (folder / "unknown.txt").write_bytes(b"untrusted")
            elif variant == "changed":
                (folder / "raw.txt").write_bytes(b"changed")
            elif variant == "symlink":
                (folder / "raw.txt").unlink()
                (folder / "raw.txt").symlink_to(self.root / "positive/raw.txt")
            else:
                with self.assertWarns(UserWarning):
                    with zipfile.ZipFile(folder / "evidence.zip", "a") as archive:
                        archive.writestr("raw.txt", b"actual raw bytes")
                record = GATE.load_json(folder / "archive.json")
                record["sha256"] = GATE.sha(folder / "evidence.zip")
                (folder / "archive.json").unlink()
                GATE.write_json(folder / "archive.json", record)
            with self.assertRaises(GATE.GateError):
                GATE.verify_bundle(folder, {"raw.txt"}, GATE.SCHEMA)

    def test_tar_rejects_alias_traversal_duplicate_hardlink_and_special(self):
        for index, names in enumerate((("fixed", "fixed/ok"), ("fixed", "fixed/../out"),
                                      ("fixed", "fixed/./ok"), ("fixed", "fixed//ok"),
                                      ("fixed", "fixed/ok", "fixed/ok"), ("other",),
                                      ("fixed", "fixed/hardlink"), ("fixed", "fixed/fifo"),
                                      ("fixed", "fixed/absolute-symlink"), ("fixed", "fixed/escape-symlink"))):
            archive_path = self.root / ("archive-%d.tar.xz" % index)
            with tarfile.open(archive_path, "w:xz") as archive:
                for name in names:
                    member = tarfile.TarInfo(name)
                    if name in ("fixed", "other"):
                        member.type = tarfile.DIRTYPE
                    elif name.endswith("hardlink"):
                        member.type, member.linkname = tarfile.LNKTYPE, "fixed/ok"
                    elif name.endswith("fifo"):
                        member.type = tarfile.FIFOTYPE
                    elif name.endswith("absolute-symlink"):
                        member.type, member.linkname = tarfile.SYMTYPE, "/outside"
                    elif name.endswith("escape-symlink"):
                        member.type, member.linkname = tarfile.SYMTYPE, "../../outside"
                    archive.addfile(member)
            support = SimpleNamespace(ARCHIVE_NAME="fixed.tar.xz", ARCHIVE_SHA=GATE.sha(archive_path))
            if index == 0:
                self.assertEqual(GATE.archive_inventory(archive_path, support)["ok"]["kind"], "file")
            else:
                with self.assertRaises(GATE.GateError):
                    GATE.archive_inventory(archive_path, support)


class StopOnFailureTests(TempTests):
    def model(self, failure):
        source, install, out = (self.root / "source", self.root / "install", self.root / "out")
        for folder in (source, install, out):
            folder.mkdir()
        calls = []
        support = SimpleNamespace(CC=Path("/opt/tool/clang"), READOBJ=Path("/opt/tool/readobj"),
                                  TOOL_ROOT=Path("/opt/tool"),
                                  TARGET="aarch64-w64-mingw32", host_gate=lambda: None,
                                  coff_admit=lambda path: {"machine": "0xAA64"},
                                  readobj_agrees=lambda text, path: None)
        def require_zero(record):
            if record["wait_terminal"] != "exited" or record["returncode"] != 0:
                raise GATE.GateError("modeled command failed")
        support.require_zero = require_zero
        def terminal(folder, label, argv, **kwargs):
            # orchestration 故障模型只保存事实，不执行工具或 native。
            kwargs["validator"]()
            calls.append(label)
            (folder / (label + ".stdout")).write_bytes(b"")
            (folder / (label + ".stderr")).write_bytes(b"modeled failure diagnostic")
            record = {"argv": argv, "wait_terminal": "exited", "returncode": 0, "pid": 1, "failure": None}
            for stream in ("stdout", "stderr"):
                record[stream] = {"sha256": GATE.sha(folder / (label + "." + stream))}
            if label.endswith("-compile"):
                if failure != "missing-object":
                    (folder / "driver-0.obj").write_bytes(COFF)
                if failure == "compile-failure":
                    record["returncode"] = 1
            if label.endswith("-link") and failure == "link-failure":
                (folder / "abi-contract-0.exe").write_bytes(b"partial failed linker bytes")
                (folder / "abi-contract-0.map").write_bytes(b"partial map")
                record["returncode"] = 1
            GATE.write_json(folder / (label + ".receipt.json"), record)
            if failure == "registered-json-drift" and label == "variant-0-compile-expansion":
                (folder / "authorization.json").write_bytes(b'{"altered":"between commands"}\n')
            kwargs["validator"]()
            return record
        support.terminal = terminal
        summary = {"variants": [], "not_attempted": list(range(5)), "status": "failed"}
        allowed = set(GATE.FIXED_MEMBERS)
        evidence = GATE.Evidence(out, allowed)
        with mock.patch.object(GATE, "source_snapshot", return_value={}), mock.patch.object(GATE, "admit_inputs", return_value=({}, {}, {}, {})), mock.patch.object(GATE, "tools_match", return_value={}), mock.patch.object(GATE, "CRT", ()):
            with self.assertRaises((GATE.GateError, OSError)):
                GATE.build(self.root, source, install, out, support, summary, allowed, evidence)
        self.assertEqual(summary["not_attempted"], [1, 2, 3, 4])
        self.assertTrue(all(label.startswith("variant-0-") for label in calls))
        self.assertEqual(len(calls), len(set(calls)))
        self.assertTrue((out / (calls[-1] + ".receipt.json")).is_file())
        self.assertEqual((out / (calls[-1] + ".stderr")).read_bytes(), b"modeled failure diagnostic")
        self.assertNotEqual(summary["status"], "compiled-only")
        return calls, out, summary

    def test_compile_failure_preserves_partial_object_and_no_next_variant(self):
        calls, out, summary = self.model("compile-failure")
        self.assertEqual(calls, ["variant-0-compile-expansion", "variant-0-compile"])
        self.assertEqual((out / "driver-0.obj").read_bytes(), COFF)
        self.assertEqual(summary["first_command_failure"]["label"], "variant-0-compile")

    def test_link_failure_preserves_partial_pe_map_and_no_next_variant(self):
        calls, out, summary = self.model("link-failure")
        self.assertEqual(calls[-1], "variant-0-link")
        self.assertEqual((out / "abi-contract-0.exe").read_bytes(), b"partial failed linker bytes")
        self.assertEqual((out / "abi-contract-0.map").read_bytes(), b"partial map")
        self.assertEqual(summary["first_command_failure"]["label"], "variant-0-link")

    def test_zero_compile_but_missing_actual_output_stops(self):
        calls, out, summary = self.model("missing-object")
        self.assertEqual(calls[-1], "variant-0-compile")
        self.assertFalse((out / "driver-0.obj").exists())

    def test_zero_link_but_missing_actual_pe_stops(self):
        calls, out, summary = self.model("missing-pe")
        self.assertEqual(calls[-1], "variant-0-link")
        self.assertFalse((out / "abi-contract-0.exe").exists())

    def test_registered_json_drift_in_post_validator_stops_before_compile_or_next_variant(self):
        calls, out, summary = self.model("registered-json-drift")
        self.assertEqual(calls, ["variant-0-compile-expansion"])
        self.assertFalse((out / "driver-0.obj").exists())


class EvidenceFinalizationFailureTests(TempTests):
    def setup_evidence(self):
        out = self.root / "evidence"
        out.mkdir()
        allowed = set(GATE.FIXED_MEMBERS)
        evidence = GATE.Evidence(out, allowed)
        # 只模拟进入封装前的 orchestration 状态；不是 compiler/Windows 成功证据。
        summary = {"schema": GATE.SCHEMA, "status": "compiled-only", "failures": [],
                   "scope": "fault-injected packaging state only; native never evaluated"}
        return out, allowed, evidence, summary

    def assert_failed(self, out, result):
        self.assertEqual(result, 1)
        summary = GATE.load_json(out / "summary.json")
        terminal = GATE.load_json(out / "terminal-status.json")
        failure = GATE.load_json(out / "archive-failure.json")
        self.assertEqual(summary["status"], "failed")
        self.assertTrue(summary["failures"])
        self.assertEqual(terminal["status"], "failed")
        self.assertFalse(terminal["archive_verified"])
        self.assertEqual(terminal["archive_failure"]["phase"], "archive-finalization")
        self.assertEqual(failure["first_failure"], summary["first_failure"])
        self.assertTrue(failure["partial_archive_is_not_accepted"])

    def test_registered_source_copy_uses_original_pin_and_drift_is_not_refreshed(self):
        out, allowed, evidence, summary = self.setup_evidence()
        path = out / "inputs/reviewed.c"
        path.parent.mkdir()
        original = b"reviewed source bytes"
        path.write_bytes(original)
        pin = hashlib.sha256(original).hexdigest()
        evidence.register(path, pin=pin)
        path.write_bytes(b"unapproved changed source")
        with self.assertRaises(GATE.GateError):
            evidence.check()
        with self.assertRaises(GATE.GateError):
            evidence.register(path)  # 不允许重新 hash 接纳漂移。
        self.assertEqual(evidence.expected[path]["sha256"], pin)
        with contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        self.assert_failed(out, result)
        self.assertFalse((out / "evidence.zip").exists())

    def test_registered_bindings_and_dynamic_argv_json_drift_is_rejected(self):
        for name in ("variant-0-pe-bindings.json", "variant-0-disassemble-main.argv.json"):
            folder = self.root / name.removesuffix(".json")
            folder.mkdir()
            allowed = set(GATE.FIXED_MEMBERS)
            evidence = GATE.Evidence(folder, allowed)
            evidence.write(name, {"generated": "frozen real-material identity"})
            pin = evidence.expected[folder / name]["sha256"]
            (folder / name).write_bytes(b'{"generated":"tampered"}\n')
            with self.assertRaises(GATE.GateError):
                evidence.check()
            with self.assertRaises(GATE.GateError):
                GATE.artifact(folder, allowed, evidence)
            self.assertEqual(evidence.expected[folder / name]["sha256"], pin)
            self.assertFalse((folder / "evidence.zip").exists())

    def test_final_actual_member_mutated_after_bundle_verification_is_not_accepted(self):
        out, allowed, evidence, summary = self.setup_evidence()
        path = out / "driver-0.obj"
        path.write_bytes(b"opaque actual-member bytes for identity-only failure control")
        evidence.register(path)
        pin = evidence.expected[path]["sha256"]
        original_verify = GATE.verify_bundle
        def verify_then_mutate(*args, **kwargs):
            value = original_verify(*args, **kwargs)
            path.write_bytes(b"changed after ZIP/current-file consistency check")
            return value
        with mock.patch.object(GATE, "verify_bundle", side_effect=verify_then_mutate), contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        self.assert_failed(out, result)
        self.assertEqual(evidence.expected[path]["sha256"], pin)
        self.assertTrue((out / "evidence.zip").is_file())  # 原失败 attempt 不删除、不重封装。

    def test_archive_exception_persists_failed_terminal_summary_first_error_and_raw(self):
        out, allowed, evidence, summary = self.setup_evidence()
        raw = out / "variant-0-link.stderr"
        raw.write_bytes(b"preserved raw first-attempt diagnostic")
        evidence.register(raw)
        def archive_failure(*args):
            self.assertEqual(GATE.load_json(out / "summary.json")["status"], "packaging-pending")
            (out / "evidence.zip").write_bytes(b"partial ZIP attempt")
            raise OSError("modeled archive write exception")
        with mock.patch.object(GATE, "artifact", side_effect=archive_failure) as archive, contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        archive.assert_called_once()
        self.assert_failed(out, result)
        persisted = GATE.load_json(out / "summary.json")
        self.assertEqual(persisted["first_failure"]["message"], "modeled archive write exception")
        self.assertEqual(raw.read_bytes(), b"preserved raw first-attempt diagnostic")
        self.assertEqual((out / "evidence.zip").read_bytes(), b"partial ZIP attempt")

    def test_archive_failure_keeps_existing_build_first_error(self):
        out, allowed, evidence, summary = self.setup_evidence()
        previous = {"phase": "build", "type": "GateError", "message": "original compiler failure"}
        summary.update({"status": "failed", "first_failure": previous, "failures": [previous]})
        with mock.patch.object(GATE, "artifact", side_effect=OSError("later archive exception")), contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        self.assert_failed(out, result)
        persisted = GATE.load_json(out / "summary.json")
        self.assertEqual(persisted["first_failure"], previous)
        self.assertEqual(persisted["failures"][0], previous)
        self.assertEqual(persisted["failures"][1]["phase"], "archive-finalization")

    def combined_finalization_failure(self, fault):
        out, allowed, evidence, summary = self.setup_evidence()
        raw = out / "variant-0-link.stderr"
        raw.write_bytes(b"opaque raw bytes; orchestration fault control only")
        evidence.register(raw)
        original_verify = GATE.verify_bundle
        original_failed = GATE.controlled_failed_json
        checks = {"verification": 0, "terminal_writes": 0}
        def verify(*args, **kwargs):
            checks["verification"] += 1
            if checks["verification"] == 2:
                self.assertFalse((out / "terminal-status.json").exists())
                self.assertTrue((out / GATE.TERMINAL_PRECOMMIT).is_file())
                if fault == "second-verification":
                    raise OSError("modeled second final verification failure")
            return original_verify(*args, **kwargs)
        def input_check():
            if fault == "final-input" and checks["verification"] == 2:
                self.assertFalse((out / "terminal-status.json").exists())
                raise OSError("modeled final input check failure")
        def failed_write(folder, name, value):
            if name == "terminal-status.json":
                checks["terminal_writes"] += 1
                raise OSError("modeled blocked failed-terminal persistence")
            return original_failed(folder, name, value)
        evidence.input_validator = input_check
        with mock.patch.object(GATE, "verify_bundle", side_effect=verify), mock.patch.object(GATE, "controlled_failed_json", side_effect=failed_write), mock.patch.object(GATE, "commit_terminal") as commit, contextlib.redirect_stdout(io.StringIO()) as output:
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        commit.assert_not_called()
        self.assertEqual(result, 1)
        self.assertEqual(checks["verification"], 2)
        self.assertEqual(checks["terminal_writes"], 1)
        self.assertFalse((out / "terminal-status.json").exists())  # 不需要撤回已发布 success。
        self.assertEqual(GATE.load_json(out / "summary.json")["status"], "failed")
        failure = GATE.load_json(out / "archive-failure.json")
        self.assertFalse(failure["archive_verified"])
        self.assertEqual(failure["archive_failure"]["phase"], "archive-finalization")
        self.assertEqual(GATE.load_json(out / GATE.TERMINAL_PRECOMMIT)["authority_filename"], "terminal-status.json")
        self.assertEqual(raw.read_bytes(), b"opaque raw bytes; orchestration fault control only")
        self.assertTrue((out / "evidence.zip").is_file())
        record = json.loads(output.getvalue())
        self.assertEqual(record["status"], "failed")
        self.assertEqual([item["file"] for item in record["persistence_errors"]], ["terminal-status.json"])

    def test_second_final_verification_and_failed_terminal_write_failure_leave_no_success_authority(self):
        self.combined_finalization_failure("second-verification")

    def test_final_input_check_and_failed_terminal_write_failure_leave_no_success_authority(self):
        self.combined_finalization_failure("final-input")

    def test_atomic_publish_failure_and_failed_terminal_write_failure_leave_authority_absent(self):
        out, allowed, evidence, summary = self.setup_evidence()
        original_failed = GATE.controlled_failed_json
        def failed_write(folder, name, value):
            if name == "terminal-status.json":
                raise OSError("modeled failed-terminal persistence blocked")
            return original_failed(folder, name, value)
        with mock.patch.object(GATE.os, "rename", side_effect=OSError("modeled atomic publication failure")) as rename, mock.patch.object(GATE, "controlled_failed_json", side_effect=failed_write), contextlib.redirect_stdout(io.StringIO()):
            result = GATE.finish_evidence(out, summary, allowed, evidence)
        rename.assert_called_once_with(out / GATE.TERMINAL_PRECOMMIT, out / "terminal-status.json")
        self.assertEqual(result, 1)
        self.assertFalse((out / "terminal-status.json").exists())
        self.assertTrue((out / GATE.TERMINAL_PRECOMMIT).is_file())
        self.assertEqual(GATE.load_json(out / "summary.json")["status"], "failed")
        failure = GATE.load_json(out / "archive-failure.json")
        self.assertFalse(failure["archive_verified"])
        self.assertEqual(failure["archive_failure"]["message"], "modeled atomic publication failure")


if __name__ == "__main__":
    unittest.main()
