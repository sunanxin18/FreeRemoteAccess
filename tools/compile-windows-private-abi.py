#!/usr/bin/env python3
"""私有 Windows ARM64 source/object 探针；绝不授予 native/SEH 资格。"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import tarfile
import time
import urllib.request
import urllib.parse
import zipfile

SCHEMA = "freeremotedesk.windows.private-compile.v1"
TARGET = "aarch64-w64-mingw32"
TOOL_ROOT = Path("/opt/frd-llvm-mingw-20250709")
CC = TOOL_ROOT / "bin/aarch64-w64-mingw32-clang"
READOBJ = TOOL_ROOT / "bin/llvm-readobj"
OBJDUMP = TOOL_ROOT / "bin/llvm-objdump"
ARCHIVE_NAME = "llvm-mingw-20250709-msvcrt-ubuntu-22.04-x86_64.tar.xz"
ARCHIVE_URL = "https://github.com/mstorsjo/llvm-mingw/releases/download/20250709/" + ARCHIVE_NAME
ARCHIVE_SHA = "422f13915a41acd8b93566e590531f55d3de6e531575197d51756ff8e8d16525"
FIXTURE = "third_party/ffmpeg/8.1.2/tests"
AUTHORIZATION_PROVENANCE = {
    "source_commit": "f475cf28ec6fcc65fd8c17984ee88b23a4a16fcd",
    "root_manifest_sha256": "2b4b20775ce15495bcce03cf930bfa11d71f3f0d95c858d809384c3a0342d667",
    "scope": "root independently approved embedded 12-file pins; local planning manifest is provenance only, not a CI runtime dependency",
}
# Root-reviewed B2A fix1/report + B2B review package; not discovered authorization.
# Root clarified the inventory to 7 B2A + frozen shared header + 3 B2B + exact diagnostic header = 12.
SOURCE_PINS = {
    "h264-luma-dc-windows-diagnostics.h": "c3b4186d9b582ad29dce667f00974cb8364c998f3363828210afef181c62c13b",
    "h264-luma-dc-platform-windows.c": "29ec138f105dbfcf705239505eba2a7ba22bb27888d1ce2a4f9f33c24ec0836d",
    "h264-luma-dc-clock-convert.h": "235d633d2e99d18c438e142d96b9d797c16e14fd6bdc0da8f644231a3fc7647a",
    "h264-luma-dc-platform-windows-tests.c": "59e87c8c2f37ee09f9b1bba3985defaad0fbb67a9ed107703747639ae2f32993",
    "README-windows-platform.md": "ebb1110f8636f0f4f76bb3753e2f5788f6764fef6c8f0af6341411d16f510a9d",
    "h264-luma-dc-clock-tests.c": "0d69b13ade81662c0fb0ebd58801ef90a82f3d21510b6812d533fe4c73c8ea76",
    "h264-luma-dc-clock-oracle.py": "2eca0534ddddc1b69f969524a3a46662e8a796390f5fafd632c8552e7a0465df",
    "h264-luma-dc-windows-clock-record.h": "e92e7c22bb0d5dd6cc2fe18cf14c3759ecb2da9b13159909bb45c99503aba370",
    "h264-luma-dc-platform.h": "2afd1fdc3126aae4bc0f7f86bf73cedabe6c821a88c0a8851ba087070547981e",
    "h264-luma-dc-abi-windows.S": "8998d8b6b5a83aa31625439e33e46355da64c4126dac65bc75f0e537d9d777d0",
    "h264-luma-dc-abi-windows-negative.S": "f6ec49f8c2e8d9f202581463a5e16df7f7d2e380a02d3443457c1b801d6d4bbf",
    "README-windows-abi.md": "7f556bc381df052f8d2af373b90d44e97c58e5019aeb0bf81cada9faa1b99110",
}
OWN_INPUTS = (
    "tools/compile-windows-private-abi.py",
    "tools/tests/test_compile_windows_private_abi.py",
    ".github/workflows/qualify-windows-private-abi.yml",
)
CASES = ("platform", "abi", "negative") + tuple("bad-unwind-%d" % n for n in range(1, 6))
MAX_OBJECT = 64 * 1024 * 1024


class GateError(Exception):
    pass


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def json_write(path, value):
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")


def ordinary(path):
    """拒绝源/输出 reparse、相对路径和不规范路径；工具 symlink 另处理。"""
    if not path.is_absolute() or path != path.resolve(strict=True) or not path.is_file():
        raise GateError("非普通精确文件路径: %s" % path)


def source_snapshot(root):
    files = {}
    expected_headers = {"h264-luma-dc-platform.h", "h264-luma-dc-clock-convert.h", "h264-luma-dc-windows-clock-record.h", "h264-luma-dc-windows-diagnostics.h"}
    # 唯一 -I 目录不允许 shadow SDK/CRT headers 或额外 header 子目录。
    fixture = root / FIXTURE
    if any(path.is_dir() or (path.suffix.lower() == ".h" and path.name not in expected_headers)
           for path in fixture.iterdir()):
        raise GateError("fixture include directory 含未授权 header/子目录")
    for name, pin in SOURCE_PINS.items():
        path = root / FIXTURE / name
        ordinary(path)
        actual = digest(path)
        if actual != pin:
            raise GateError("受信 source hash 不匹配: %s" % name)
        files[FIXTURE + "/" + name] = {"path": str(path), "sha256": actual}
    for name in OWN_INPUTS:
        path = root / name
        ordinary(path)
        files[name] = {"path": str(path), "sha256": digest(path)}
    return files


def paths(root_text, output_text):
    root, output = Path(root_text), Path(output_text)
    if not root.is_absolute() or root != root.resolve(strict=True) or not root.is_dir():
        raise GateError("source-root 必须是精确绝对 checkout 路径")
    if Path(__file__).resolve() != root / OWN_INPUTS[0]:
        raise GateError("source-root 与执行中的 checkout script 不匹配")
    if (not output.is_absolute() or output.parent != root / "target" or
            not re.fullmatch(r"task4c5c3b2c-[a-z0-9-]+", output.name)):
        raise GateError("output 必须是 checkout/target/task4c5c3b2c-* 新目录")
    output.parent.mkdir(exist_ok=True)
    if output.parent != output.parent.resolve(strict=True):
        raise GateError("output parent 不得是 symlink")
    output.mkdir()  # 已存在即失败；绝不复用 cache/receipts。
    return root, output


def tool_snapshot():
    """完整 extracted inventory；不是仅 entrypoint hash，也不包括 host loader。"""
    if not TOOL_ROOT.is_dir() or TOOL_ROOT != TOOL_ROOT.resolve(strict=True):
        raise GateError("固定工具目录缺失或非精确路径")
    files = {}
    for path in sorted(TOOL_ROOT.rglob("*")):
        name = path.relative_to(TOOL_ROOT).as_posix()
        if path.is_symlink():
            resolved = path.resolve(strict=True)
            if not resolved.is_relative_to(TOOL_ROOT):
                raise GateError("工具 symlink 越界: %s" % name)
            files[name] = {"kind": "symlink", "target": os.readlink(path)}
        elif path.is_file():
            files[name] = {"kind": "file", "sha256": digest(path), "size": path.stat().st_size}
        elif not path.is_dir():
            raise GateError("工具特殊文件: %s" % name)
    for path in (CC, READOBJ, OBJDUMP):
        if not path.is_file() or not os.access(path, os.X_OK):
            raise GateError("固定工具 entrypoint 缺失: %s" % path)
    return files


def host_gate():
    # 仅 installer/build host；不等于 Windows target OS/process 证据。
    if (os.environ.get("GITHUB_ACTIONS") != "true" or
            os.environ.get("RUNNER_OS") != "Linux" or
            platform.system() != "Linux" or platform.machine() != "x86_64"):
        raise GateError("仅允许显式 GitHub Actions Linux x86_64 build host")
    if not re.fullmatch(r"[0-9a-f]{40}", os.environ.get("GITHUB_SHA", "")):
        raise GateError("缺少精确 dispatched GITHUB_SHA")
    event = os.environ.get("GITHUB_EVENT_NAME")
    if event != "workflow_dispatch" and not (
            event == "push" and os.environ.get("GITHUB_REF") == "refs/heads/codex/rdp-egfx-hevc-preference"):
        raise GateError("仅接受 root 放行后的窄 workflow-file push 或显式 dispatch")


def archive_inventory(archive):
    """受信 archive bytes 独立生成 expected extracted identities，receipt 不授权工具。"""
    if digest(archive) != ARCHIVE_SHA:
        raise GateError("固定 archive SHA256 不匹配")
    inventory = {}
    top = ARCHIVE_NAME.removesuffix(".tar.xz")
    with tarfile.open(archive, "r:xz") as tar:
        for member in tar:
            parts = Path(member.name).parts
            if not parts or parts[0] != top or ".." in parts or Path(member.name).is_absolute():
                raise GateError("archive member 越界")
            relative = Path(*parts[1:]).as_posix()
            if relative == "." or member.isdir():
                continue
            if relative in inventory:
                raise GateError("archive duplicate file member")
            if member.isfile():
                with tar.extractfile(member) as stream:
                    hashed = hashlib.file_digest(stream, "sha256").hexdigest()
                inventory[relative] = {"kind": "file", "sha256": hashed, "size": member.size}
            elif member.issym():
                inventory[relative] = {"kind": "symlink", "target": member.linkname}
            else:
                raise GateError("archive 特殊/hardlink member 被拒绝")
    return inventory


def install(root, out, initial):
    """只有 remote CI 调用；固定 URL/hash，extract 的所有成员受 data filter。"""
    host_gate()
    if TOOL_ROOT.exists():
        raise GateError("固定安装目录已存在；不复用工具缓存")
    archive = out / ARCHIVE_NAME
    with urllib.request.urlopen(ARCHIVE_URL, timeout=120) as response, archive.open("xb") as stream:
        if urllib.parse.urlsplit(response.url).scheme != "https":
            raise GateError("archive redirect 必须保持 HTTPS")
        shutil.copyfileobj(response, stream)
    if digest(archive) != ARCHIVE_SHA:
        raise GateError("固定 archive SHA256 不匹配；保留下载")
    TOOL_ROOT.mkdir()
    top = ARCHIVE_NAME.removesuffix(".tar.xz")
    with tarfile.open(archive, "r:xz") as tar:
        # 固定已验证 archive；仍拒绝特殊/越界/重复成员。
        names = set()
        for member in tar.getmembers():
            parts = Path(member.name).parts
            if not parts or parts[0] != top or ".." in parts or Path(member.name).is_absolute():
                raise GateError("archive member 越界")
            relative = Path(*parts[1:]).as_posix()
            if relative == ".":
                if not member.isdir():
                    raise GateError("archive root 类型错误")
                continue
            if relative in names:
                raise GateError("archive duplicate member")
            names.add(relative)
            if not (member.isfile() or member.isdir() or member.issym()):
                raise GateError("archive 特殊/hardlink member 被拒绝")
            member.name = relative
            tar.extract(member, TOOL_ROOT, filter="data")
    inventory = archive_inventory(archive)
    if tool_snapshot() != inventory:
        raise GateError("extracted files 不匹配固定 archive 的实际 bytes")
    if source_snapshot(root) != initial:
        raise GateError("安装期间 source snapshot 改变")
    json_write(out / "extracted-inventory.json", inventory)
    json_write(out / "installation.json", {
        "schema": SCHEMA + ".installation", "archive_url": ARCHIVE_URL,
        "archive_sha256": ARCHIVE_SHA, "tool_root": str(TOOL_ROOT),
        "inventory_sha256": digest(out / "extracted-inventory.json"),
        "scope": "pinned extracted files/symlinks; host dynamic loader and OS libraries outside inventory",
    })
    commands = allowed_commands(root, out)

    def validator():
        if source_snapshot(root) != initial or tool_snapshot() != inventory:
            raise GateError("installer version probe 输入改变")

    for label in ("cc-version", "readobj-version", "objdump-version"):
        require_zero(terminal(out, label, commands[label], cwd=root, validator=validator, expected=commands))


def compile_argv(root, out, case):
    if case not in CASES:
        raise GateError("未知固定 compile case")
    fixture = root / FIXTURE
    base = [str(CC), "--target=" + TARGET, "-I", str(fixture)]
    if case == "platform":
        flags = ["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-D_WIN32_WINNT=0x0A00"]
        source = "h264-luma-dc-platform-windows.c"
    else:
        flags = ["-O3"]
        source = "h264-luma-dc-abi-windows-negative.S" if case == "negative" else "h264-luma-dc-abi-windows.S"
        # 五份 private C/PE helper 均显式启用 C0；generic 另行保持 macro-off。
        flags += ["-DFRD_DC_PLATFORM_CONTRACT=1", "-DFRD_DC_WINDOWS_ABI_CONTRACT=1"]
        if case.startswith("bad-unwind-"):
            flags += ["-DFRD_DC_WINDOWS_ABI_BAD_UNWIND=" + case.rsplit("-", 1)[1]]
    return base + flags + ["-c", str(fixture / source), "-o", str(out / (case + ".obj"))]


def allowed_commands(root, out):
    """所有可执行 argv 都由本模块生成；不接受 receipt/任意 flags。"""
    commands = {"cc-version": [str(CC), "--version"],
                "readobj-version": [str(READOBJ), "--version"],
                "objdump-version": [str(OBJDUMP), "--version"],
                "macros": [str(CC), "--target=" + TARGET, "-dM", "-E", "-x", "c", "-"]}
    for case in CASES:
        argv = compile_argv(root, out, case)
        commands[case + "-expansion"] = argv[:1] + ["-###"] + argv[1:]
        commands[case + "-compile"] = argv
        obj = str(out / (case + ".obj"))
        commands[case + "-readobj"] = [str(READOBJ), "--file-headers", "--sections", "--symbols", "--relocations", "--unwind", obj]
        commands[case + "-objdump"] = [str(OBJDUMP), "--disassemble-all", "--reloc", obj]
    return commands


def terminal(out, label, argv, *, cwd, validator, expected, stdin=None):
    """先保存 wait 终态，再处理 exit；快进程不查询 session ID。"""
    if label not in expected or argv != expected[label]:
        raise GateError("非白名单 argv/path/flags")
    validator()
    started, clock = utc(), time.monotonic_ns()
    record = {"argv": argv, "cwd": str(cwd), "started_utc": started, "pid": None,
              "wait_terminal": "not-started", "returncode": None, "failure": None}
    stdout, stderr = out / (label + ".stdout"), out / (label + ".stderr")
    env = {"PATH": str(TOOL_ROOT / "bin") + ":/usr/bin:/bin", "LC_ALL": "C", "LANG": "C", "HOME": "/tmp"}
    record["environment"] = env
    record["stdin_sha256"] = hashlib.sha256(stdin).hexdigest() if stdin is not None else None
    with stdout.open("xb") as sout, stderr.open("xb") as serr:
        try:
            process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                                       stdout=sout, stderr=serr, start_new_session=True)
            record["pid"] = process.pid
            try:
                process.communicate(input=stdin, timeout=120)
                record["wait_terminal"] = "signaled" if process.returncode < 0 else "exited"
            except subprocess.TimeoutExpired:
                import signal
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()
                record["wait_terminal"] = "timeout-killed"
            record["returncode"] = process.returncode
        except OSError as error:
            record["failure"] = {"type": type(error).__name__, "errno": error.errno, "message": str(error)}
    record.update({"ended_utc": utc(), "elapsed_ns": time.monotonic_ns() - clock,
                   "stdout": {"name": stdout.name, "sha256": digest(stdout)},
                   "stderr": {"name": stderr.name, "sha256": digest(stderr)}})
    json_write(out / (label + ".receipt.json"), record)
    validator()  # receipt 已保存，post-use mutation 也不能吞真实 terminal。
    return record


def require_zero(record):
    if record["wait_terminal"] != "exited" or record["returncode"] != 0:
        raise GateError("command 未正常 exit0: %s" % record["argv"])


def coff_admit(path):
    """仅 regular ARM64 COFF 容器边界，不解释 unwind/函数范围。"""
    ordinary(path)
    size = path.stat().st_size
    if size < 20 or size > MAX_OBJECT:
        raise GateError("COFF object 尺寸越界/截断")
    data = path.read_bytes()
    machine, count, timestamp, sympos, symbols, optional, flags = struct.unpack_from("<HHIIIHH", data)
    if machine != 0xAA64 or count == 0 or optional != 0 or flags & 2:
        raise GateError("非 regular ARM64 COFF object (PE/ELF/AMD64/ARM64EC 禁止)")
    end = 20 + count * 40
    if end > size:
        raise GateError("COFF section table 截断")
    for n in range(count):
        _, _, _, rawsize, rawpos, relocpos, linepos, relocs, lines, characteristics = struct.unpack_from("<8sIIIIIIHHI", data, 20 + n * 40)
        # COFF uninitialized-data sections have no file payload (e.g. platform clock .bss).
        stored_size = 0 if rawpos == 0 and characteristics & 0x80 else rawsize
        for offset, length in ((rawpos, stored_size), (relocpos, relocs * 10), (linepos, lines * 6)):
            if length and (offset < end or offset > size or length > size - offset):
                raise GateError("COFF section 数据/relocation 截断")
        if characteristics & 0x01000000:
            raise GateError("此窄探针不接受 extended relocation container")
    if symbols:
        table_end = sympos + symbols * 18
        if sympos < end or table_end + 4 > size:
            raise GateError("COFF symbol/string table 截断")
        strings = struct.unpack_from("<I", data, table_end)[0]
        if strings < 4 or strings > size - table_end:
            raise GateError("COFF string table 越界")
    elif sympos:
        raise GateError("COFF empty symbol table pointer 不明确")
    return {"path": str(path), "sha256": hashlib.sha256(data).hexdigest(), "size": size,
            "machine": "0xAA64", "section_count": count, "scope": "regular COFF container only"}


def readobj_agrees(text, exact_path):
    files = re.findall(r"^File: (.+)$", text, re.MULTILINE)
    machines = re.findall(r"^\s*Machine: (.+)$", text, re.MULTILINE)
    if (files != [str(exact_path)] or machines != ["IMAGE_FILE_MACHINE_ARM64 (0xAA64)"] or
            re.findall(r"^Format: (.+)$", text, re.MULTILINE) != ["COFF-ARM64"]):
        raise GateError("实际 readobj 缺少 exact 文件/ARM64 target agreement")


def macro_gate(text):
    macros = dict(re.findall(r"^#define (\w+)(?: (.*))?$", text, re.MULTILINE))
    if (macros.get("_WIN32") != "1" or macros.get("__aarch64__") != "1" or
            macros.get("__SIZEOF_POINTER__") != "8" or
            any(name in macros for name in ("_M_ARM64EC", "__arm64ec__", "__ARM64EC__"))):
        raise GateError("实际 target 预定义宏不匹配 Windows ARM64 pointer64")


def candidate_outcome(record, stderr, case):
    if record["wait_terminal"] == "exited" and record["returncode"] == 0:
        return "compiled"
    # 默认 unknown nonzero 为 error；不能把 driver/I/O/internal/resource fault 当源拒绝。
    # 唯一候选类别来自 LLVM20.1.8 MCWin64EH.cpp checkARM64Instructions：
    # https://github.com/llvm/llvm-project/blob/llvmorg-20.1.8/llvm/lib/MC/MCWin64EH.cpp#L280-L314
    # 只允许 BAD_UNWIND=5 的这个函数 epilogue-size diagnostic；实际版本不同则先保存 failed。
    if (case == "bad-unwind-5" and record["wait_terminal"] == "exited" and
            record["returncode"] == 1 and record.get("failure") is None):
        lines = [line for line in stderr.splitlines() if line.strip()]
        if len(lines) == 1:
            diagnostic = re.fullmatch(
                r"(?:<unknown>:0: )?error: Incorrect size for frd_h264_dc_check_abi epilogue: "
                r"([0-9]+) bytes of instructions in range, but \.seh directives corresponding to ([0-9]+) bytes",
                lines[0],
            )
            if diagnostic and int(diagnostic[1]) != int(diagnostic[2]):
                return "rejected"
    return "error"


def probe(root, out, initial, installation_dir, summary):
    host_gate()
    ordinary(installation_dir / "installation.json")
    ordinary(installation_dir / "extracted-inventory.json")
    ordinary(installation_dir / ARCHIVE_NAME)
    receipt = json.loads((installation_dir / "installation.json").read_text())
    keys = {"schema", "archive_url", "archive_sha256", "tool_root", "inventory_sha256", "scope"}
    if (set(receipt) != keys or receipt["schema"] != SCHEMA + ".installation" or
            receipt["archive_url"] != ARCHIVE_URL or receipt["archive_sha256"] != ARCHIVE_SHA or
            receipt["tool_root"] != str(TOOL_ROOT) or
            receipt["inventory_sha256"] != digest(installation_dir / "extracted-inventory.json") or
            digest(installation_dir / ARCHIVE_NAME) != ARCHIVE_SHA):
        raise GateError("installation receipt/archive 不匹配固定 trust anchor")
    inventory = archive_inventory(installation_dir / ARCHIVE_NAME)
    # 授权来自独立固定 archive pin，不能通过同时替换 tool+自写 receipt 通过。
    if (json.loads((installation_dir / "extracted-inventory.json").read_text()) != inventory or
            tool_snapshot() != inventory):
        raise GateError("实际 extracted inventory 不匹配")
    for name in ("installation.json", "extracted-inventory.json"):
        shutil.copyfile(installation_dir / name, out / name)
    commands = allowed_commands(root, out)
    json_write(out / "allowed-commands.json", commands)
    produced = {}

    def validator():
        if source_snapshot(root) != initial or tool_snapshot() != inventory:
            raise GateError("source/tool input 在使用前后改变")
        for path, expected_hash in produced.items():
            ordinary(path)
            if digest(path) != expected_hash:
                raise GateError("实际 object 在 raw inspection 使用前后改变: %s" % path)

    def run(label, stdin=None):
        return terminal(out, label, commands[label], cwd=root, validator=validator, expected=commands, stdin=stdin)

    for label in ("cc-version", "readobj-version", "objdump-version", "macros"):
        require_zero(run(label, stdin=b"" if label == "macros" else None))
    macro_gate((out / "macros.stdout").read_text())
    summary["macro_scope"] = "actual cross compiler target macros; not native host/process evidence"
    summary["tools"] = [{"entrypoint": str(p), "resolved": str(p.resolve()), "entry_sha256": digest(p)} for p in (CC, READOBJ, OBJDUMP)]
    for case in CASES:
        summary["not_attempted"].remove(case)
        item = {"case": case, "mandatory": case in CASES[:3], "outcome": "error", "object": None, "failure": None}
        summary["objects"].append(item)
        try:
            require_zero(run(case + "-expansion"))
            result = run(case + "-compile")
            diagnostic = (out / (case + "-compile.stderr")).read_text(errors="replace")
            item["outcome"] = candidate_outcome(result, diagnostic, case)
            if item["outcome"] == "rejected":
                item["rejection_category"] = "llvm-mc-helper-epilogue-size-mismatch"
            item["compile_terminal"] = {key: result[key] for key in ("pid", "wait_terminal", "returncode", "failure")}
            item["compile_receipt"] = case + "-compile.receipt.json"
            if result["wait_terminal"] != "exited" or result["returncode"] != 0:
                summary.setdefault("first_compiler_failure", {
                    "case": case, "receipt": item["compile_receipt"], "terminal": item["compile_terminal"],
                    "first_error_line": next((line for line in diagnostic.splitlines() if "error:" in line), None),
                    "scope": "actual compiler failure/rejection; never native control success",
                })
            obj = out / (case + ".obj")
            # 保存任何实际产物的原始工具输出，即使 compiler 返回失败或 COFF 不准入。
            if obj.exists():
                ordinary(obj)
                produced[obj] = digest(obj)
                item["object"] = {"path": str(obj), "sha256": produced[obj], "size": obj.stat().st_size}
                read_result = run(case + "-readobj")
                dump_result = run(case + "-objdump")
                item["container"] = coff_admit(obj)
                require_zero(read_result)
                require_zero(dump_result)
                readobj_agrees((out / (case + "-readobj.stdout")).read_text(), obj)
            elif item["outcome"] == "compiled":
                raise GateError("compiler exit0 却缺实际 object")
            if item["mandatory"]:
                require_zero(result)
            elif item["outcome"] == "error":
                raise GateError("candidate 没有可归类的正常 compiler rejection")
        except (GateError, OSError) as error:
            item["failure"] = {"type": type(error).__name__, "message": str(error)}
            summary["failures"].append({"case": case, **item["failure"]})
            # 第一真实 mandatory failure 停止；不重试/换 flags，不伪造剩余结果。
            if item["mandatory"]:
                raise
    validator()
    if any(x["failure"] for x in summary["objects"]):
        raise GateError("object admission/candidate infrastructure 有失败")
    summary["status"] = "compiled-only"


def archive_evidence(out):
    """严格成员/hash list 不自引用；ZIP/manifest 的外部 SHA 用 archive.json 绑定。"""
    permitted = {"summary.json", "source-inputs.json", "source-authorization.json", "installation.json", "extracted-inventory.json", "allowed-commands.json", ARCHIVE_NAME}
    permitted.update("inputs/" + name for name in (
        [FIXTURE + "/" + name for name in SOURCE_PINS] + list(OWN_INPUTS)))
    labels = list(allowed_commands(Path("/source"), Path("/output")))
    permitted.update(label + suffix for label in labels for suffix in (".stdout", ".stderr", ".receipt.json"))
    permitted.update(case + ".obj" for case in CASES)
    members = {}
    for path in sorted(out.rglob("*")):
        if path.is_file():
            ordinary(path)
            name = path.relative_to(out).as_posix()
            if name not in permitted:
                raise GateError("未授权 evidence member: %s" % name)
            members[name] = digest(path)
        elif path.is_symlink():
            raise GateError("evidence symlink 被拒绝")
    json_write(out / "members.json", {"schema": SCHEMA + ".members", "files": members})
    with zipfile.ZipFile(out / "evidence.zip", "x", compression=zipfile.ZIP_DEFLATED) as archive:
        for name in sorted(members):
            archive.write(out / name, name)
        archive.write(out / "members.json", "members.json")
    json_write(out / "archive.json", {"schema": SCHEMA + ".archive", "sha256": digest(out / "evidence.zip"),
                                    "members_sha256": digest(out / "members.json"),
                                    "strict_members": sorted(members) + ["members.json"]})


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("mode", choices=("install", "probe"))
    parser.add_argument("--source-root", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--installation")
    args = parser.parse_args(argv)
    root, out = paths(args.source_root, args.output)
    summary = {"schema": SCHEMA, "status": "failed", "scope": "compile/object admission only; native/SEH/parity/benchmark/package remain OPEN",
               "mode": args.mode, "started_utc": utc(), "source_root": str(root), "build_target": TARGET,
               "host": {"system": platform.system(), "machine": platform.machine(), "node": platform.node()},
               "github": {key: os.environ.get(key) for key in ("GITHUB_SHA", "GITHUB_REF", "GITHUB_EVENT_NAME", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "RUNNER_OS", "RUNNER_ARCH")},
               "objects": [], "failures": []}
    summary["not_attempted"] = list(CASES) if args.mode == "probe" else []
    try:
        initial = source_snapshot(root)
        json_write(out / "source-inputs.json", initial)
        json_write(out / "source-authorization.json", {**AUTHORIZATION_PROVENANCE, "reviewed_source_pins": SOURCE_PINS})
        sources = out / "inputs"
        for name in initial:
            dest = sources / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(root / name, dest)
            if digest(dest) != initial[name]["sha256"]:
                raise GateError("归档 source input copy hash 改变")
        if args.mode == "install":
            if args.installation is not None:
                raise GateError("install 不接受 installation 参数")
            install(root, out, initial)
            summary["status"] = "installed-only"
        else:
            if args.installation is None:
                raise GateError("probe 需要本次固定 installer output")
            installation = Path(args.installation)
            if (installation.parent != root / "target" or not installation.is_absolute() or
                    installation != installation.resolve(strict=True) or
                    not re.fullmatch(r"task4c5c3b2c-[a-z0-9-]+", installation.name)):
                raise GateError("installation 必须是精确本次 target/task4c5c3b2c-* 目录")
            probe(root, out, initial, installation, summary)
        if source_snapshot(root) != initial:
            raise GateError("最终 source inputs 改变")
    except Exception as error:
        summary["status"] = "failed"
        summary["failures"].append({"type": type(error).__name__, "message": str(error)})
    summary["ended_utc"] = utc()
    json_write(out / "summary.json", summary)
    archive_evidence(out)
    print(json.dumps({"status": summary["status"], "output": str(out), "archive_sha256": digest(out / "evidence.zip")}))
    return 1 if summary["status"] == "failed" else 0


if __name__ == "__main__":
    raise SystemExit(main())
