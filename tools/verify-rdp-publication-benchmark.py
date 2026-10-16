#!/usr/bin/env python3
"""四个固定普通 publication benchmark；不授 shipping、ABI 或端到端延迟资格。"""

import hashlib
import json
import ntpath
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import stat
import struct
import subprocess
import sys
import threading
import time


TARGETS = {
    "windows-x86": ("i686-pc-windows-msvc", "windows", "x86", 32),
    "windows-x86_64": ("x86_64-pc-windows-msvc", "windows", "x86_64", 64),
    "linux-i686": ("i686-unknown-linux-gnu", "linux", "x86", 32),
    "linux-x86_64": ("x86_64-unknown-linux-gnu", "linux", "x86_64", 64),
}
EXAMPLE = "rdp_publication_benchmark"
BUDGETS = {"build": 900, "benchmark": 130}
REAP_SECONDS = 1
EOF_SECONDS = 2
LIMITS = {"build": (64 * 1024 * 1024, 64 * 1024 * 1024), "benchmark": (2 * 1024 * 1024, 65536)}
COUNTS = (0, 1, 2, 3, 4, 7, 33, 65, 1024, 1920, 1921, 3840, 4096, 2073600, 8294400)
ROUTES = ("reference", "direct-sse2", "direct-ssse3", "current-dispatch")
ARCHIVE = "crates/frd-rdp-legacy-kernels/tests/pixel_original/original-pixel.bin.gz"
ARCHIVE_SHA = "ebfe8d6db6195ae1f45213733c8789fb48425f57e1ae2d106a9decc103592196"
RAW_SHA = "ee69627eb9513f5e71a7b310dd60b41ececf12ef72aa2be45562fc7cd0910f26"
OLD_SHA = "514cacd540e94cdef57c15482022ea50b8efe894cc512d9b8ae4e1cbc987aa2b"
INPUT_SHA = "19f8954d765f4687af7e34ce46875f1680e460c3244ec8e20614bfa84ac7ed08"
OUTPUT_SHA = "944256d1f6be362de8dd56fe36eead743046f3bf0fcf46f32091efce5a0b92a8"
# 从原版 record165 独立按 bytes 循环扩展后预计算并转录；运行时不接纳新 SHA。
CASE_SHA = {
    0: ("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
    1: ("c33c6e9c3e87cd5de1f3c97d62b6fbe403eaa234feb02f2a838cd1c4b3e76f23", "0392da569c92593e2cb47507feb8bf59de1144b0956747a694a92cc1c7a1034f"),
    2: ("f857fafae86fa7cf72d2d5bc9a1382e8dc2be1a842f3a68a7076ada3a615440e", "6574d4dfcff59b4538ddbcffba9b432cec1102705b35e624c20038397681267d"),
    3: ("08169b2ab0a229f48263e6cc94091b74853b343777c415c2afff85b0d7fc28b1", "2d8f26d4c834042928a2de5dbae678779bcf6df4465a99a831169ef44640372a"),
    4: ("d3a271040fb40b99f14b2f699ed3a4cebdb6333d67680d2b76ca5e9f44b1d730", "8f2255be618c94869accc301aab69a7567aa087b95e4a2e2cce50e3d90980a25"),
    7: ("ab316b09387016bdfa9911d73e471db6b2ac0b4b826667e9b3a446a6faa9e069", "e5b369f07a6efe07a4fedd210596d2c054dceda308f6723a3ac22d0c4c8b8a3e"),
    33: ("b095ff847f26c8ad6507ad0983fe8929bb0d04243bf9b2ca92c2315f9fd6097b", "7d2795fca47047de57aa6865f0aec0535d27669b3a45bb99b7423f420aaa8aaf"),
    65: ("c0e184779dd61851bf71f25181b858de5701f10517e7b22dd2e4c5de1e97c191", "0e498a5182fc9b78cbea1cfcf015147826ab2409320f131594032b4e9089f387"),
    1024: ("e57149ed261593c84490ba2e827d88950adf9dc0cf9a08d5b429d44e20a87b77", "3e8618a686d190d0f73d4bcfe3bafccd04f05c768d7fcc1f1f5a18c54f040c30"),
    1920: ("5acd879e526fc6466eb613014a86f37bc8261ba0f08df763197debcd0113f659", "53eaca2e39562b9b30f7b7b9f1f8315a86b5fa4a9ee14895e042a0fff86a0df2"),
    1921: ("83e0b976053e0c1872c5ce9ea69ffa7f7cfa045f618eabc90dbbf2acf83dfe95", "81ed9a2812d0daa4473787155b6795969b290685ac9cd5734bb2eb414f55acfb"),
    3840: ("c7596dfd11b57bd777bb192c0ca0c13ff3e23645c2205d348004105f132b1d92", "4857a97840ef69d878f3640b7ea8e8c4fcc7db11dd78231e6541c28e155c0d56"),
    4096: ("ac048c617275c8349493479a4c05733a47e912def65c7e4b00184be82867c899", "a2405b859d11a7d4b2963f033c33a6cbc376721a2b39afa496134e53d7522fbc"),
    2073600: ("8fcfabacda4489a00ece29c37f90249fbc3f7c99fcb078fc59cd74034c0b437c", "bd8374bdf48480d157e403940cddc0882e9734246f6cdf555e7fa066765c379c"),
    8294400: ("09f96a39880df6e8c660684ee2eba1ba97bb11c0be24c95e795ce24af742404b", "01a4ae29cb70c3351de408927b2360000601e742d6a6568dc9cd28a0dab67529"),
}
SOURCES = (
    ("crates/frd-rdp-legacy-kernels/examples/rdp_publication_benchmark.rs", "source-rdp_publication_benchmark.rs", "a473358d226cec1c1f3e0c6896869a1ad96a52b752bb003ebb5e2ab2f39292e6"),
    ("crates/frd-protocol-rdp/src/pixel_convert.rs", "source-pixel_convert.rs", "4fc0bd692b3eb8851224ffd3e2943e02e65a9b39e8bf15e533423d4bca62a7e6"),
    ("crates/frd-protocol-rdp/src/pixel_convert/x86.rs", "source-pixel_convert-x86.rs", "232fabaca6781a9e3ef5110de292d3c4b017066b847f7ee9b31ad4f7ad8aeaf9"),
    ("crates/frd-protocol-rdp/tests/fixtures/sse2-publication-red/pixel_convert.rs", "source-frozen-pixel_convert.rs", OLD_SHA),
)
CONFIGS = (
    "Cargo.toml", "Cargo.lock", ".cargo/config.toml", "rust-toolchain.toml",
    "crates/frd-rdp-legacy-kernels/Cargo.toml", "tools/verify-rdp-publication-benchmark.py",
    ".github/workflows/build-windows.yml", ".github/workflows/build-linux-ffmpeg.yml",
)
META = "BENCH_META scope=private-publication-warm-microbenchmark cases=30 routes=4 warmup_rounds=8 samples=31 total_budget_seconds=120 timed_allocation=false payload_mib_definition=one-source-payload"
ORACLE = f"BENCH_ORACLE record=165 pixels=96 archive_sha256={ARCHIVE_SHA} uncompressed_sha256={RAW_SHA} source_sha256={INPUT_SHA} output_sha256={OUTPUT_SHA} frozen_scalar_source_sha256={OLD_SHA}"
SCOPE = "BENCH_SCOPE direct_leaf_and_current_dispatch=separate-compilation-contexts frozen_rust_reference_auto_vectorization=not-excluded first_operation=per-case-after-isa-and-input-preparation cache_cold=not-claimed frame_rate=not-claimed end_to_end_latency=not-measured speed_threshold=none"
DONE = "BENCH_OK cases=30 routes=4 samples_per_route_case=31 scope=private-warm-only shipping=open full_unwind=open protocol_latency=open shared_runner_noise=not-excluded"


def require(ok, message):
    if not ok:
        raise ValueError(message)


def fixed_calls(pixels):
    return 4096 if pixels <= 65 else 64 if pixels <= 4096 else 1


def ratio(numerator, denominator):
    return "UNDEFINED_ZERO_ELAPSED" if denominator == 0 else f"{float(numerator) / float(denominator):.6f}"


def parse_benchmark(raw, stderr, native_exit, selector):
    """仅解析完整四路原件；77、partial 和任何非规范行均不能 PASS。"""
    require(selector in TARGETS, "目标不在四个固定目标中")
    require(native_exit == 0 and stderr == b"", "基准非0或stderr非空")
    require(len(raw) <= LIMITS["benchmark"][0] and raw.endswith(b"\n"), "输出长度或final newline不符")
    if b"\r" in raw:
        require(b"\n" not in raw.replace(b"\r\n", b""), "混用LF/CRLF")
        raw = raw.replace(b"\r\n", b"\n")
    require(all(v == 10 or 32 <= v < 127 for v in raw), "输出必须为ASCII可打印字符及一致换行")
    lines = raw.decode("ascii").split("\n")[:-1]
    require(len(lines) == 4055, "必须恰好4055行")
    position = 0

    def take(expected):
        nonlocal position
        require(lines[position] == expected, f"第{position + 1}行合同不符")
        position += 1

    def elapsed(prefix):
        nonlocal position
        match = re.fullmatch(re.escape(prefix) + r"(0|[1-9][0-9]{0,38})", lines[position])
        require(match is not None, f"第{position + 1}行原整数或固定字段不符")
        value = int(match[1])
        require(value < 2 ** 128, "elapsed_ns不是u128")
        position += 1
        return value

    take(META)
    take(ORACLE)
    take(SCOPE)
    _, target_os, arch, bits = TARGETS[selector]
    # Rust Debug字符串只接受ASCII转义形式；不从日志中的名字推断CPU准入。
    quoted = r'"(?:[^"\\\x00-\x1f]|\\(?:["\\nrt0]|x[0-9a-f]{2}|u\{[0-9a-f]+\}))*"'
    cpu_pattern = (rf"BENCH_CPU process_arch={arch} os={target_os} pointer_bits={bits} vendor=({quoted}) brand=({quoted}) "
                   r"leaf1_eax=0x([0-9a-f]{8}) leaf1_ecx=0x([0-9a-f]{8}) leaf1_edx=0x([0-9a-f]{8}) "
                   r"actual_sse2=true actual_ssse3=true host_scope=external-runner-record")
    cpu = re.fullmatch(cpu_pattern, lines[position])
    require(cpu is not None, "真实CPU/目标记录不完整或缺SSSE3")
    require(int(cpu[4], 16) & (1 << 9) and int(cpu[5], 16) & (1 << 26), "CPUID与SSE2/SSSE3准入不一致")
    cpu_line = lines[position]
    position += 1
    for case in range(30):
        pixels = COUNTS[case // 2]
        source_mod, destination_mod = ((0, 0), (1, 3))[case % 2]
        calls = fixed_calls(pixels)
        input_sha, output_sha = CASE_SHA[pixels]
        take(f"BENCH_CASE case={case} pixels={pixels} src_mod16={source_mod} dst_mod16={destination_mod} calls={calls} input_sha256={input_sha} expected_sha256={output_sha}")
        for order in range(4):
            route = ROUTES[(case + order) % 4]
            elapsed(f"BENCH_FIRST case={case} route={route} calls=1 elapsed_ns=")
        samples = {route: [] for route in ROUTES}
        for sample in range(31):
            for order in range(4):
                route = ROUTES[(case + 1 + 8 + sample + order) % 4]
                value = elapsed(f"BENCH_SAMPLE case={case} route={route} sample={sample} order={order} calls={calls} elapsed_ns=")
                samples[route].append(value)
        medians = {}
        for route in ROUTES:
            sorted_ns = sorted(samples[route])
            median, p95 = sorted_ns[15], sorted_ns[29]
            medians[route] = median
            # 和Rust同一f64运算顺序；保留31个原u128，不先做整数per-call除法。
            mib_s = "NA" if pixels == 0 or median == 0 else f"{float(pixels * 4) * float(calls) * 1_000_000_000.0 / (float(median) * 1_048_576.0):.6f}"
            take(f"BENCH_SUMMARY case={case} route={route} calls={calls} samples=31 median_batch_ns={median} p95_batch_ns={p95} median_ns_per_call={float(median) / float(calls):.6f} p95_ns_per_call={float(p95) / float(calls):.6f} payload_mib_s={mib_s}")
        take(f"BENCH_COMPARE case={case} reference_over_sse2={ratio(medians['reference'], medians['direct-sse2'])} reference_over_ssse3={ratio(medians['reference'], medians['direct-ssse3'])} current_dispatch=reported-separately")
        take(f"BENCH_CASE_DONE case={case} output_sha256={output_sha} source=unchanged canaries=unchanged")
    take(DONE)
    require(position == len(lines), "存在额外行")
    return dict(complete=True, lines=4055, cases=30, samples=3720, cpu=cpu_line,
                scope="private warm microbenchmark; no speed threshold; shipping remains open")


def first_error(record, message, code=1):
    if record["failure_exit"] == 0:
        record["failure_exit"] = code
        record["first_failure"] = message


def capture(stage, argv, repo, env, evidence):
    """本runner仅build/benchmark两处固定调用；线程分别独占pipe和原件文件。"""
    require(stage in BUDGETS, "未知固定stage")
    record = dict(stage=stage, argv=argv, budget_seconds=BUDGETS[stage], started=False,
                  terminal_observed=False, timed_out=False, exit_code=None,
                  stdout_eof=False, stderr_eof=False, files_closed=False,
                  capture_complete=False, failure_exit=0, first_failure=None, errors=[],
                  ownership_scope="direct Popen handle only; descendants not proven quiescent")
    process = None
    readers = []
    streams = []
    cancelled = threading.Event()
    reader_failure = threading.Event()
    started = time.monotonic()
    record["start_ns"] = time.time_ns()

    def copy(pipe, output, state, limit):
        # main不跨线程关闭pipe/file；未知EOF时也不称原件稳定。没有communicate内存缓冲。
        try:
            while not cancelled.is_set():
                chunk = os.read(pipe.fileno(), 65536)
                if not chunk:
                    state["eof"] = True
                    break
                if cancelled.is_set():
                    break
                if state["bytes"] + len(chunk) > limit:
                    remaining = limit - state["bytes"]
                    written = output.write(chunk[:remaining])
                    state["bytes"] += written
                    require(written == remaining, "raw文件short write")
                    raise ValueError("固定raw流大小上界耗尽")
                written = output.write(chunk)
                state["bytes"] += written
                require(written == len(chunk), "raw文件short write")
        except BaseException as error:
            state["errors"].append(type(error).__name__ + ": " + str(error))
            reader_failure.set()
        finally:
            close_complete = True
            for owned in (pipe, output):
                try:
                    owned.close()
                except BaseException as error:
                    state["errors"].append("close: " + str(error))
                    reader_failure.set()
                    close_complete = False
            state["closed"] = close_complete

    try:
        # launch失败时仍保留两份空raw；成功后所有权逐一交给对应reader。
        for name in ("stdout", "stderr"):
            streams.append((evidence / (stage + "." + name)).open("xb", buffering=0))
        process = subprocess.Popen(argv, cwd=repo, env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        record["started"] = True
        record["pid"] = process.pid
        for index, (name, pipe, output) in enumerate(zip(("stdout", "stderr"), (process.stdout, process.stderr), streams)):
            state = dict(eof=False, closed=False, bytes=0, errors=[])
            record[name] = state
            thread = threading.Thread(target=copy, args=(pipe, output, state, LIMITS[stage][index]), daemon=True)
            thread.start()
            readers.append(thread)
        while True:
            code = process.poll()
            if code is not None:
                record["terminal_observed"] = True
                record["exit_code"] = code
                if code != 0:
                    first_error(record, "native nonzero", code)
                break
            if reader_failure.is_set():
                first_error(record, "raw reader failed")
                break
            if time.monotonic() - started >= BUDGETS[stage]:
                record["timed_out"] = True
                first_error(record, "fixed direct process budget exceeded", 124)
                break
            time.sleep(0.01)
    except Exception as error:
        record["errors"].append(type(error).__name__ + ": " + str(error))
        first_error(record, record["errors"][-1])
    finally:
        if process is not None and record["started"] and not record["terminal_observed"]:
            try:
                process.kill()  # 不用killpg、taskkill或树终止，也不重试目标程序。
            except OSError as error:
                record["errors"].append("direct kill: " + str(error))
            try:
                record["exit_code"] = process.wait(timeout=REAP_SECONDS)
                record["terminal_observed"] = True
            except (subprocess.TimeoutExpired, OSError) as error:
                record["errors"].append("bounded direct reap: " + str(error))
        deadline = time.monotonic() + EOF_SECONDS
        for thread in readers:
            thread.join(max(0, deadline - time.monotonic()))
        if any(thread.is_alive() for thread in readers):
            cancelled.set()
            record["errors"].append("raw EOF/file owner remains unknown after fixed 2s drain; partial")
        # 未交给线程的文件仍由main关闭；已交出的资源只由reader finally关闭。
        for output in streams[len(readers):]:
            try:
                output.close()
            except OSError as error:
                record["errors"].append("untransferred file close: " + str(error))
        if process is not None:
            for pipe in (process.stdout, process.stderr)[len(readers):]:
                if pipe is not None:
                    try:
                        pipe.close()
                    except OSError as error:
                        record["errors"].append("untransferred pipe close: " + str(error))
        for name in ("stdout", "stderr"):
            state = record.get(name, dict(eof=False, closed=False, bytes=0, errors=[]))
            record[name + "_eof"] = state["eof"]
            record["errors"].extend(name + ": " + error for error in state["errors"])
        record["files_closed"] = len(readers) == 2 and all(record[name]["closed"] for name in ("stdout", "stderr")) and not any(t.is_alive() for t in readers)
        record["capture_complete"] = (record["terminal_observed"] and record["stdout_eof"] and record["stderr_eof"]
                                      and record["files_closed"] and not record["errors"])
        if not record["capture_complete"]:
            first_error(record, "partial terminal/EOF/closure or capture failure")
        record["end_ns"] = time.time_ns()
    return record


def file_identity(path):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode), "输入/原件必须为普通非链接文件: " + str(path))
    digest = hashlib.sha256()
    with path.open("rb") as raw:
        for chunk in iter(lambda: raw.read(65536), b""):
            digest.update(chunk)
    return dict(bytes=info.st_size, sha256=digest.hexdigest())


def windows_argv(command):
    """只解码Cargo保存的Windows命令行；不调用shell，不执行解析结果。"""
    result = []
    i = 0
    while i < len(command):
        while i < len(command) and command[i] in " \t":
            i += 1
        if i == len(command):
            break
        token = ""
        quoted = False
        while i < len(command) and (quoted or command[i] not in " \t"):
            backslashes = 0
            while i < len(command) and command[i] == "\\":
                backslashes += 1
                i += 1
            if i < len(command) and command[i] == '"':
                token += "\\" * (backslashes // 2)
                if backslashes % 2:
                    token += '"'
                else:
                    quoted = not quoted
                i += 1
            else:
                token += "\\" * backslashes
                if i < len(command) and (quoted or command[i] not in " \t"):
                    token += command[i]
                    i += 1
        require(not quoted, "rustc日志引号不闭合")
        result.append(token)
    return result


def build_command(cargo, target, built_map, target_os):
    link_arg = "/MAP:" + str(built_map) if target_os == "windows" else "-Wl,-Map," + str(built_map)
    return [cargo, "rustc", "--release", "--locked", "-v", "-p", "frd-rdp-legacy-kernels",
            "--example", EXAMPLE, "--target", target, "--", "-C", "link-arg=" + link_arg]


def parse_rustc(raw, target, built_map, target_os):
    # 保留完整raw双流；额外转录唯一普通example实际rustc argv，不能以cargo argv代替。
    text = raw.decode("utf-8", errors="strict")
    matches = []
    for line in text.splitlines():
        found = re.fullmatch(r"\s*Running `(.+)`", line)
        if not found:
            continue
        command = found[1]
        args = windows_argv(command) if target_os == "windows" else shlex.split(command)
        if "--crate-name" in args and args[args.index("--crate-name") + 1] == EXAMPLE:
            matches.append((line, args))
    require(len(matches) == 1, "必须恰有一次普通example rustc调用")
    line, args = matches[0]
    require((ntpath.basename(args[0]) if target_os == "windows" else Path(args[0]).name).lower() in ("rustc", "rustc.exe"), "不接受rustc wrapper或未知callee")
    require("--test" not in args and "--emit=dep-info,link" in args, "必须为普通linked non-test图像")
    for option, value in (("--crate-type", "bin"), ("--target", target)):
        require(args.count(option) == 1 and args[args.index(option) + 1] == value, "真实rustc目标/类型不符")
    require(args.count("--crate-name") == 1, "crate-name重复")
    require(sum(arg.replace("\\", "/").endswith("/examples/" + EXAMPLE + ".rs") for arg in args) == 1, "真实example源路径不符")
    require(not any(args[i + 1] == "test" for i, value in enumerate(args[:-1]) if value == "--cfg"), "不接受cfg(test)")
    codes = [args[i + 1] for i, value in enumerate(args[:-1]) if value == "-C"]
    require("opt-level=3" in codes and "lto=thin" in codes, "实际release/thin-LTO不符")
    link = build_command("unused", target, built_map, target_os)[-1]
    require(codes.count(link) == 1, "实际rustc未绑定同次map")
    require(not any(value.startswith(("target-cpu=", "target-feature=", "panic=", "control-flow-guard=", "force-frame-pointers=")) for value in codes), "不得添加CPU/保护策略覆盖")
    return dict(raw_running_line=line, argv=args, compiler_path=args[0], compiler_identity=file_identity(Path(args[0])))


def check_binary(path, target_os, bits):
    size = path.stat().st_size
    require(64 <= size <= 64 * 1024 * 1024, "普通图像大小异常")
    with path.open("rb") as raw:
        header = raw.read(min(size, 1024 * 1024))
    if target_os == "linux":
        require(header[:4] == b"\x7fELF" and header[4:6] == bytes((1 if bits == 32 else 2, 1)), "ELF class/endian不符")
        require(struct.unpack_from("<H", header, 18)[0] == (3 if bits == 32 else 62), "ELF机器架构不符")
    else:
        require(header[:2] == b"MZ", "PE缺MZ")
        offset = struct.unpack_from("<I", header, 0x3c)[0]
        require(offset + 26 <= len(header) and header[offset:offset + 4] == b"PE\0\0", "PE签名/范围不符")
        machine = struct.unpack_from("<H", header, offset + 4)[0]
        optional_size = struct.unpack_from("<H", header, offset + 20)[0]
        magic = struct.unpack_from("<H", header, offset + 24)[0]
        require(optional_size >= 2 and offset + 24 + optional_size <= size, "PE optional header范围不符")
        require((machine, magic) == ((0x14c, 0x10b) if bits == 32 else (0x8664, 0x20b)), "PE机器架构不符")
    return dict(format="PE" if target_os == "windows" else "ELF", pointer_bits=bits)


def run(selector):
    target, target_os, arch, bits = TARGETS[selector]
    repo = Path(__file__).resolve().parent.parent
    evidence = repo / "target" / ("rdp-publication-benchmark-" + selector)
    cargo_output = repo / "target" / ("rdp-publication-benchmark-build-" + selector)
    # fresh checkout尚无target；只创建固定普通父目录，不写入链接或复用旧evidence。
    target_root = repo / "target"
    if not target_root.exists() and not target_root.is_symlink():
        target_root.mkdir()
    require(stat.S_ISDIR(target_root.lstat().st_mode), "target父路径必须为普通非链接目录")
    require(not evidence.exists() and not evidence.is_symlink(), "拒绝既有evidence目录")
    evidence.mkdir()
    identity_path = evidence / "runtime.identity.json"
    identity = dict(schema="frd.publication-benchmark.runtime.v1", selector=selector, target=target,
                    commit=os.environ.get("GITHUB_SHA"), run_id=os.environ.get("GITHUB_RUN_ID"),
                    run_attempt=os.environ.get("GITHUB_RUN_ATTEMPT"), job=os.environ.get("GITHUB_JOB"),
                    host=dict(system=platform.system(), machine=platform.machine(), python=sys.version),
                    required_rust="1.96.0; preceding workflow pin check", build_reused=False,
                    sources=[], configuration=[], tools=[], artifacts=[], stages=[],
                    build_complete=False, runtime_complete=False, first_failure=None, failure_exit=0,
                    ownership_scope="direct process handles only", descendants_quiescent="not proven",
                    shipping="open", full_unwind="open", protection="not-qualified; defaults unchanged")

    def fail(message, code=1):
        first_error(identity, message, code)

    def save():
        pending = evidence / "runtime.identity.pending.json"
        pending.write_text(json.dumps(identity, ensure_ascii=True, indent=2) + "\n", encoding="utf-8")
        os.replace(pending, identity_path)

    def unchanged(phase):
        for row in identity["sources"] + identity["configuration"] + [identity["oracle_archive"]]:
            row[phase] = file_identity(repo / row["path"])
            require(row[phase] == row["before"], "输入变化: " + row["path"])
            if "snapshot" in row:
                row["snapshot_" + phase] = file_identity(evidence / row["snapshot"])
                require(row["snapshot_" + phase] == row["before"], "源snapshot变化")
        for row in identity["artifacts"]:
            row[phase] = dict(build=file_identity(Path(row["build_path"])), staged=file_identity(evidence / row["path"]))
            require(row[phase]["build"] == row["build"] == row["staged"] == row[phase]["staged"], "build/staged图像或map变化")
        for row in identity["tools"]:
            row[phase] = file_identity(Path(row["path"]))
            require(row[phase] == row["before"], "工具文件变化")

    save()
    try:
        require(platform.system().lower() == target_os, "只接受对应原生Windows/Linux runner")
        require(not cargo_output.exists() and not cargo_output.is_symlink(), "拒绝既有Cargo目录")
        cargo_output.mkdir()
        for name, snapshot, expected in SOURCES:
            row = dict(path=name, snapshot=snapshot, before=file_identity(repo / name))
            identity["sources"].append(row)
            require(row["before"]["sha256"] == expected, "已审Rust source身份变化")
            with (repo / name).open("rb") as source, (evidence / snapshot).open("xb") as output:
                shutil.copyfileobj(source, output)
            require(file_identity(evidence / snapshot) == row["before"], "source snapshot不符")
        for name in CONFIGS:
            identity["configuration"].append(dict(path=name, before=file_identity(repo / name)))
        identity["oracle_archive"] = dict(path=ARCHIVE, before=file_identity(repo / ARCHIVE),
            blob_source="repository-tracked original-pixel.bin.gz; frozen original FRDPIX01 record165/176; no generator execution", uncompressed_sha256=RAW_SHA)
        require(identity["oracle_archive"]["before"]["sha256"] == ARCHIVE_SHA, "原gzip身份不符")
        env = os.environ.copy()
        if target == "i686-unknown-linux-gnu":
            env.update(CC_i686_unknown_linux_gnu="i686-linux-gnu-gcc", CXX_i686_unknown_linux_gnu="i686-linux-gnu-g++",
                       CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_LINKER="i686-linux-gnu-gcc")
        env["CARGO_TARGET_DIR"] = str(cargo_output)
        names = ("RUSTUP_TOOLCHAIN", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
                 "CARGO_BUILD_TARGET", "CARGO_TARGET_DIR", "CARGO_PROFILE_RELEASE_LTO", "CARGO_PROFILE_RELEASE_OPT_LEVEL",
                 "CARGO_PROFILE_RELEASE_PANIC", "CARGO_PROFILE_RELEASE_DEBUG", "CARGO_PROFILE_RELEASE_STRIP", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
                 "CARGO_TARGET_" + target.upper().replace("-", "_") + "_RUSTFLAGS",
                 "CARGO_TARGET_" + target.upper().replace("-", "_") + "_LINKER", "CC_i686_unknown_linux_gnu", "CXX_i686_unknown_linux_gnu")
        identity["compilation_environment"] = {name: env.get(name) for name in names}
        for name in names:
            if name in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS") or name.startswith("CARGO_PROFILE_") or name.endswith("_RUSTFLAGS"):
                require(not env.get(name), "拒绝未审编译/CPU/profile覆盖: " + name)
        for name in ("cargo", "rustc"):
            path = shutil.which(name)
            require(path is not None, "缺少固定Rust工具")
            path = str(Path(path).resolve())
            identity["tools"].append(dict(command=name, path=path, before=file_identity(Path(path)), scope="resolved command may be rustup shim; preceding step checks 1.96.0"))
        cargo = str(Path(shutil.which("cargo")).absolute())
        built_map = cargo_output / (EXAMPLE + ".map")
        unchanged("before_build")
        built_record = capture("build", build_command(cargo, target, built_map, target_os), repo, env, evidence)
        identity["stages"].append(built_record)
        if built_record["failure_exit"]:
            fail("build: " + built_record["first_failure"], built_record["failure_exit"])
        if identity["failure_exit"] == 0:
            invocation = parse_rustc((evidence / "build.stderr").read_bytes(), target, built_map, target_os)
            identity["actual_rustc"] = invocation
            identity["tools"].append(dict(command="actual rustc -v", path=invocation["compiler_path"], before=invocation["compiler_identity"]))
            name = EXAMPLE + (".exe" if target_os == "windows" else "")
            built = cargo_output / target / "release" / "examples" / name
            for kind, source, dest in (("binary", built, name), ("link_map", built_map, EXAMPLE + ".map")):
                row = dict(kind=kind, build_path=str(source), path=dest, build=file_identity(source))
                identity["artifacts"].append(row)
                require(0 < row["build"]["bytes"] <= 64 * 1024 * 1024, "图像/map长度不符")
                with source.open("rb") as raw, (evidence / dest).open("xb") as output:
                    shutil.copyfileobj(raw, output)
                if kind == "binary":
                    shutil.copymode(source, evidence / dest)
                    row["header"] = check_binary(evidence / dest, target_os, bits)
                    row["staged_mode"] = oct((evidence / dest).stat().st_mode & 0o777)
                    require(target_os != "linux" or (evidence / dest).stat().st_mode & stat.S_IXUSR, "staged ELF无执行位")
                row["staged"] = file_identity(evidence / dest)
                require(row["build"] == row["staged"], "staged副本不是唯一build原件")
            unchanged("after_build")
            identity["build_complete"] = True
            save()
            unchanged("before_benchmark")
            runtime = capture("benchmark", [str(evidence / name)], repo, env, evidence)
            identity["stages"].append(runtime)
            if runtime["failure_exit"]:
                fail("benchmark: " + runtime["first_failure"], runtime["failure_exit"])
            if runtime["failure_exit"] == 0:
                identity["parsed"] = parse_benchmark((evidence / "benchmark.stdout").read_bytes(), (evidence / "benchmark.stderr").read_bytes(), runtime["exit_code"], selector)
            unchanged("after_benchmark")
    except Exception as error:
        fail(type(error).__name__ + ": " + str(error))
    finally:
        try:
            # 若早期失败，仍逐已记录的成员检查；oracle未建立时不伪造after。
            if "oracle_archive" in identity:
                unchanged("final")
        except Exception as error:
            identity["finalization_error"] = type(error).__name__ + ": " + str(error)
            fail(identity["finalization_error"])
        for stage in ("build", "benchmark"):
            for stream in ("stdout", "stderr"):
                path = evidence / (stage + "." + stream)
                if not path.exists():
                    try:
                        path.open("xb").close()  # identity stages区分NOT_RUN与实际空流。
                    except OSError as error:
                        fail("NOT_RUN raw finalization: " + str(error))
        identity["runtime_complete"] = bool(identity["failure_exit"] == 0 and identity["build_complete"] and identity.get("parsed", {}).get("complete") and len(identity["stages"]) == 2)
        if not identity["runtime_complete"]:
            fail("benchmark remained NOT_RUN/partial")
        # 只给已关闭、完整捕获的raw流作最终身份；未知writer明确保留partial。
        for stage in identity["stages"]:
            if stage["capture_complete"]:
                try:
                    stage["raw_identity"] = {stream: file_identity(evidence / (stage["stage"] + "." + stream)) for stream in ("stdout", "stderr")}
                except Exception as error:
                    fail("raw finalization: " + str(error))
                    identity["runtime_complete"] = False
        try:
            save()
        except Exception as error:
            fail("identity finalization: " + str(error))
            identity["runtime_complete"] = False
            print("partial identity finalization", file=sys.stderr)
    return identity["failure_exit"]


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in TARGETS:
        print("仅接受固定目标: " + ", ".join(TARGETS), file=sys.stderr)
        return 2
    try:
        return run(sys.argv[1])
    except Exception as error:
        # target不可写/非目录或已有evidence时不能安全覆盖它来伪造新identity。
        print("setup/evidence I/O failed; no complete identity: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
