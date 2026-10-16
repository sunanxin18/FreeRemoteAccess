#!/usr/bin/env python3
"""仅CI opt-in的普通Linux GTK包身份收集；不运行客户端或授ISA资格。"""
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import stat
import struct
import sys


PROFILES = {"linux-x86": ("i686-unknown-linux-gnu", 1, 3), "linux-x86_64": ("x86_64-unknown-linux-gnu", 2, 62)}
APPLICATION = "freeremotedesk-linux"
CRATE = "freeremotedesk_linux"
HELPER = "tools/verify-rdp-publication-benchmark.py"
HELPER_SHA = "c2e17b6016979330043a9e23530f5f9083a1fdaf42ecd9e17a2c3c701e3aac2e"
ELF_CAP = 256 * 1024 * 1024
MAP_CAP = 128 * 1024 * 1024
RAW_CAP = 64 * 1024 * 1024  # 每条build流；复用helper的固定上限。
SOURCE_CAP = 256 * 1024
INPUT_CAP = 2 * 1024 * 1024
JSON_CAP = 256 * 1024
SOURCES = (
    ("apps/freeremotedesk-linux/src/main.rs", "source-main.rs"),
    ("crates/frd-protocol-rdp/src/pixel_convert.rs", "source-pixel_convert.rs"),
    ("crates/frd-protocol-rdp/src/pixel_convert/x86.rs", "source-pixel_convert-x86.rs"),
    ("crates/frd-protocol-rdp/src/baseline.rs", "source-baseline.rs"),
    ("crates/frd-protocol-rdp/src/surface.rs", "source-surface.rs"),
)
CONFIGS = (
    "crates/frd-protocol-rdp/src/egfx.rs", "crates/frd-protocol-rdp/src/active_session.rs",
    "crates/frd-protocol-rdp/src/factory.rs", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
    ".cargo/config.toml", "apps/freeremotedesk-linux/Cargo.toml", "crates/frd-protocol-rdp/Cargo.toml",
    "tools/stage-linux-package.sh", ".github/workflows/build-linux-ffmpeg.yml",
    "tools/collect-linux-publication-shipping.py", HELPER,
)


def require(ok, message):
    if not ok:
        raise ValueError(message)


def fail(identity, message, code=1):
    if identity["failure_exit"] == 0:
        identity["failure_exit"] = code
        identity["first_failure"] = message
    identity["collection_complete"] = False


def ordinary(path):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode), "普通非符号链接文件要求: " + str(path))
    return info


def file_identity(path, cap):
    before = ordinary(path)
    require(0 <= before.st_size <= cap, "固定文件byte cap: " + str(path))
    digest = hashlib.sha256()
    count = 0
    with path.open("rb") as raw:
        while True:
            block = raw.read(65536)
            if not block:
                break
            count += len(block)
            require(count <= cap, "读取超过byte cap")
            digest.update(block)
    after = ordinary(path)
    signature = lambda value: (value.st_dev, value.st_ino, value.st_mode, value.st_nlink, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
    require(signature(before) == signature(after) and count == before.st_size, "hash期间文件变化")
    return dict(bytes=count, sha256=digest.hexdigest())



def read_bounded(path, cap):
    require(ordinary(path).st_size <= cap, "read byte cap")
    with path.open("rb") as raw:
        data = raw.read(cap + 1)
    require(len(data) <= cap, "read grew beyond cap")
    return data


def copy_bounded(source, destination, cap):
    count = 0
    with source.open("rb") as raw, destination.open("xb") as output:
        while True:
            block = raw.read(min(65536, cap - count + 1))
            if not block:
                break
            count += len(block)
            require(count <= cap, "copy byte cap")
            require(output.write(block) == len(block), "copy short write")


def directory(path, create=False):
    # 固定旁路路径逐级验证，禁止target或evidence为链接/非目录。
    if create and not os.path.lexists(path):
        path.mkdir()
    require(stat.S_ISDIR(path.lstat().st_mode), "普通非链接目录要求: " + str(path))


def locations(repo, selected):
    target = PROFILES[selected][0]
    text = os.environ.get("CARGO_TARGET_DIR") or str(repo / "target")
    require("\0" not in text and ".." not in Path(text).parts, "opt-in Cargo目录不可含NUL/..")
    cargo_root = Path(text)
    if not cargo_root.is_absolute():
        cargo_root = repo / cargo_root
    cargo_root = cargo_root.absolute()
    evidence = repo / "target" / ("rdp-publication-shipping-" + selected)
    return dict(evidence=evidence, cargo_root=cargo_root,
                built=cargo_root / target / "release" / APPLICATION,
                staged=cargo_root / "linux" / "release" / selected / "FreeRemoteDesk" / APPLICATION,
                map=evidence / (APPLICATION + ".map"))


def load_capture(repo):
    path = repo / HELPER
    ordinary(path)
    raw = read_bounded(path, 65536)
    require(len(raw) <= 65536 and hashlib.sha256(raw).hexdigest() == HELPER_SHA, "已审capture helper完整SHA不符")
    namespace = {"__name__": "frd_bound_shipping_capture", "__file__": str(path)}
    # 执行刚刚核过的实际source bytes；不经importlib/pycache，也不运行helper的main。
    exec(compile(raw, str(path), "exec"), namespace)
    require(namespace["BUDGETS"]["build"] == 900 and namespace["LIMITS"]["build"] == (RAW_CAP, RAW_CAP), "capture固定预算/流cap变化")
    return namespace["capture"]


def build_argv(cargo, selected, mapping):
    return [cargo, "rustc", "--locked", "--release", "-v", "-p", APPLICATION,
            "--bin", APPLICATION, "--target", PROFILES[selected][0], "--", "-C", "link-arg=-Wl,-Map," + str(mapping)]


def rustc_invocation(raw, selected, mapping):
    require(len(raw) <= RAW_CAP, "build stderr cap")
    candidates = []
    for line in raw.decode("utf-8", errors="strict").splitlines():
        stripped = line.strip()
        if not (stripped.startswith("Running `") and stripped.endswith("`")):
            continue
        args = shlex.split(stripped[9:-1])
        if "--crate-name" in args and args.index("--crate-name") + 1 < len(args) and args[args.index("--crate-name") + 1] == CRATE:
            candidates.append((line, args))
    require(len(candidates) == 1, "必须恰有一次普通main rustc调用")
    line, args = candidates[0]
    require(Path(args[0]).name == "rustc", "不接受wrapper或未知编译callee")
    require("--test" not in args and args.count("--emit=dep-info,link") == 1, "普通linked non-test main要求")
    for name, value in (("--crate-name", CRATE), ("--crate-type", "bin"), ("--target", PROFILES[selected][0])):
        require(args.count(name) == 1 and args[args.index(name) + 1] == value, "真实rustc目标/类型不符")
    require(sum(arg.endswith("apps/freeremotedesk-linux/src/main.rs") for arg in args) == 1, "真实main路径不符")
    cfgs = [args[i + 1] for i, arg in enumerate(args[:-1]) if arg == "--cfg"]
    require('feature="default"' in cfgs and 'feature="gtk-shell"' in cfgs and "test" not in cfgs, "默认GTK/non-test配置要求")
    codes = [args[i + 1] for i, arg in enumerate(args[:-1]) if arg == "-C"]
    require("opt-level=3" in codes and "lto=thin" in codes, "普通release thin-LTO要求")
    require(codes.count("link-arg=-Wl,-Map," + str(mapping)) == 1, "必须同次map")
    require(not any(code.startswith(("target-cpu=", "target-feature=", "panic=", "force-frame-pointers=", "control-flow-guard=")) for code in codes), "不得覆盖CPU/保护策略")
    require(len(line.encode()) <= 65536, "真实rustc argv cap")
    return dict(raw_running_line=line, argv=args, compiler_path=args[0], compiler_identity=file_identity(Path(args[0]), ELF_CAP))


def elf_header(path, selected):
    with path.open("rb") as raw:
        header = raw.read(64)
    require(len(header) == 64 and header[:4] == b"\x7fELF", "ELF header不符")
    _, elf_class, machine = PROFILES[selected]
    require(header[4:7] == bytes((elf_class, 1, 1)) and struct.unpack_from("<H", header, 18)[0] == machine, "ELF架构不符")
    require(path.stat().st_mode & stat.S_IXUSR, "ELF无执行位")


def save(evidence, identity):
    raw = (json.dumps(identity, ensure_ascii=True, indent=2) + "\n").encode()
    require(len(raw) <= JSON_CAP, "identity JSON cap")
    destination = evidence / "identity.json"
    pending = evidence / "identity.pending.json"
    if os.path.lexists(destination):
        ordinary(destination)
    with pending.open("xb") as output:
        require(output.write(raw) == len(raw), "identity short write")
    os.replace(pending, destination)


def read_identity(evidence):
    path = evidence / "identity.json"
    require(ordinary(path).st_size <= JSON_CAP, "identity JSON cap")
    def pairs(items):
        require(len(dict(items)) == len(items), "重复JSON key")
        return dict(items)
    return json.loads(read_bounded(path, JSON_CAP), object_pairs_hook=pairs)


def check_inputs(repo, evidence, identity, phase):
    expected = {name: snapshot for name, snapshot in SOURCES}
    expected.update({name: None for name in CONFIGS})
    require(set(identity["inputs"]) == set(expected), "固定source/config集合不符")
    for name, snapshot in expected.items():
        row = identity["inputs"][name]
        current = file_identity(repo / name, SOURCE_CAP if snapshot else INPUT_CAP)
        row[phase] = current
        require(current == row["before"], "source/config变化: " + name)
        require(row["snapshot"] == snapshot, "snapshot路径不符")
        if snapshot:
            row["snapshot_" + phase] = file_identity(evidence / snapshot, SOURCE_CAP)
            require(row["snapshot_" + phase] == row["before"], "source snapshot变化")
    require(identity["inputs"][HELPER]["before"]["sha256"] == HELPER_SHA, "capture helper pin变化")
    for row in identity["tools"]:
        require(row["command"] in ("cargo", "rustc", "actual rustc -v"), "未知工具身份")
        row[phase] = file_identity(Path(row["path"]), ELF_CAP)
        require(row[phase] == row["before"], "工具身份变化")


def build(repo, selected, paths):
    directory(repo / "target", create=True)
    evidence = paths["evidence"]
    require(not os.path.lexists(evidence), "拒绝既有旁路目录")
    evidence.mkdir()
    identity = dict(schema="frd.linux-publication-shipping.v1", selected=selected, target=PROFILES[selected][0], profile="release", default_features="gtk-shell",
                    commit=os.environ.get("GITHUB_SHA"), run_id=os.environ.get("GITHUB_RUN_ID"), run_attempt=os.environ.get("GITHUB_RUN_ATTEMPT"), job=os.environ.get("GITHUB_JOB"),
                    cargo_target_dir=str(paths["cargo_root"]), inputs={}, tools=[], build=None,
                    build_complete=False, staged_complete=False, collection_complete=False, failure_exit=0, first_failure=None,
                    caps=dict(elf=ELF_CAP, map=MAP_CAP, build_stream_each=RAW_CAP, source_snapshot_each=SOURCE_CAP, input_each=INPUT_CAP, identity=JSON_CAP),
                    runtime="NOT_RUN", shipping_isa="NOT_REVIEWED", shipping_qualification="OPEN", descendants_quiescent="not proven",
                    archive_member="FreeRemoteDesk/" + APPLICATION, archive_member_verification="NOT_RUN; existing subsequent archive must bind this exact staged SHA")
    save(evidence, identity)
    try:
        for name, snapshot in SOURCES + tuple((name, None) for name in CONFIGS):
            cap = SOURCE_CAP if snapshot else INPUT_CAP
            row = dict(snapshot=snapshot, before=file_identity(repo / name, cap))
            identity["inputs"][name] = row
            if snapshot:
                copy_bounded(repo / name, evidence / snapshot, cap)
                require(file_identity(evidence / snapshot, cap) == row["before"], "snapshot复制不符")
        capture = load_capture(repo)
        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(paths["cargo_root"])
        names = ("RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTUP_TOOLCHAIN", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET",
                 "CARGO_PROFILE_RELEASE_LTO", "CARGO_PROFILE_RELEASE_OPT_LEVEL", "CARGO_PROFILE_RELEASE_STRIP", "CARGO_PROFILE_RELEASE_DEBUG", "CARGO_PROFILE_RELEASE_PANIC", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
                 "CARGO_TARGET_" + PROFILES[selected][0].upper().replace("-", "_") + "_RUSTFLAGS",
                 "CARGO_TARGET_" + PROFILES[selected][0].upper().replace("-", "_") + "_LINKER",
                 "CC_i686_unknown_linux_gnu", "CXX_i686_unknown_linux_gnu", "PKG_CONFIG_ALLOW_CROSS", "PKG_CONFIG_LIBDIR")
        identity["compilation_environment"] = {name: env.get(name) for name in names}
        for name in names:
            if name in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS") or name.startswith("CARGO_PROFILE_") or name.endswith("_RUSTFLAGS"):
                require(not env.get(name), "拒绝未审profile/CPU/wrapper覆盖")
        for command in ("cargo", "rustc"):
            path = shutil.which(command)
            require(path is not None, "缺少固定Rust命令")
            # 调用保留cargo名字，身份可指向其rustup shim，不重复执行版本查询。
            identity["tools"].append(dict(command=command, path=str(Path(path).resolve()), before=file_identity(Path(path).resolve(), ELF_CAP)))
        check_inputs(repo, evidence, identity, "before_build")
        argv = build_argv(str(Path(shutil.which("cargo")).absolute()), selected, paths["map"])
        record = capture("build", argv, repo, env, evidence)
        identity["build"] = record
        if record["failure_exit"]:
            fail(identity, "build: " + record["first_failure"], record["failure_exit"])
        if identity["failure_exit"] == 0:
            require(record["capture_complete"] and record["exit_code"] == 0, "build终态/EOF不完整")
            invocation = rustc_invocation(read_bounded(evidence / "build.stderr", RAW_CAP), selected, paths["map"])
            identity["actual_rustc"] = invocation
            identity["tools"].append(dict(command="actual rustc -v", path=invocation["compiler_path"], before=invocation["compiler_identity"]))
            elf_header(paths["built"], selected)
            identity["built_elf"] = file_identity(paths["built"], ELF_CAP)
            identity["link_map"] = file_identity(paths["map"], MAP_CAP)
            require(identity["built_elf"]["bytes"] >= 64 and identity["link_map"]["bytes"] > 0, "缺失/空ELF或map")
            identity["raw_identity"] = {name: file_identity(evidence / ("build." + name), RAW_CAP) for name in ("stdout", "stderr")}
            check_inputs(repo, evidence, identity, "after_build")
            identity["build_complete"] = True
    except Exception as error:
        fail(identity, type(error).__name__ + ": " + str(error))
    finally:
        if set(identity["inputs"]) == {name for name, _ in SOURCES} | set(CONFIGS):
            try:
                check_inputs(repo, evidence, identity, "build_final")
            except Exception as error:
                fail(identity, "build finalization: " + str(error))
        # 未执行build时，空双流不附native0/EOF；没有虚构runtime双流。
        for name in ("build.stdout", "build.stderr"):
            if not os.path.lexists(evidence / name):
                try:
                    (evidence / name).open("xb").close()
                except OSError as error:
                    fail(identity, "NOT_RUN raw finalization: " + str(error))
        try:
            save(evidence, identity)
        except Exception as error:
            fail(identity, "identity finalization: " + str(error))
    return identity["failure_exit"]


def finalize(repo, selected, paths):
    directory(repo / "target")
    evidence = paths["evidence"]
    directory(evidence)
    identity = read_identity(evidence)
    require(type(identity) is dict and identity.get("schema") == "frd.linux-publication-shipping.v1", "非当前build身份")
    require(not identity.get("staged_complete") and not identity.get("collection_complete"), "拒绝重复finalize，不覆盖旧结果")
    try:
        require(identity["selected"] == selected and identity["target"] == PROFILES[selected][0] and identity["profile"] == "release" and identity["default_features"] == "gtk-shell", "selector/profile绑定不符")
        require(identity["cargo_target_dir"] == str(paths["cargo_root"]), "staging CARGO_TARGET_DIR变化")
        require(not identity["staged_complete"] and not identity["collection_complete"], "拒绝重复finalize")
        require(identity["failure_exit"] == 0 and identity["build_complete"] and identity["build"]["capture_complete"] and identity["build"]["exit_code"] == 0, "build失败/partial不能绑定包")
        check_inputs(repo, evidence, identity, "before_staged")
        require(file_identity(paths["map"], MAP_CAP) == identity["link_map"], "同次map变化")
        require(file_identity(paths["built"], ELF_CAP) == identity["built_elf"], "原build ELF变化")
        elf_header(paths["staged"], selected)
        staged = file_identity(paths["staged"], ELF_CAP)
        require(staged == identity["built_elf"], "最终staged ELF不是同一build bytes")
        for name in ("stdout", "stderr"):
            require(file_identity(evidence / ("build." + name), RAW_CAP) == identity["raw_identity"][name], "build原流变化")
        copy_bounded(paths["staged"], evidence / APPLICATION, ELF_CAP)
        shutil.copymode(paths["staged"], evidence / APPLICATION)
        copied = file_identity(evidence / APPLICATION, ELF_CAP)
        require(file_identity(paths["staged"], ELF_CAP) == copied == staged and file_identity(paths["built"], ELF_CAP) == staged, "ELF复制期间bytes变化")
        require(file_identity(paths["map"], MAP_CAP) == identity["link_map"], "final map变化")
        identity["staged_elf"] = staged
        identity["saved_elf"] = copied
        identity["final_link_map"] = file_identity(paths["map"], MAP_CAP)
        check_inputs(repo, evidence, identity, "after_staged")
        identity["staged_complete"] = True
        identity["collection_complete"] = True
    except Exception as error:
        fail(identity, type(error).__name__ + ": " + str(error))
    try:
        save(evidence, identity)
    except Exception as error:
        fail(identity, "identity finalization: " + str(error))
    return identity["failure_exit"]


def run(mode, selected, profile):
    require(mode in ("build", "finalize") and selected in PROFILES and profile == "release", "只接受build/finalize、固定两x86平台、release")
    require(os.environ.get("FRD_LINUX_PUBLICATION_SHIPPING") == "1" and os.environ.get("GITHUB_ACTIONS") == "true", "仅显式CI opt-in")
    require(platform.system() == "Linux", "仅Linux普通包宿主")
    repo = Path(__file__).resolve().parent.parent
    paths = locations(repo, selected)
    return build(repo, selected, paths) if mode == "build" else finalize(repo, selected, paths)


def main():
    try:
        require(len(sys.argv) == 4, "固定mode/platform/release参数要求")
        return run(*sys.argv[1:])
    except Exception as error:
        # 非法/不可写evidence或尚无identity时不能安全伪造可上传结果。
        print("publication shipping collection unavailable: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
