#!/usr/bin/env python3
"""固定 Windows ARM64 私有 C/PE 门；只生成 compiled-only，不执行 Windows 程序。"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import struct
import sys
import tarfile
import zipfile

SCHEMA = "freeremotedesk.windows.private-contract-compile.v1"
SUPPORT = "tools/compile-windows-private-abi.py"
SUPPORT_SHA = "8167c1d9413e58e6d1d948631863cc76c22b300ca7eb126b9807d1d5b0bbd3f5"
FIXTURE = "third_party/ffmpeg/8.1.2/tests"
PINS = {
    "h264-luma-dc-windows-contract.c": "e5ae7af8d9c7792bfecce5e876e92404331d6604820090dfc24212aecf7ccb6e",
    "README-windows-contract.md": "d67ea776ca6a5246f5ac0f8e0705d736e16bd5afdb7af5e7dc3f5bab4d3e44fb",
}
OWN = ("tools/compile-windows-contract.py", "tools/tests/test_compile_windows_contract.py")
VARIANTS = tuple(range(5))
OBJECTS = ("platform", "abi", "negative") + tuple("bad-unwind-%d" % n for n in range(1, 5))
CRT = ("crt2.o", "crtbegin.o", "crtend.o", "libmingw32.a", "libmingwex.a",
       "libmsvcrt.a", "libkernel32.a", "libntdll.a")
FUNCTIONS = ("main", "capture_callback", "native_admission", "linked_admission",
             "child_main", "isolated_child", "isolated_suite")
LABELS = ("frd_h264_dc_check_abi", "frd_h264_dc_abi_pc_after_alloc",
          "frd_h264_dc_abi_pc_body", "frd_h264_dc_abi_pc_call",
          "frd_h264_dc_abi_pc_return", "frd_h264_dc_abi_pc_epilogue",
          "frd_h264_dc_abi_pc_epilogue_after_d14", "frd_h264_dc_abi_pc_ret",
          "frd_h264_dc_outer_probe", "frd_h264_dc_outer_pc_return",
          "frd_h264_dc_wrong_return")
MAX_FILE = 64 * 1024 * 1024
FIXED_MEMBERS = {"summary.json", "source-inputs.json", "source-inputs-after.json", "tool-inputs.json", "tool-inputs-after.json",
                 "same-run-inputs.json", "authorization.json", "dependency-inputs.json", "actual-output-identities.json",
                 "allowed-commands.json", "failure-output-identities.json"}
ENVELOPE_MEMBERS = {"members.json", "evidence.zip", "archive.json"}
TERMINAL_PRECOMMIT = "terminal-precommit.json"


class GateError(Exception):
    pass


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def ordinary(path):
    if (not path.is_absolute() or path != path.resolve(strict=True) or
            not stat.S_ISREG(path.lstat().st_mode)):
        raise GateError("必须为精确普通文件: %s" % path)


def directory(path):
    if (not path.is_absolute() or path != path.resolve(strict=True) or
            not stat.S_ISDIR(path.lstat().st_mode)):
        raise GateError("必须为精确普通目录: %s" % path)


def load_json(path):
    ordinary(path)
    if path.stat().st_size > MAX_FILE:
        raise GateError("JSON 超出窄门限制")

    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise GateError("JSON duplicate key")
            result[key] = value
        return result

    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique)


def write_json(path, value):
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")


def clean_member(name):
    parts = PurePosixPath(name).parts
    if (not name or "\\" in name or name.startswith("/") or
            any(part in ("", ".", "..") for part in name.split("/")) or
            PurePosixPath(name).as_posix() != name or not parts):
        raise GateError("archive member 非规范相对路径")
    return parts


def tree(path):
    directory(path)
    result = {}
    for file in sorted(path.rglob("*")):
        key = file.relative_to(path).as_posix()
        clean_member(key)
        mode = file.lstat().st_mode
        if stat.S_ISDIR(mode):
            directory(file)
            result[key] = {"kind": "directory"}
        elif stat.S_ISREG(mode):
            ordinary(file)
            result[key] = {"kind": "file", "size": file.stat().st_size, "sha256": sha(file)}
        else:
            raise GateError("evidence tree symlink/特殊文件")
    return result


def pinned_support(root):
    path = root / SUPPORT
    ordinary(path)
    if sha(path) != SUPPORT_SHA:
        raise GateError("旧 support script 不匹配 root 固定 SHA")
    spec = importlib.util.spec_from_file_location("frd_fixed_private_compile", path)
    module = importlib.util.module_from_spec(spec)
    previous = sys.dont_write_bytecode
    try:
        sys.dont_write_bytecode = True
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = previous
    if sha(path) != SUPPORT_SHA:
        raise GateError("support import 期间改变")
    return module


def source_snapshot(root, support):
    result = support.source_snapshot(root)
    for name, pin in PINS.items():
        path = root / FIXTURE / name
        ordinary(path)
        if sha(path) != pin:
            raise GateError("root 授权 C/README pin 改变: %s" % name)
        result[FIXTURE + "/" + name] = {"path": str(path), "sha256": pin}
    for name in OWN:
        path = root / name
        ordinary(path)
        result[name] = {"path": str(path), "sha256": sha(path)}
    if sha(root / SUPPORT) != SUPPORT_SHA:
        raise GateError("support pin drift")
    return result


def paths(root_text, input_text, install_text, out_text):
    root, source, installation, out = map(Path, (root_text, input_text, install_text, out_text))
    directory(root)
    if Path(__file__).resolve() != root / OWN[0]:
        raise GateError("执行 source-root 与本脚本不匹配")
    run, attempt = os.environ.get("GITHUB_RUN_ID", ""), os.environ.get("GITHUB_RUN_ATTEMPT", "")
    if not all(re.fullmatch(r"[1-9][0-9]*", value) for value in (run, attempt)):
        raise GateError("缺少精确 same-run ID/attempt")
    expected = (root / ("target/task4c5c3b2c-compile-%s-%s" % (run, attempt)),
                root / ("target/task4c5c3b2c-install-%s-%s" % (run, attempt)),
                root / ("target/task4c5c3b2e-contract-%s-%s" % (run, attempt)))
    if (source, installation, out) != expected:
        raise GateError("input/install/output 必须为本 run 固定互不相同目录")
    directory(root / "target")
    directory(source)
    directory(installation)
    out.mkdir()  # fresh only; 已存在、symlink 或 alias 都拒绝。
    directory(out)
    return root, source, installation, out


def archive_inventory(archive, support):
    """独立严格 tar 路径/type/inventory；不从 installer receipt 获得授权。"""
    ordinary(archive)
    if sha(archive) != support.ARCHIVE_SHA:
        raise GateError("固定 archive bytes 改变")
    top = support.ARCHIVE_NAME.removesuffix(".tar.xz")
    entries, seen = {}, set()
    with tarfile.open(archive, "r:xz") as tar:
        for member in tar:
            # tar directory 的一个 trailing slash 是格式语法，不能归一化其它 path。
            name = member.name[:-1] if member.isdir() and member.name.endswith("/") else member.name
            parts = clean_member(name)
            if parts[0] != top or name in seen:
                raise GateError("tar root/duplicate member")
            seen.add(name)
            if len(parts) == 1:
                if not member.isdir():
                    raise GateError("tar root 非目录")
                continue
            key = "/".join(parts[1:])
            if member.isdir():
                entries[key] = {"kind": "directory"}
            elif member.isfile():
                with tar.extractfile(member) as stream:
                    hashed = hashlib.file_digest(stream, "sha256").hexdigest()
                entries[key] = {"kind": "file", "sha256": hashed, "size": member.size}
            elif member.issym():
                if not member.linkname or PurePosixPath(member.linkname).is_absolute() or "\\" in member.linkname:
                    raise GateError("tar symlink 非相对工具内链接")
                entries[key] = {"kind": "symlink", "target": member.linkname}
            else:
                raise GateError("tar hardlink/特殊 member")
    # 所有祖先必须是目录；绝不允许 archive 通过 symlink 写入别处。
    for key in entries:
        for parent in PurePosixPath(key).parents:
            if str(parent) != "." and entries.get(str(parent), {}).get("kind") != "directory":
                raise GateError("tar ancestor 缺失/非目录")
    def link_target(key, visited):
        if key in visited or key not in entries:
            raise GateError("tar symlink cycle/missing target")
        info = entries[key]
        if info["kind"] != "symlink":
            return key
        parts = list(PurePosixPath(key).parent.parts)
        if parts == ["."]:
            parts = []
        for part in info["target"].split("/"):
            if part in ("", "."):
                continue
            if part == "..":
                if not parts:
                    raise GateError("tar symlink escape")
                parts.pop()
            else:
                parts.append(part)
        return link_target("/".join(parts), visited | {key})
    for key, info in entries.items():
        if info["kind"] == "symlink":
            link_target(key, set())
    return entries


def tools_match(expected, support):
    directory(support.TOOL_ROOT)
    actual = {}
    for path in sorted(support.TOOL_ROOT.rglob("*")):
        key = path.relative_to(support.TOOL_ROOT).as_posix()
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            resolved = path.resolve(strict=True)
            if not resolved.is_relative_to(support.TOOL_ROOT):
                raise GateError("tool symlink 越界")
            actual[key] = {"kind": "symlink", "target": os.readlink(path)}
        elif stat.S_ISDIR(mode):
            actual[key] = {"kind": "directory"}
        elif stat.S_ISREG(mode):
            actual[key] = {"kind": "file", "sha256": sha(path), "size": path.stat().st_size}
        else:
            raise GateError("tool 特殊文件")
    if actual != expected:
        raise GateError("工具完整 type/path/inventory 与固定 tar 不同")
    for path in (support.CC, support.READOBJ, support.OBJDUMP):
        if not path.is_file() or not os.access(path, os.X_OK):
            raise GateError("fixed executable entrypoint 缺失")
    return actual


def support_members(support):
    result = {"summary.json", "source-inputs.json", "source-authorization.json", "installation.json",
              "extracted-inventory.json", "allowed-commands.json", support.ARCHIVE_NAME}
    result.update("inputs/" + FIXTURE + "/" + name for name in support.SOURCE_PINS)
    result.update("inputs/" + name for name in support.OWN_INPUTS)
    result.update(label + suffix for label in support.allowed_commands(Path("/source"), Path("/output"))
                  for suffix in (".receipt.json", ".stdout", ".stderr"))
    result.update(case + ".obj" for case in support.CASES)
    return result


def verify_bundle(folder, allowed, schema, *, external=frozenset()):
    """ZIP/list/loose bytes 全部比较；records 是数据而非命令权限。"""
    snapshot = tree(folder)
    record = load_json(folder / "archive.json")
    manifest = load_json(folder / "members.json")
    if (set(record) != {"schema", "sha256", "members_sha256", "strict_members"} or
            record["schema"] != schema + ".archive" or
            set(manifest) != {"schema", "files"} or manifest["schema"] != schema + ".members" or
            record["sha256"] != sha(folder / "evidence.zip") or
            record["members_sha256"] != sha(folder / "members.json")):
        raise GateError("archive/manifest binding 不匹配")
    members = manifest["files"]
    if not isinstance(members, dict) or not set(members).issubset(allowed):
        raise GateError("未授权 archive files")
    expected = sorted(members) + ["members.json"]
    if record["strict_members"] != expected:
        raise GateError("strict_members 不一致")
    loose = {name for name, info in snapshot.items() if info["kind"] == "file"}
    if loose != set(expected) | {"archive.json", "evidence.zip"} | set(external):
        raise GateError("loose evidence extra/missing member")
    allowed_dirs = {str(parent) for name in loose for parent in PurePosixPath(name).parents if str(parent) != "."}
    if {name for name, info in snapshot.items() if info["kind"] == "directory"} != allowed_dirs:
        raise GateError("loose evidence extra/missing directory")
    with zipfile.ZipFile(folder / "evidence.zip") as archive:
        infos = archive.infolist()
        if [entry.filename for entry in infos] != expected:
            raise GateError("ZIP exact order/members/duplicates")
        for entry in infos:
            clean_member(entry.filename)
            filetype = stat.S_IFMT(entry.external_attr >> 16)
            if entry.is_dir() or entry.flag_bits & 1 or filetype not in (0, stat.S_IFREG):
                raise GateError("ZIP dir/encrypted/symlink/特殊 member")
            path = folder / entry.filename
            ordinary(path)
            if entry.file_size != path.stat().st_size:
                raise GateError("ZIP size mismatch")
            with archive.open(entry) as stream:
                hashed = hashlib.file_digest(stream, "sha256").hexdigest()
            if hashed != sha(path) or entry.filename != "members.json" and hashed != members[entry.filename]:
                raise GateError("ZIP/loose/manifest bytes mismatch")
    return snapshot


def receipt(folder, label, argv, root, support):
    value = load_json(folder / (label + ".receipt.json"))
    expected_env = {"PATH": str(support.TOOL_ROOT / "bin") + ":/usr/bin:/bin", "LC_ALL": "C", "LANG": "C", "HOME": "/tmp"}
    if value.get("argv") != argv or value.get("cwd") != str(root) or value.get("environment") != expected_env:
        raise GateError("same-run receipt 与固定源码命令不同")
    for stream in ("stdout", "stderr"):
        item = value.get(stream, {})
        if item != {"name": label + "." + stream, "sha256": sha(folder / (label + "." + stream))}:
            raise GateError("receipt raw stream identity")
    if not isinstance(value.get("pid"), int) or value["pid"] <= 0 or not isinstance(value.get("elapsed_ns"), int):
        raise GateError("receipt 缺少 actual PID/wait time")
    return value


def same_run(summary, mode, status, root, support):
    if (summary.get("schema") != support.SCHEMA or summary.get("mode") != mode or
            summary.get("status") != status or summary.get("source_root") != str(root) or
            summary.get("build_target") != support.TARGET or summary.get("failures") != [] or
            summary.get("host", {}).get("system") != "Linux" or
            summary.get("host", {}).get("machine") != "x86_64"):
        raise GateError("same-run summary 未成功或 host/target 不同")
    keys = ("GITHUB_SHA", "GITHUB_REF", "GITHUB_EVENT_NAME", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "RUNNER_OS", "RUNNER_ARCH")
    if summary.get("github") != {key: os.environ.get(key) for key in keys}:
        raise GateError("summary 非本 dispatched revision/run/attempt")


def actual_object(source, case, item, support):
    path = source / (case + ".obj")
    actual = support.coff_admit(path)
    identity = {key: actual[key] for key in ("path", "sha256", "size")}
    if item.get("outcome") != "compiled" or item.get("object") != identity:
        raise GateError("实际 COFF bytes 与 success identity 不同")
    return actual


def admit_inputs(root, source, installation, support):
    source_tree = verify_bundle(source, support_members(support), support.SCHEMA)
    install_tree = verify_bundle(installation, support_members(support), support.SCHEMA)
    same_run(load_json(source / "summary.json"), "probe", "compiled-only", root, support)
    same_run(load_json(installation / "summary.json"), "install", "installed-only", root, support)
    old_sources = support.source_snapshot(root)
    for folder in (source, installation):
        if load_json(folder / "source-inputs.json") != old_sources:
            raise GateError("same-run inputs 不匹配当前 source/tool/workflow")
        if load_json(folder / "source-authorization.json") != {**support.AUTHORIZATION_PROVENANCE, "reviewed_source_pins": support.SOURCE_PINS}:
            raise GateError("旧 11 pins/provenance 改变")
        for name, item in old_sources.items():
            if sha(folder / "inputs" / name) != item["sha256"]:
                raise GateError("archived source bytes 不匹配独立 snapshot")
    inventory = archive_inventory(installation / support.ARCHIVE_NAME, support)
    nondirs = {key: value for key, value in inventory.items() if value["kind"] != "directory"}
    tools_match(inventory, support)
    install_record = load_json(installation / "installation.json")
    expected = {"schema": support.SCHEMA + ".installation", "archive_url": support.ARCHIVE_URL,
                "archive_sha256": support.ARCHIVE_SHA, "tool_root": str(support.TOOL_ROOT),
                "inventory_sha256": sha(installation / "extracted-inventory.json"),
                "scope": "pinned extracted files/symlinks; host dynamic loader and OS libraries outside inventory"}
    if install_record != expected or load_json(installation / "extracted-inventory.json") != nondirs:
        raise GateError("installer identity 不匹配 actual archive")
    for name in ("installation.json", "extracted-inventory.json"):
        if sha(source / name) != sha(installation / name):
            raise GateError("object probe installer identity 不同")
    commands = support.allowed_commands(root, source)
    if load_json(source / "allowed-commands.json") != commands:
        raise GateError("receipt command list 不同；从不执行其内容")
    for label in ("cc-version", "readobj-version", "objdump-version"):
        support.require_zero(receipt(installation, label, support.allowed_commands(root, installation)[label], root, support))
    for label in ("cc-version", "readobj-version", "objdump-version", "macros"):
        support.require_zero(receipt(source, label, commands[label], root, support))
    support.macro_gate((source / "macros.stdout").read_text())
    summary = load_json(source / "summary.json")
    items = summary.get("objects")
    if not isinstance(items, list) or [item.get("case") for item in items] != list(support.CASES) or summary.get("not_attempted") != []:
        raise GateError("same-run 必须完成 exact8 cases/7 objects")
    objects = {}
    for item in items:
        case = item["case"]
        support.require_zero(receipt(source, case + "-expansion", commands[case + "-expansion"], root, support))
        terminal = receipt(source, case + "-compile", commands[case + "-compile"], root, support)
        if item.get("failure") is not None:
            raise GateError("input object admission 有失败")
        if case == "bad-unwind-5":
            if ((source / (case + ".obj")).exists() or item.get("object") is not None or
                    item.get("outcome") != "rejected" or
                    support.candidate_outcome(terminal, (source / (case + "-compile.stderr")).read_text(), case) != "rejected"):
                raise GateError("bad5 必须原样 MC-rejected/no object")
            continue
        support.require_zero(terminal)
        path = source / (case + ".obj")
        actual = actual_object(source, case, item, support)
        for suffix in ("-readobj", "-objdump"):
            support.require_zero(receipt(source, case + suffix, commands[case + suffix], root, support))
        support.readobj_agrees((source / (case + "-readobj.stdout")).read_text(), path)
        objects[case] = actual
    if tuple(objects) != OBJECTS:
        raise GateError("不是 exact7 admitted ARM64 COFF")
    return inventory, source_tree, install_tree, objects


def span(data, pos, length):
    if pos < 0 or length < 0 or pos > len(data) or length > len(data) - pos:
        raise GateError("binary table/section 截断")
    return data[pos:pos + length]


def coff_symbols(data, header, sections):
    pos, count = struct.unpack_from("<II", span(data, header + 8, 8))
    if not count:
        return {}
    end = pos + count * 18
    size = struct.unpack("<I", span(data, end, 4))[0]
    strings = span(data, end, size)
    if size < 4:
        raise GateError("COFF string table size")
    result, index = {}, 0
    while index < count:
        record = span(data, pos + index * 18, 18)
        name, value, section, typ, storage, aux = struct.unpack("<8sIhHBB", record)
        if index + aux >= count:
            raise GateError("COFF aux records 越界")
        if name[:4] == b"\0" * 4:
            offset = struct.unpack_from("<I", name, 4)[0]
            if offset < 4 or offset >= size or b"\0" not in strings[offset:]:
                raise GateError("COFF long symbol name 越界")
            name = strings[offset:].split(b"\0", 1)[0]
        else:
            name = name.split(b"\0", 1)[0]
        name = name.decode("ascii", errors="strict")
        if section > 0:
            if section > len(sections) or value > max(sections[section - 1]["virtual_size"], sections[section - 1]["raw_size"]):
                raise GateError("COFF symbol section/value 越界")
            if name in result and name in set(FUNCTIONS) | set(LABELS):
                raise GateError("owned symbol 重复")
            result[name] = {"value": value, "section": section, "type": typ, "storage": storage,
                            "rva": sections[section - 1]["rva"] + value}
        index += 1 + aux
    return result


def sections_at(data, pos, count):
    if count < 1 or count > 96:
        raise GateError("section count 越界")
    sections = []
    for index in range(count):
        values = struct.unpack("<8sIIIIIIHHI", span(data, pos + index * 40, 40))
        name, vs, rva, rawsize, rawpos, reloc, line, nr, nl, flags = values
        if rawpos:
            span(data, rawpos, rawsize)
        sections.append({"name": name.rstrip(b"\0").decode("ascii"), "virtual_size": vs,
                         "rva": rva, "raw_size": rawsize, "raw_offset": rawpos, "flags": flags})
    return sections


def pe_admit(path):
    ordinary(path)
    if path.stat().st_size > MAX_FILE:
        raise GateError("PE 太大")
    data = path.read_bytes()
    if span(data, 0, 2) != b"MZ":
        raise GateError("PE 缺少 MZ")
    pe = struct.unpack("<I", span(data, 60, 4))[0]
    if pe < 64 or span(data, pe, 4) != b"PE\0\0":
        raise GateError("PE signature/offset")
    header = pe + 4
    machine, count, timestamp, symbols, num, optsize, flags = struct.unpack("<HHIIIHH", span(data, header, 20))
    optional = span(data, header + 20, optsize)
    if (machine != 0xAA64 or optsize < 240 or struct.unpack_from("<H", optional)[0] != 0x20B or
            not flags & 2 or flags & 0x2000 or flags & 0x100):
        raise GateError("必须为 ARM64 PE32+ executable；DLL/32bit/EC/x64 拒绝")
    imagebase = struct.unpack_from("<Q", optional, 24)[0]
    image_size, header_size = struct.unpack_from("<II", optional, 56)
    subsystem = struct.unpack_from("<H", optional, 68)[0]
    ndirs = struct.unpack_from("<I", optional, 108)[0]
    if subsystem != 3 or ndirs != 16 or image_size == 0 or header_size > len(data):
        raise GateError("PE console/image/data-directories gate")
    dirs = [struct.unpack_from("<II", optional, 112 + index * 8) for index in range(16)]
    sections = sections_at(data, header + 20 + optsize, count)
    for section in sections:
        if section["rva"] + max(section["virtual_size"], section["raw_size"]) > image_size:
            raise GateError("PE section image 越界")
    def at(rva, size):
        candidates = [section for section in sections if section["rva"] <= rva and
                      rva + size <= section["rva"] + section["raw_size"] and section["raw_offset"]]
        if len(candidates) != 1:
            raise GateError("PE RVA 没有唯一 file-backed section")
        section = candidates[0]
        return span(data, section["raw_offset"] + rva - section["rva"], size)
    if dirs[14] != (0, 0):
        raise GateError("managed CLR image 拒绝")
    if dirs[10][0]:
        config = at(*dirs[10])
        size = struct.unpack("<I", span(config, 0, 4))[0]
        if size >= 208:
            if struct.unpack("<Q", span(config, 200, 8))[0]:
                raise GateError("CHPE metadata 非零")
    imports = []
    irva, isize = dirs[1]
    if not irva or isize < 20:
        raise GateError("missing actual PE imports")
    def cstring(rva):
        raw = bytearray()
        for offset in range(512):
            byte = at(rva + offset, 1)
            if byte == b"\0":
                return raw.decode("ascii")
            raw.extend(byte)
        raise GateError("PE import name 太长")
    terminated = False
    for offset in range(0, isize - 19, 20):
        original, timestamp, forward, name, first = struct.unpack("<IIIII", at(irva + offset, 20))
        if (original, timestamp, forward, name, first) == (0, 0, 0, 0, 0):
            terminated = True
            break
        symbols = []
        for index in range(MAX_FILE // 8):
            entry = struct.unpack("<Q", at((original or first) + index * 8, 8))[0]
            if not entry:
                break
            symbols.append({"ordinal": entry & 0xFFFF} if entry >> 63 else {"name": cstring(entry + 2)})
        else:
            raise GateError("PE import thunk 未终止")
        imports.append({"dll": cstring(name), "symbols": symbols})
    dlls = [item["dll"].lower() for item in imports]
    if not terminated or len(dlls) != 2 or set(dlls) != {"kernel32.dll", "msvcrt.dll"}:
        raise GateError("actual import closure 必须恰为 KERNEL32/default MSVCRT")
    required_apis = {"RtlCaptureContext", "RtlLookupFunctionEntry", "RtlVirtualUnwind"}
    for item in imports:
        names = {symbol["name"] for symbol in item["symbols"] if "name" in symbol}
        if item["dll"].lower() == "kernel32.dll":
            if not required_apis.issubset(names):
                raise GateError("KERNEL32 缺少精确命名 RtlCaptureContext/RtlLookupFunctionEntry/RtlVirtualUnwind")
        elif required_apis & names:
            raise GateError("Rtl API 必须由 KERNEL32 提供")
    prva, psize = dirs[3]
    if not prva or not psize or psize % 8:
        raise GateError("ARM64 exception table 缺失/尺寸错误")
    pdata, previous = [], -1
    for index in range(psize // 8):
        begin, unwind = struct.unpack("<II", at(prva + index * 8, 8))
        if begin <= previous:
            raise GateError("pdata 非严格升序")
        previous = begin
        entry = {"begin_rva": begin, "unwind_data": unwind, "pdata_rva": prva + index * 8}
        if unwind & 3 == 3:
            raise GateError("ARM64 pdata reserved encoding")
        if unwind & 3:
            entry["length"] = ((unwind >> 2) & 0x7FF) * 4
            entry["kind"] = "packed"
        else:
            word = struct.unpack("<I", at(unwind, 4))[0]
            if (word >> 18) & 3:
                raise GateError("ARM64 xdata 未支持 version")
            entry.update({"kind": "full", "xdata_rva": unwind, "length": (word & 0x3FFFF) * 4})
            epilogues, codewords = (word >> 22) & 31, (word >> 27) & 31
            header_words = 1
            if not epilogues and not codewords:
                extension = struct.unpack("<I", at(unwind + 4, 4))[0]
                epilogues, codewords, header_words = extension & 0xFFFF, (extension >> 16) & 255, 2
            size = (header_words + (0 if word & (1 << 21) else epilogues) + codewords + (1 if word & (1 << 20) else 0)) * 4
            raw = at(unwind, size)
            entry.update({"xdata_size": size, "xdata_hex": raw.hex(), "xdata_sha256": hashlib.sha256(raw).hexdigest(),
                          "xdata_scope": "header/epilogue scopes/unwind codes/optional handler RVA; opaque language handler data not inferred",
                          "language_handler_data": "OPEN" if word & (1 << 20) else "absent"})
        if entry["length"] == 0:
            raise GateError("pdata zero length")
        at(begin, entry["length"])
        pdata.append(entry)
    return {"path": str(path), "sha256": sha(path), "size": len(data), "machine": "0xAA64",
            "optional_magic": "0x20B", "image_base": imagebase, "image_size": image_size,
            "sections": sections, "symbols": coff_symbols(data, header, sections), "imports": imports,
            "pdata": pdata, "at": at}


def object_range(path, symbol):
    data = path.read_bytes()
    count = struct.unpack_from("<H", data, 2)[0]
    sections = sections_at(data, 20, count)
    symbols = coff_symbols(data, 0, sections)
    if symbol not in symbols:
        raise GateError("actual input object 缺 owned symbol")
    item = symbols[symbol]
    section = sections[item["section"] - 1]
    ends = [other["value"] for other in symbols.values() if other["section"] == item["section"] and
            other["value"] > item["value"] and other["type"] & 0x20]
    end = min(ends, default=section["raw_size"])
    if end <= item["value"] or end - item["value"] > 0x20000:
        raise GateError("actual owned function range 不明确")
    return span(data, section["raw_offset"] + item["value"], end - item["value"]), symbols


def bindings(pe, helper, negative):
    symbols = pe["symbols"]
    result = {"labels": {}, "functions": {}, "open": []}
    for name in LABELS:
        if name in symbols:
            item = symbols[name]
            result["labels"][name] = {"rva": item["rva"], "section": item["section"]}
        else:
            result["open"].append("missing linked symbol: " + name)
    ranges = {}
    for obj, name in ((helper, "frd_h264_dc_check_abi"), (negative, "frd_h264_dc_outer_probe"),
                      (negative, "frd_h264_dc_wrong_return")):
        raw, input_symbols = object_range(obj, name)
        if name not in symbols:
            result["open"].append("cannot bind owned helper/outer: " + name)
            continue
        start = symbols[name]["rva"]
        linked = pe["at"](start, len(raw))
        if raw != linked:
            raise GateError("linked helper/outer text 与实际 input object 不同")
        ranges[name] = (start, start + len(raw))
        owned = [entry for entry in pe["pdata"] if entry["begin_rva"] == start]
        label_offsets = {label: item["rva"] - start for label, item in symbols.items()
                         if label in LABELS and start <= item["rva"] < start + len(raw)}
        input_offsets = {label: item["value"] - input_symbols[name]["value"] for label, item in input_symbols.items()
                         if label in LABELS and item["section"] == input_symbols[name]["section"] and
                         0 <= item["value"] - input_symbols[name]["value"] < len(raw)}
        if any(label in label_offsets and label_offsets[label] != offset for label, offset in input_offsets.items()):
            raise GateError("linked owned label offset 与实际 object symbol 不同")
        result["functions"][name] = {"rva": start, "length": len(raw), "text_sha256": hashlib.sha256(linked).hexdigest(),
                                     "object_sha256": sha(obj), "label_offsets": label_offsets,
                                     "actual_input_label_offsets": input_offsets, "table_entries": owned}
        if not owned:
            if name != "frd_h264_dc_wrong_return":
                result["open"].append("no owned pdata (bad1 expected helper omission): " + name)
        elif len(owned) != 1 or owned[0]["length"] != len(raw):
            raise GateError("linked owned pdata/range 不一致")
    for name in FUNCTIONS:
        if name not in symbols:
            result["open"].append("compiler omitted/inlined symbol: " + name)
            continue
        start = symbols[name]["rva"]
        owned = [entry for entry in pe["pdata"] if entry["begin_rva"] == start]
        if len(owned) != 1:
            result["open"].append("compiler frame ownership unresolved: " + name)
            continue
        end = start + owned[0]["length"]
        if end <= start or end - start > 0x20000:
            raise GateError("owned C range 超出窄界限")
        ranges[name] = (start, end)
        result["functions"][name] = {"rva": start, "length": end - start, "table_entries": owned,
                                     "text_sha256": hashlib.sha256(pe["at"](start, end - start)).hexdigest(),
                                     "actual_frame_review": "OPEN; requires independent compiled review"}
    return result, ranges


def commands(root, source, out, support):
    result = {}
    for name in CRT:
        result["dependency-" + name] = [str(support.CC), "--target=" + support.TARGET, "-print-file-name=" + name]
    for variant in VARIANTS:
        stem = "variant-%d" % variant
        obj = out / ("driver-%d.obj" % variant)
        exe = out / ("abi-contract-%d.exe" % variant)
        helper = source / (("abi" if variant == 0 else "bad-unwind-%d" % variant) + ".obj")
        compile_argv = [str(support.CC), "--target=" + support.TARGET, "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
                        "-fno-omit-frame-pointer", "-fno-optimize-sibling-calls", "-D_WIN32_WINNT=0x0A00", "-DWINVER=0x0A00",
                        "-DFRD_DC_WINDOWS_ABI_CONTRACT=1", "-DFRD_CONTRACT_VARIANT=%d" % variant,
                        "-c", str(root / FIXTURE / "h264-luma-dc-windows-contract.c"), "-o", str(obj)]
        link_argv = [str(support.CC), "--target=" + support.TARGET, str(obj), str(helper), str(source / "negative.obj"),
                     "-lkernel32", "-lntdll", "-Wl,--Map=" + str(out / ("abi-contract-%d.map" % variant)), "-o", str(exe)]
        for label, argv in (("compile", compile_argv), ("link", link_argv)):
            result[stem + "-" + label + "-expansion"] = argv[:1] + ["-###"] + argv[1:]
            result[stem + "-" + label] = argv
        result[stem + "-object-readobj"] = [str(support.READOBJ), "--file-headers", "--sections", "--symbols", "--relocations", "--unwind", str(obj)]
        result[stem + "-pe-readobj"] = [str(support.READOBJ), "--file-headers", "--sections", "--symbols", "--relocations", "--unwind", "--coff-imports", str(exe)]
    return result


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")


class Evidence:
    """一次注册的期望身份不可刷新；output 副本与外部依赖共享同一 identity 表。"""
    def __init__(self, out, allowed):
        self.out = out
        self.allowed = allowed
        self.expected = {}
        self.input_validator = None

    def register(self, path, *, pin=None):
        ordinary(path)
        identity = {"sha256": sha(path), "size": path.stat().st_size}
        if pin is not None and identity["sha256"] != pin:
            raise GateError("已完成 evidence 与原始预期 pin 不同: %s" % path)
        if path in self.expected:
            if identity != self.expected[path]:
                raise GateError("已注册 evidence 漂移；不得用新 hash 刷新: %s" % path)
        else:
            self.expected[path] = identity
        if path.is_relative_to(self.out):
            self.allowed.add(path.relative_to(self.out).as_posix())
        return {"path": str(path), **self.expected[path]}

    def write(self, name, value):
        clean_member(name)
        path = self.out / name
        data = json_bytes(value)
        pin = hashlib.sha256(data).hexdigest()  # 先固定生成值，绝不事后采纳被改写的 JSON。
        self.allowed.add(name)
        with path.open("xb") as stream:
            stream.write(data)
        self.register(path, pin=pin)

    def check(self, *, complete=False, inputs=True):
        if inputs and self.input_validator is not None:
            self.input_validator()
        snapshot = tree(self.out)
        dirs = {str(parent) for name in self.allowed for parent in PurePosixPath(name).parents if str(parent) != "."}
        if any(name not in (self.allowed if item["kind"] == "file" else dirs) for name, item in snapshot.items()):
            raise GateError("output inventory 出现未授权成员")
        for path, expected in self.expected.items():
            ordinary(path)
            if {"sha256": sha(path), "size": path.stat().st_size} != expected:
                raise GateError("已注册 evidence 在使用/封装前后改变: %s" % path)
        if complete:
            actual = {name for name, item in snapshot.items() if item["kind"] == "file"}
            registered = {path.relative_to(self.out).as_posix() for path in self.expected if path.is_relative_to(self.out)}
            if actual != registered:
                raise GateError("封装时存在未冻结的实际 output member")

    def failed_partials(self):
        """仅失败阶段冻结新出现的原始残留；从不重新接纳已注册成员的新 bytes。"""
        snapshot = tree(self.out)
        for name, item in snapshot.items():
            if item["kind"] == "file" and name in self.allowed:
                path = self.out / name
                if path not in self.expected:
                    self.register(path, pin=item["sha256"])
        self.write("failure-output-identities.json", {"scope": "failed partial observation; no acceptance", "observed": snapshot,
                   "original_expected": {str(path): value for path, value in self.expected.items()}})


def artifact(out, allowed, evidence):
    # 只从生成/复制时冻结的期望表生成 manifest，绝不把当前 snapshot 当作新授权。
    evidence.check(complete=True)
    members = {path.relative_to(out).as_posix(): item["sha256"] for path, item in evidence.expected.items() if path.is_relative_to(out)}
    if set(members) & (ENVELOPE_MEMBERS | {TERMINAL_PRECOMMIT, "terminal-status.json", "archive-failure.json"}):
        raise GateError("封装阶段非法自引用/复用")
    evidence.write("members.json", {"schema": SCHEMA + ".members", "files": members})
    allowed.add("evidence.zip")
    evidence.check(complete=True)
    with zipfile.ZipFile(out / "evidence.zip", "x", compression=zipfile.ZIP_DEFLATED) as archive:
        for name in sorted(members):
            evidence.check(inputs=False)
            archive.write(out / name, name)
            evidence.check(inputs=False)
        archive.write(out / "members.json", "members.json")
    evidence.register(out / "evidence.zip")
    identity = {"schema": SCHEMA + ".archive", "sha256": evidence.expected[out / "evidence.zip"]["sha256"],
                "members_sha256": evidence.expected[out / "members.json"]["sha256"], "strict_members": sorted(members) + ["members.json"]}
    evidence.write("archive.json", identity)
    evidence.check(complete=True)
    verify_bundle(out, set(members), SCHEMA)
    evidence.check(complete=True)  # 闭环后仍比较原 pin；不能由 ZIP/current bytes 自证。
    return identity


def build(root, source, installation, out, support, summary, allowed, evidence):
    support.host_gate()
    initial = source_snapshot(root, support)
    evidence.write("source-inputs.json", initial)
    for name in initial:
        evidence.register(root / name, pin=initial[name]["sha256"])
        dest = out / "inputs" / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        with (root / name).open("rb") as original, dest.open("xb") as copy:
            while block := original.read(1024 * 1024):
                copy.write(block)
        evidence.register(dest, pin=initial[name]["sha256"])
    inventory, input_tree, install_tree, objects = admit_inputs(root, source, installation, support)
    evidence.write("tool-inputs.json", inventory)
    evidence.write("same-run-inputs.json", {"objects": objects, "object_directory": input_tree, "installation_directory": install_tree})
    evidence.write("authorization.json", {"base": "122f44e9a1c691a723cdb0229d7aa40b41ae123b", "driver_review_sha256": "cf32dcd01c86d2ec9b1525d94635756355e03bff951a3511beb7d390c1ce239c",
                                            "driver_pins": PINS, "support_sha256": SUPPORT_SHA,
                                            "scope": "root embedded source pins; no SDD runtime/discovered authorization; native OPEN"})
    argv = commands(root, source, out, support)
    evidence.write("allowed-commands.json", argv)
    dependencies = {}
    def validate_inputs():
        if source_snapshot(root, support) != initial or tree(source) != input_tree or tree(installation) != install_tree:
            raise GateError("source/input/install identity 在命令前后改变")
        tools_match(inventory, support)
    evidence.input_validator = validate_inputs
    validator = evidence.check
    validator()  # 第一条工具命令前，所有已完成的材料/源码副本均已注册。
    def run(label):
        allowed.update(label + suffix for suffix in (".stdout", ".stderr", ".receipt.json"))
        record = support.terminal(out, label, argv[label], cwd=root, validator=validator, expected=argv)
        if record["wait_terminal"] != "exited" or record["returncode"] != 0:
            summary["first_command_failure"] = {"label": label, "receipt": label + ".receipt.json", "terminal": record}
            support.require_zero(record)
        for stream in ("stdout", "stderr"):
            path = out / (label + "." + stream)
            if sha(path) != record[stream]["sha256"]:
                raise GateError("actual command raw stream 在封存后改变")
            keep(path, pin=record[stream]["sha256"])
        keep(out / (label + ".receipt.json"), pin=hashlib.sha256(json_bytes(record)).hexdigest())
        return record
    def keep(path, *, pin=None):
        if path.is_relative_to(support.TOOL_ROOT):
            pin = inventory[path.relative_to(support.TOOL_ROOT).as_posix()]["sha256"]
        elif path.is_relative_to(source):
            pin = input_tree[path.relative_to(source).as_posix()]["sha256"]
        return evidence.register(path, pin=pin)
    for name in CRT:
        label = "dependency-" + name
        run(label)
        text = (out / (label + ".stdout")).read_text().strip()
        path = Path(text)
        if not path.is_absolute() or not path.is_relative_to(support.TOOL_ROOT) or path.name != name:
            raise GateError("compiler dependency diagnostic 非固定工具内精确 path")
        resolved = path.resolve(strict=True)
        if not resolved.is_relative_to(support.TOOL_ROOT):
            raise GateError("dependency symlink 越界")
        dependencies[name] = keep(resolved)
    evidence.write("dependency-inputs.json", dependencies)
    for variant in VARIANTS:
        summary["not_attempted"].remove(variant)
        item = {"variant": variant, "status": "failed", "native": "OPEN", "object": None, "pe": None}
        summary["variants"].append(item)
        stem = "variant-%d" % variant
        obj, exe, linkmap = (out / ("driver-%d.obj" % variant), out / ("abi-contract-%d.exe" % variant), out / ("abi-contract-%d.map" % variant))
        # 注册 partial files；失败时绝不检查它们然后继续下一 variant。
        allowed.update((obj.name, exe.name, linkmap.name))
        run(stem + "-compile-expansion")
        run(stem + "-compile")
        item["object"] = keep(obj)  # exit0 + missing output 是失败。
        item["object_container"] = support.coff_admit(obj)
        run(stem + "-object-readobj")
        support.readobj_agrees((out / (stem + "-object-readobj.stdout")).read_text(), obj)
        run(stem + "-link-expansion")
        run(stem + "-link")
        item["pe"] = keep(exe)
        item["map"] = keep(linkmap)
        if linkmap.stat().st_size == 0:
            raise GateError("link exit0 + empty map")
        run(stem + "-pe-readobj")
        support.readobj_agrees((out / (stem + "-pe-readobj.stdout")).read_text(), exe)
        pe = pe_admit(exe)
        helper = source / (("abi" if variant == 0 else "bad-unwind-%d" % variant) + ".obj")
        owned, ranges = bindings(pe, helper, source / "negative.obj")
        # map 原文与 -### 是真实完整 closure diagnostic；绝不执行其中 linker commands。
        closure = []
        expansion = (out / (stem + "-link-expansion.stderr")).read_text()
        mapped = linkmap.read_text()
        # 实际 expansion/map 内的明确 absolute archive/object bytes 均绑定；-l 的
        # resolution 由 pinned tool inventory + 上述固定 print-file-name diagnostics 保留。
        for text in (expansion, mapped):
            for name in re.findall(r"(/[^\s\"'()]+\.(?:a|o|obj))(?=[\s\"'():]|$)", text):
                path = Path(name)
                if path in (obj, helper, source / "negative.obj"):
                    identity = keep(path)
                elif path.is_relative_to(support.TOOL_ROOT):
                    resolved = path.resolve(strict=True)
                    if not resolved.is_relative_to(support.TOOL_ROOT):
                        raise GateError("actual linker dependency 越界")
                    identity = keep(resolved)
                else:
                    raise GateError("actual linker absolute dependency 不在授权输入内")
                if identity not in closure:
                    closure.append(identity)
        if not closure:
            raise GateError("真实 linker dependency provenance unresolved")
        material = {key: value for key, value in pe.items() if key != "at"}
        material.update({"owned_bindings": owned, "actual_linker_dependencies": closure,
                         "fixed_dependency_diagnostics": dependencies,
                         "scope": "actual raw PE/table/import/text ownership; compiler frame/native review OPEN"})
        filename = "variant-%d-pe-bindings.json" % variant
        evidence.write(filename, material)
        for name, (begin, end) in ranges.items():
            label = stem + "-disassemble-" + name
            argv[label] = [str(support.OBJDUMP), "--disassemble", "--reloc",
                           "--start-address=0x%x" % (pe["image_base"] + begin),
                           "--stop-address=0x%x" % (pe["image_base"] + end), str(exe)]
            # 符号产生的 bounded argv 在实际 spawn 前单独封存；固定 commands 不重写。
            command_name = label + ".argv.json"
            evidence.write(command_name, argv[label])
            run(label)
        item.update({"status": "compiled-only", "open": owned["open"], "bindings": filename})
    validator()
    evidence.write("source-inputs-after.json", source_snapshot(root, support))
    evidence.write("tool-inputs-after.json", tools_match(inventory, support))
    evidence.write("actual-output-identities.json", {"scope": "completed identities before this list/summary/envelope; later members registered separately",
                   "files": {str(path): value for path, value in evidence.expected.items()}})
    validator()
    summary["status"] = "compiled-only"
    # 全部实际 argv 来自固定源码向量/真实 symbol-derived 范围；archive records 不授权。


def note_failure(summary, error, phase):
    item = {"phase": phase, "type": type(error).__name__, "message": str(error)}
    summary["failures"].append(item)
    summary.setdefault("first_failure", item)
    summary["status"] = "failed"
    return item


def controlled_failed_json(out, name, value):
    """唯一失败终结写入：保留失败 ZIP/raw；不重新封装或刷新原 identity 表。"""
    directory(out)
    path = out / name
    if path.exists() or path.is_symlink():
        ordinary(path)
    temporary = out / ("." + name + ".failed.tmp")
    with temporary.open("xb") as stream:
        stream.write(json_bytes(value))
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def commit_terminal(out, evidence):
    """所有接受检查之后的最后原子发布；后面不得再执行可失败的接受校验。"""
    candidate, authority = out / TERMINAL_PRECOMMIT, out / "terminal-status.json"
    if authority.exists() or authority.is_symlink():
        raise GateError("唯一 terminal authority 必须尚不存在")
    # 预先准备 rename 后的 identity 表；不重新 hash，不在发布后读取文件。
    expected = dict(evidence.expected)
    expected[authority] = expected.pop(candidate)
    allowed = (evidence.allowed - {TERMINAL_PRECOMMIT}) | {"terminal-status.json"}
    os.rename(candidate, authority)  # 同一固定新目录内原子 commit；failure 尚无 success authority。
    evidence.expected = expected
    evidence.allowed = allowed


def finish_evidence(out, summary, allowed, evidence):
    # 成功不能在封装前成为 loose result。summary 固定为待封装，最终状态独立提交。
    build_status = summary["status"]
    pending = {**summary, "status": "packaging-pending", "build_status": build_status,
               "terminal_authority": "terminal-status.json only; absent/pending is never success; terminal-precommit.json is not authority"}
    try:
        evidence.write("summary.json", pending)
        identity = artifact(out, allowed, evidence)
        terminal = {"schema": SCHEMA + ".terminal", "status": build_status,
                    "archive_verified": True, "archive": identity,
                    "summary_sha256": evidence.expected[out / "summary.json"]["sha256"],
                    "failures": summary["failures"], "first_failure": summary.get("first_failure"),
                    "scope": summary.get("scope"), "authority_filename": "terminal-status.json"}
        # precommit 固定最终 bytes，但文件名不授予 authority；终态发布前完成所有检查。
        evidence.write(TERMINAL_PRECOMMIT, terminal)
        with (out / TERMINAL_PRECOMMIT).open("rb") as stream:
            os.fsync(stream.fileno())
        evidence.check(complete=True)
        verify_bundle(out, set(load_json(out / "members.json")["files"]), SCHEMA, external={TERMINAL_PRECOMMIT})
        evidence.check(complete=True)
        commit_terminal(out, evidence)  # 成功后的接受路径不再有 validation/hash/read/fsync。
    except Exception as error:
        archive_failure = note_failure(summary, error, "archive-finalization")
        terminal = {"schema": SCHEMA + ".terminal", "status": "failed", "archive_verified": False,
                    "archive_failure": archive_failure, "first_failure": summary["first_failure"],
                    "failures": summary["failures"], "scope": summary.get("scope"),
                    "partial_archive_is_not_accepted": True}
        failure = {**terminal, "schema": SCHEMA + ".archive-failure",
                   "attempt_summary_sha256": evidence.expected.get(out / "summary.json", {}).get("sha256"),
                   "original_expected": {str(path): value for path, value in evidence.expected.items()}}
        # 写失败 terminal 优先；另外发布 failed summary 与首错，always-upload 不再只靠 stdout。
        persistence_errors = []
        for name, value in (("terminal-status.json", terminal), ("summary.json", summary), ("archive-failure.json", failure)):
            try:
                controlled_failed_json(out, name, value)
            except Exception as persistence_error:
                persistence_errors.append({"file": name, "type": type(persistence_error).__name__, "message": str(persistence_error)})
        print(json.dumps({"status": "failed", "archive_failure": archive_failure, "output": str(out), "persistence_errors": persistence_errors}))
        return 1
    print(json.dumps({"status": build_status, "output": str(out), "archive_sha256": identity["sha256"]}))
    return 0 if build_status == "compiled-only" else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--source-root", required=True)
    parser.add_argument("--compile-input", required=True)
    parser.add_argument("--installation", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args(argv)
    root, source, installation, out = paths(args.source_root, args.compile_input, args.installation, args.output)
    summary = {"schema": SCHEMA, "status": "failed", "variants": [], "not_attempted": list(VARIANTS), "failures": [],
               "scope": "C/object/PE compile-link only; Windows admission/Rtl/exception/cleanup/native/shipping/qmul/parity/QPC/guard/perf/package/live OPEN",
               "source_root": str(root), "compile_input": str(source), "installation": str(installation),
               "github": {key: os.environ.get(key) for key in ("GITHUB_SHA", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT")}}
    allowed = set(FIXED_MEMBERS)
    evidence = Evidence(out, allowed)
    support = None
    try:
        support = pinned_support(root)
        summary["started_utc"] = support.utc()
        build(root, source, installation, out, support, summary, allowed, evidence)
    except Exception as error:
        note_failure(summary, error, "build")
        # 只读封存 partial actual files；不是 retry、inspect command 或 synthetic PASS。
        try:
            evidence.failed_partials()
            if support is not None and not (out / "source-inputs-after.json").exists():
                evidence.write("source-inputs-after.json", source_snapshot(root, support))
            if support is not None and (out / "tool-inputs.json").exists() and not (out / "tool-inputs-after.json").exists():
                evidence.write("tool-inputs-after.json", tools_match(load_json(out / "tool-inputs.json"), support))
        except Exception as freeze_error:
            note_failure(summary, freeze_error, "failure-freeze")
    if support is not None:
        summary["ended_utc"] = support.utc()
    return finish_evidence(out, summary, allowed, evidence)


if __name__ == "__main__":
    raise SystemExit(main())
