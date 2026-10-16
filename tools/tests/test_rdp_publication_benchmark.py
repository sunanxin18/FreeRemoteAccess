"""固定benchmark parser与小型host child控制；不运行Rust或下载原件。"""
import gzip
import hashlib
import importlib.util
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("publication_benchmark", Path(__file__).resolve().parents[1] / "verify-rdp-publication-benchmark.py")
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)


def fixture(selector="linux-x86_64"):
    """纯文本原件：降序31..1使median=16/p95=30；四route各有独立倍数。"""
    _, target_os, arch, bits = BENCH.TARGETS[selector]
    lines = [BENCH.META, BENCH.ORACLE, BENCH.SCOPE,
             f'BENCH_CPU process_arch={arch} os={target_os} pointer_bits={bits} vendor="GenuineIntel" brand="Host fixture only" leaf1_eax=0x00000000 leaf1_ecx=0x00000200 leaf1_edx=0x04000000 actual_sse2=true actual_ssse3=true host_scope=external-runner-record']
    for case in range(30):
        count = BENCH.COUNTS[case // 2]
        calls = 4096 if count <= 65 else 64 if count <= 4096 else 1
        source_sha, output_sha = BENCH.CASE_SHA[count]
        src, dst = [(0, 0), (1, 3)][case % 2]
        lines.append(f"BENCH_CASE case={case} pixels={count} src_mod16={src} dst_mod16={dst} calls={calls} input_sha256={source_sha} expected_sha256={output_sha}")
        for order in range(4):
            route = BENCH.ROUTES[(case + order) % 4]
            lines.append(f"BENCH_FIRST case={case} route={route} calls=1 elapsed_ns=42")
        for sample in range(31):
            for order in range(4):
                index = (case + 9 + sample + order) % 4
                route = BENCH.ROUTES[index]
                ns = (31 - sample) * (index + 1)
                lines.append(f"BENCH_SAMPLE case={case} route={route} sample={sample} order={order} calls={calls} elapsed_ns={ns}")
        for index, route in enumerate(BENCH.ROUTES):
            median, p95 = 16 * (index + 1), 30 * (index + 1)
            mib = "NA" if count == 0 else f"{(count * 4.0 * calls * 1e9) / (median * 1048576.0):.6f}"
            lines.append(f"BENCH_SUMMARY case={case} route={route} calls={calls} samples=31 median_batch_ns={median} p95_batch_ns={p95} median_ns_per_call={median / calls:.6f} p95_ns_per_call={p95 / calls:.6f} payload_mib_s={mib}")
        lines.append(f"BENCH_COMPARE case={case} reference_over_sse2=0.500000 reference_over_ssse3=0.333333 current_dispatch=reported-separately")
        lines.append(f"BENCH_CASE_DONE case={case} output_sha256={output_sha} source=unchanged canaries=unchanged")
    lines.append(BENCH.DONE)
    return ("\n".join(lines) + "\n").encode("ascii")


class ParserTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.raw = fixture()

    def rejects(self, raw, stderr=b"", native_exit=0, selector="linux-x86_64"):
        with self.assertRaises(ValueError):
            BENCH.parse_benchmark(raw, stderr, native_exit, selector)

    def test_complete_all_four_targets_and_consistent_crlf(self):
        for selector in BENCH.TARGETS:
            with self.subTest(selector=selector):
                for raw in (fixture(selector), fixture(selector).replace(b"\n", b"\r\n")):
                    result = BENCH.parse_benchmark(raw, b"", 0, selector)
                    self.assertEqual((result["lines"], result["samples"]), (4055, 3720))

    def test_raw_layout_partial_native_stderr_and_cpu(self):
        for raw in (self.raw[:-1], self.raw + b"\n", self.raw.replace(b"\n", b"\r\n", 1),
                    self.raw.replace(b"scope=private-", b"scope=\xffprivate-", 1),
                    self.raw.replace(b"actual_ssse3=true", b"actual_ssse3=false", 1),
                    self.raw.replace(b"leaf1_ecx=0x00000200", b"leaf1_ecx=0x00000000", 1),
                    self.raw.replace(b"pointer_bits=64", b"pointer_bits=32", 1),
                    self.raw.replace(b"BENCH_OK", b"BENCH_PARTIAL", 1),
                    self.raw.replace(b"speed_threshold=none", b"speed_threshold=2", 1)):
            with self.subTest(raw=raw[-80:]):
                self.rejects(raw)
        self.rejects(self.raw, native_exit=77)
        self.rejects(self.raw, native_exit=124)
        self.rejects(self.raw, stderr=b"warning\n")
        self.rejects(self.raw, selector="macos-x86_64")

    def test_case_sha_alignment_calls_and_counts(self):
        for old, new in [(b"pixels=8294400", b"pixels=2560"), (b"src_mod16=1 dst_mod16=3", b"src_mod16=0 dst_mod16=0"),
                         (b"calls=4096", b"calls=4095"),
                         (BENCH.CASE_SHA[33][0].encode(), b"0" * 64), (BENCH.CASE_SHA[65][1].encode(), b"f" * 64),
                         (b"source=unchanged", b"source=changed")]:
            with self.subTest(old=old):
                self.rejects(self.raw.replace(old, new, 1))
        lines = self.raw.splitlines(keepends=True)
        # 相同行数仍必须识别漏sample+重复sample与路由/轮转顺序变化。
        duplicated = list(lines)
        duplicated[10] = duplicated[9]
        self.rejects(b"".join(duplicated))
        swapped = list(lines)
        swapped[9], swapped[10] = swapped[10], swapped[9]
        self.rejects(b"".join(swapped))
        self.rejects(b"".join(lines[:10] + lines[11:]))

    def test_all_statistics_recomputed_from_raw_samples(self):
        for old, new in [(b"median_batch_ns=16", b"median_batch_ns=17"), (b"p95_batch_ns=30", b"p95_batch_ns=29"),
                         (b"median_ns_per_call=0.003906", b"median_ns_per_call=0.003907"),
                         (b"payload_mib_s=NA", b"payload_mib_s=0.000000"),
                         (b"payload_mib_s=976562.500000", b"payload_mib_s=1953125.000000"),
                         (b"reference_over_sse2=0.500000", b"reference_over_sse2=2.000000"),
                         (b"reference_over_ssse3=0.333333", b"reference_over_ssse3=0.333334")]:
            self.assertIn(old, self.raw)
            with self.subTest(old=old):
                self.rejects(self.raw.replace(old, new, 1))
        # 修改raw而不改summary；强行末行BENCH_OK不能掩盖错误。
        self.rejects(self.raw.replace(b"sample=15 order=0 calls=4096 elapsed_ns=16", b"sample=15 order=0 calls=4096 elapsed_ns=999", 1))

    def test_u128_zero_and_canonical_integer(self):
        # 首sample原最大值31被替换为u128MAX，median16/p9530不变。
        old = b"route=reference sample=0 order=3 calls=4096 elapsed_ns=31"
        self.assertIn(old, self.raw)
        maximum = self.raw.replace(old, old.rsplit(b"=", 1)[0] + b"=" + str(2 ** 128 - 1).encode(), 1)
        BENCH.parse_benchmark(maximum, b"", 0, "linux-x86_64")
        for value in (b"-1", b"+31", b"031", b"31.0", str(2 ** 128).encode()):
            self.rejects(self.raw.replace(old, old.rsplit(b"=", 1)[0] + b"=" + value, 1))
        # 全零原样本的独立替换，分母为0时必须明确UNDEFINED而不是inf。
        zero = self.raw.decode()
        import re
        zero = re.sub(r"elapsed_ns=[0-9]+", "elapsed_ns=0", zero)
        zero = re.sub(r"median_batch_ns=[0-9]+ p95_batch_ns=[0-9]+ median_ns_per_call=[0-9.]+ p95_ns_per_call=[0-9.]+ payload_mib_s=[A-Z0-9.]+",
                      "median_batch_ns=0 p95_batch_ns=0 median_ns_per_call=0.000000 p95_ns_per_call=0.000000 payload_mib_s=NA", zero)
        zero = zero.replace("reference_over_sse2=0.500000 reference_over_ssse3=0.333333", "reference_over_sse2=UNDEFINED_ZERO_ELAPSED reference_over_ssse3=UNDEFINED_ZERO_ELAPSED")
        BENCH.parse_benchmark(zero.encode(), b"", 0, "linux-x86_64")
        self.rejects(zero.replace("UNDEFINED_ZERO_ELAPSED", "inf", 1).encode())

    def test_frozen_record165_sha_table_independently(self):
        repo = Path(__file__).resolve().parents[2]
        archive = (repo / BENCH.ARCHIVE).read_bytes()
        self.assertEqual(hashlib.sha256(archive).hexdigest(), BENCH.ARCHIVE_SHA)
        raw = gzip.decompress(archive)
        self.assertEqual((len(raw), hashlib.sha256(raw).hexdigest()), (6453432, BENCH.RAW_SHA))
        self.assertEqual(raw[:8], b"FRDPIX01")
        offset = 8
        for index in range(176):
            kind, sf, df, _ = raw[offset:offset + 4]
            sl, ol = struct.unpack_from("<II", raw, offset + 4)
            offset += 12 + (768 if kind == 0 else 0)
            source, output = raw[offset:offset + sl], raw[offset + sl:offset + sl + ol]
            offset += sl + ol
            if index != 165:
                continue
            self.assertEqual((kind, sf, df, sl, ol), (6, 6, 5, 384, 384))
            expected = bytes(value for pixel in range(96) for value in (source[pixel * 4 + 2], source[pixel * 4 + 1], source[pixel * 4], 255))
            self.assertEqual(output, expected)
            for count, hashes in BENCH.CASE_SHA.items():
                length = count * 4
                actual = tuple(hashlib.sha256((data * ((length + 383) // 384))[:length]).hexdigest() for data in (source, output))
                self.assertEqual(actual, hashes)
        self.assertEqual(offset, len(raw))


class CommandTests(unittest.TestCase):
    def test_closed_build_commands(self):
        for selector, (target, target_os, _, _) in BENCH.TARGETS.items():
            args = BENCH.build_command("cargo", target, Path("/fixed/benchmark.map"), target_os)
            self.assertEqual(args[:5], ["cargo", "rustc", "--release", "--locked", "-v"])
            self.assertEqual(args.count("--target"), 1)
            self.assertNotIn("--test", args)
            self.assertNotIn("--no-default-features", args)
            self.assertTrue(args[-1].startswith("link-arg=" + ("/MAP:" if target_os == "windows" else "-Wl,-Map,")))

    def test_windows_command_quotes_round_trip(self):
        for args in [[r"C:\Rust tools\rustc.exe", "--crate-name", "sample"], ["rustc", 'a"b', "", "C:" + chr(92) + "ends with slash" + chr(92)],
                     ["rustc", r"link-arg=/MAP:C:\output dir\bench.map"]]:
            self.assertEqual(BENCH.windows_argv(subprocess.list2cmdline(args)), args)

    def test_actual_argv_not_cargo_claim_and_no_test(self):
        for selector, (target, target_os, _, _) in BENCH.TARGETS.items():
            compiler = r"C:\Rust tools\rustc.exe" if target_os == "windows" else "/toolchain/rustc"
            map_path = Path("/fixed/map")
            args = [compiler, "--crate-name", BENCH.EXAMPLE, "crates/frd-rdp-legacy-kernels/examples/rdp_publication_benchmark.rs",
                    "--crate-type", "bin", "--emit=dep-info,link", "--target", target, "-C", "opt-level=3", "-C", "lto=thin",
                    "-C", BENCH.build_command("cargo", target, map_path, target_os)[-1]]
            def log(values):
                import shlex
                command = subprocess.list2cmdline(values) if target_os == "windows" else shlex.join(values)
                return ("     Running `" + command + "`\n").encode()
            with mock.patch.object(BENCH, "file_identity", return_value=dict(bytes=1, sha256="mock")):
                self.assertEqual(BENCH.parse_rustc(log(args), target, map_path, target_os)["argv"], args)
                for bad in (args + ["--test"], args + ["--target", "wrong"], args + ["-C", "target-feature=+ssse3"], args + ["--cfg", "test"]):
                    with self.assertRaises(ValueError):
                        BENCH.parse_rustc(log(bad), target, map_path, target_os)
                with self.assertRaises(ValueError):
                    BENCH.parse_rustc(log(args) * 2, target, map_path, target_os)


class SetupTests(unittest.TestCase):
    def test_fresh_root_creates_fixed_parent_and_partial_identity_without_cargo(self):
        with tempfile.TemporaryDirectory(prefix="frd-publication-fresh-") as directory:
            repo = Path(directory)
            self.assertFalse((repo / "target").exists())
            with mock.patch.object(BENCH, "__file__", str(repo / "tools" / "runner.py")), mock.patch.object(BENCH.platform, "system", return_value="Linux"), mock.patch.object(BENCH.subprocess, "Popen") as popen:
                self.assertEqual(BENCH.run("linux-x86_64"), 1)
                popen.assert_not_called()
            evidence = repo / "target" / "rdp-publication-benchmark-linux-x86_64"
            import json
            identity = json.loads((evidence / "runtime.identity.json").read_text())
            self.assertIn("FileNotFoundError", identity["first_failure"])
            self.assertFalse(identity["build_complete"])
            self.assertFalse(identity["runtime_complete"])
            self.assertEqual(identity["stages"], [])
            for stage in ("build", "benchmark"):
                for stream in ("stdout", "stderr"):
                    self.assertEqual((evidence / (stage + "." + stream)).read_bytes(), b"")

    def test_existing_cargo_output_produces_partial_identity_without_build(self):
        with tempfile.TemporaryDirectory(prefix="frd-publication-existing-") as directory:
            repo = Path(directory)
            cargo = repo / "target" / "rdp-publication-benchmark-build-linux-x86_64"
            cargo.mkdir(parents=True)
            with mock.patch.object(BENCH, "__file__", str(repo / "tools" / "runner.py")), mock.patch.object(BENCH.platform, "system", return_value="Linux"), mock.patch.object(BENCH.subprocess, "Popen") as popen:
                self.assertEqual(BENCH.run("linux-x86_64"), 1)
                popen.assert_not_called()
            import json
            identity = json.loads((repo / "target" / "rdp-publication-benchmark-linux-x86_64" / "runtime.identity.json").read_text())
            self.assertIn("拒绝既有Cargo目录", identity["first_failure"])
            self.assertEqual(identity["stages"], [])

    def test_invalid_target_or_existing_evidence_never_overwritten(self):
        for kind in ("file", "symlink", "existing-evidence"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory(prefix="frd-publication-invalid-") as directory:
                repo = Path(directory)
                target = repo / "target"
                if kind == "file":
                    target.write_bytes(b"preserve")
                elif kind == "symlink":
                    (repo / "other").mkdir()
                    target.symlink_to(repo / "other", target_is_directory=True)
                else:
                    evidence = target / "rdp-publication-benchmark-linux-x86_64"
                    evidence.mkdir(parents=True)
                    (evidence / "runtime.identity.json").write_bytes(b"preserve")
                with mock.patch.object(BENCH, "__file__", str(repo / "tools" / "runner.py")), mock.patch.object(BENCH.subprocess, "Popen") as popen:
                    with self.assertRaises(ValueError):
                        BENCH.run("linux-x86_64")
                    popen.assert_not_called()
                if kind == "file":
                    self.assertEqual(target.read_bytes(), b"preserve")
                elif kind == "symlink":
                    self.assertEqual(list((repo / "other").iterdir()), [])
                else:
                    self.assertEqual((evidence / "runtime.identity.json").read_bytes(), b"preserve")


class LifecycleTests(unittest.TestCase):
    def capture(self, program, budget=1, drain=0.1, limits=(65536, 65536)):
        with tempfile.TemporaryDirectory(prefix="frd-publication-host-") as directory:
            root = Path(directory)
            with mock.patch.dict(BENCH.BUDGETS, benchmark=budget), mock.patch.dict(BENCH.LIMITS, benchmark=limits), mock.patch.object(BENCH, "EOF_SECONDS", drain):
                before = time.monotonic()
                record = BENCH.capture("benchmark", [sys.executable, "-c", program], root, None, root)
                duration = time.monotonic() - before
            raw = tuple((root / ("benchmark." + stream)).read_bytes() for stream in ("stdout", "stderr"))
            # 未知writer fixture自行有限退出；不扩大kill到后代。
            if not record["files_closed"]:
                time.sleep(0.35)
            return record, raw, duration

    def test_exact_raw_eof_closed_native_zero(self):
        record, raw, _ = self.capture("import os; os.write(1,b'\\x00\\xff\\r\\n'); os.write(2,b'\\xfe')")
        self.assertEqual(raw, (b"\x00\xff\r\n", b"\xfe"))
        self.assertTrue(record["capture_complete"])
        self.assertEqual(record["failure_exit"], 0)

    def test_native77_not_overwritten(self):
        record, raw, _ = self.capture("import sys; print('partial'); sys.exit(77)")
        self.assertEqual((record["exit_code"], record["failure_exit"]), (77, 77))
        self.assertTrue(record["capture_complete"])
        BENCH.first_error(record, "later finalization", 1)
        self.assertEqual(record["failure_exit"], 77)

    def test_timeout_bounded_direct_reap(self):
        record, _, duration = self.capture("import time; time.sleep(2)", budget=0.05)
        self.assertTrue(record["timed_out"])
        self.assertTrue(record["terminal_observed"])
        self.assertEqual(record["failure_exit"], 124)
        self.assertLess(duration, 1.5)

    def test_inherited_unknown_writer_not_pass(self):
        record, _, duration = self.capture("import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time; time.sleep(0.25)']); print('direct done')", drain=0.02)
        self.assertEqual(record["exit_code"], 0)
        self.assertFalse(record["capture_complete"])
        self.assertFalse(record["files_closed"])
        self.assertNotEqual(record["failure_exit"], 0)
        self.assertLess(duration, 1)

    def test_stream_limit_cannot_pass(self):
        record, raw, _ = self.capture("import os; os.write(1,b'x'*65536)", limits=(32, 32))
        self.assertLessEqual(len(raw[0]), 32)
        self.assertFalse(record["capture_complete"])
        self.assertNotEqual(record["failure_exit"], 0)


if __name__ == "__main__":
    unittest.main()
