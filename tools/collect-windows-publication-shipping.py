#!/usr/bin/env python3
"""仅CI固定普通Windows GUI的build/map/最终包身份；不执行PE或授shipping资格。"""
import hashlib
import json
import ntpath
import os
from pathlib import Path
import re
import shutil
import stat
import struct
import sys


ARCHITECTURES = {"x86": ("i686-pc-windows-msvc", 0x14c, 0x10b), "x86_64": ("x86_64-pc-windows-msvc", 0x8664, 0x20b)}
APPLICATION = "freeremotedesk-windows"
CRATE = "freeremotedesk_windows"
HELPER = "tools/verify-rdp-publication-benchmark.py"
LF_SHA = "c2e17b6016979330043a9e23530f5f9083a1fdaf42ecd9e17a2c3c701e3aac2e"
RAW_HELPERS = {LF_SHA: 33022, "5b6ae4106245b5072b6795e67fa90da83670d42486e303000db0ce68adac3111": 33580}
PE_CAP = 256 * 1024 * 1024
MAP_CAP = 128 * 1024 * 1024
RAW_CAP = 64 * 1024 * 1024
SNAPSHOT_CAP = 256 * 1024
INPUT_CAP = 2 * 1024 * 1024
JSON_CAP = 256 * 1024
SOURCES = (
    ("apps/freeremotedesk-windows/src/main.rs", "source-main.rs"),
    ("crates/frd-protocol-rdp/src/pixel_convert.rs", "source-pixel_convert.rs"),
    ("crates/frd-protocol-rdp/src/pixel_convert/x86.rs", "source-pixel_convert-x86.rs"),
    ("crates/frd-protocol-rdp/src/baseline.rs", "source-baseline.rs"),
    ("crates/frd-protocol-rdp/src/surface.rs", "source-surface.rs"),
)
CONFIGS = (
    "crates/frd-protocol-rdp/src/egfx.rs", "crates/frd-protocol-rdp/src/active_session.rs", "crates/frd-protocol-rdp/src/factory.rs",
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml", ".gitattributes",
    "apps/freeremotedesk-windows/Cargo.toml", "crates/frd-protocol-rdp/Cargo.toml", "packaging/windows/ffmpeg-manifest.json",
    "tools/stage-windows-package.ps1", "tools/verify-windows-package.ps1", ".github/workflows/build-windows.yml",
    "tools/collect-windows-publication-shipping.py", HELPER,
)


def require(value, message):
    if not value:
        raise ValueError(message)


def fail(identity, message, code=1):
    if identity["failure_exit"] == 0:
        identity["first_failure"] = message
        identity["failure_exit"] = code
    identity["collection_complete"] = False


def no_reparse(info, directory=False):
    require(not stat.S_ISLNK(info.st_mode) and not getattr(info, "st_file_attributes", 0) & 0x400, "symlink/reparse refused")
    require(stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode), "regular file/directory required")
    # Cargo可以hardlink普通输出；只读其完整bytes，不修改原件。


def ordinary(path):
    info = path.lstat()
    no_reparse(info)
    return info


def directory(path, create=False):
    if create and not os.path.lexists(path):
        path.mkdir()
    no_reparse(path.lstat(), directory=True)


def read_bounded(path, cap):
    require(ordinary(path).st_size <= cap, "read byte cap")
    with path.open("rb") as raw:
        no_reparse(os.fstat(raw.fileno()))
        data = raw.read(cap + 1)
    require(len(data) <= cap, "read grew beyond cap")
    return data


def file_identity(path, cap):
    before = ordinary(path)
    require(0 <= before.st_size <= cap, "file byte cap")
    digest = hashlib.sha256()
    count = 0
    with path.open("rb") as raw:
        opened = os.fstat(raw.fileno())
        no_reparse(opened)
        while True:
            chunk = raw.read(65536)
            if not chunk:
                break
            count += len(chunk)
            require(count <= cap, "file read grew beyond cap")
            digest.update(chunk)
        ended = os.fstat(raw.fileno())
        no_reparse(ended)
    after = ordinary(path)
    signature = lambda v: (v.st_dev, v.st_ino, v.st_mode, v.st_nlink, v.st_size, v.st_mtime_ns, v.st_ctime_ns)
    # 只在相同stat API内部核稳定；不把Windows lstat/fstat差异或probe当兼容资格。
    require(signature(before) == signature(after) and signature(opened) == signature(ended), "file changed during hash")
    require(count == before.st_size == opened.st_size, "file byte count changed")
    return dict(bytes=count, sha256=digest.hexdigest())


def copy_bounded(source, destination, cap):
    count = 0
    with source.open("rb") as raw, destination.open("xb") as output:
        no_reparse(os.fstat(raw.fileno()))
        no_reparse(os.fstat(output.fileno()))
        while True:
            chunk = raw.read(min(65536, cap - count + 1))
            if not chunk:
                break
            count += len(chunk)
            require(count <= cap, "copy byte cap")
            require(output.write(chunk) == len(chunk), "copy short write")


def helper_functions(repo):
    path = repo / HELPER
    raw = read_bounded(path, 65536)
    physical = hashlib.sha256(raw).hexdigest()
    require(physical in RAW_HELPERS and len(raw) == RAW_HELPERS[physical], "unreviewed raw helper source")
    normalized = raw.replace(b"\r\n", b"\n")
    require(b"\r" not in normalized and hashlib.sha256(normalized).hexdigest() == LF_SHA, "helper normalized LF hash mismatch")
    namespace = {"__name__": "frd_bound_windows_shipping_capture", "__file__": str(path)}
    # 先核完整双pin和规范hash，再compile刚读原物理bytes；不import或走pycache。
    exec(compile(raw, str(path), "exec"), namespace)
    require(namespace["BUDGETS"]["build"] == 900 and namespace["LIMITS"]["build"] == (RAW_CAP, RAW_CAP), "capture budget/caps changed")
    binding = dict(bytes=len(raw), raw_sha256=physical, normalized_lf_sha256=LF_SHA)
    return namespace["capture"], namespace["windows_argv"], binding


def locations(repo, architecture):
    return dict(evidence=repo / "target" / ("rdp-publication-shipping-windows-" + architecture),
                built=repo / "target" / ("release" if architecture == "x86_64" else ARCHITECTURES[architecture][0] + "/release") / (APPLICATION + ".exe"),
                staged=repo / "target/package-test" / (APPLICATION + ".exe"),
                manifest=repo / "target/package-test/ffmpeg-manifest.json")


def build_argv(cargo, architecture, mapping):
    args = [cargo, "rustc", "--locked", "--release", "-v", "-p", APPLICATION, "--bin", APPLICATION]
    if architecture == "x86":
        args += ["--target", ARCHITECTURES[architecture][0]]
    return args + ["--", "-C", "link-arg=/MAP:" + str(mapping)]


def rustc_invocation(raw, architecture, mapping, windows_argv):
    candidates = []
    for line in raw.decode("utf-8", errors="strict").splitlines():
        stripped = line.strip()
        if not (stripped.startswith("Running `") and stripped.endswith("`")):
            continue
        args = windows_argv(stripped[9:-1])
        if "--crate-name" in args and args.index("--crate-name") + 1 < len(args) and args[args.index("--crate-name") + 1] == CRATE:
            candidates.append((line, args))
    require(len(candidates) == 1, "exactly one ordinary main rustc invocation required")
    line, args = candidates[0]
    require(ntpath.basename(args[0]).lower() == "rustc.exe", "unreviewed compiler/wrapper")
    require("--test" not in args and args.count("--emit=dep-info,link") == 1, "ordinary non-test linked main required")
    for option, value in (("--crate-name", CRATE), ("--crate-type", "bin")):
        require(args.count(option) == 1 and args[args.index(option) + 1] == value, "actual rustc main/type mismatch")
    if architecture == "x86":
        require(args.count("--target") == 1 and args[args.index("--target") + 1] == ARCHITECTURES[architecture][0], "explicit i686 target required")
    else:
        require("--target" not in args, "preserve original x64 implicit host target")
    require(sum(arg.replace("\\", "/").endswith("apps/freeremotedesk-windows/src/main.rs") for arg in args) == 1, "actual main source mismatch")
    cfgs = [args[i + 1] for i, arg in enumerate(args[:-1]) if arg == "--cfg"]
    require("test" not in cfgs, "cfg(test) refused")
    codes = [args[i + 1] for i, arg in enumerate(args[:-1]) if arg == "-C"]
    require("opt-level=3" in codes and "lto=thin" in codes, "ordinary release/thin-LTO required")
    require(codes.count("link-arg=/MAP:" + str(mapping)) == 1, "same-link map required")
    require(not any(code.startswith(("target-cpu=", "target-feature=", "panic=", "force-frame-pointers=", "control-flow-guard=")) for code in codes), "CPU/protection overrides refused")
    require(len(line.encode()) <= 65536, "rustc argv cap")
    return dict(raw_running_line=line, argv=args, compiler_path=args[0], compiler_identity=file_identity(Path(args[0]), PE_CAP))


def pe_header(path, architecture):
    require(64 <= ordinary(path).st_size <= PE_CAP, "PE size cap")
    with path.open("rb") as raw:
        dos = raw.read(64)
        require(dos[:2] == b"MZ", "PE MZ required")
        offset = struct.unpack_from("<I", dos, 0x3c)[0]
        require(64 <= offset <= path.stat().st_size - 26, "PE offset range")
        raw.seek(offset)
        header = raw.read(26)
    require(header[:4] == b"PE\0\0", "PE signature required")
    machine = struct.unpack_from("<H", header, 4)[0]
    optional_size = struct.unpack_from("<H", header, 20)[0]
    magic = struct.unpack_from("<H", header, 24)[0]
    require(optional_size >= 2 and offset + 24 + optional_size <= path.stat().st_size, "PE optional header range")
    require((machine, magic) == ARCHITECTURES[architecture][1:], "PE machine/optional class mismatch")


def unique_json(raw):
    def pairs(items):
        require(len(dict(items)) == len(items), "duplicate JSON key")
        return dict(items)
    return json.loads(raw.decode("utf-8-sig"), object_pairs_hook=pairs)


def save(evidence, identity):
    raw = (json.dumps(identity, ensure_ascii=True, indent=2) + "\n").encode()
    require(len(raw) <= JSON_CAP, "identity JSON cap")
    destination = evidence / "identity.json"
    if os.path.lexists(destination):
        ordinary(destination)
    pending = evidence / "identity.pending.json"
    with pending.open("xb") as output:
        no_reparse(os.fstat(output.fileno()))
        require(output.write(raw) == len(raw), "identity short write")
    os.replace(pending, destination)


def inputs_stable(repo, evidence, identity, phase):
    expected = dict(SOURCES)
    expected.update({name: None for name in CONFIGS})
    require(set(identity["inputs"]) == set(expected), "fixed source/config set mismatch")
    for name, snapshot in expected.items():
        row = identity["inputs"][name]
        require(row["snapshot"] == snapshot, "fixed snapshot mismatch")
        row[phase] = file_identity(repo / name, SNAPSHOT_CAP if snapshot else INPUT_CAP)
        require(row[phase] == row["before"], "source/config changed: " + name)
        if snapshot:
            row["snapshot_" + phase] = file_identity(evidence / snapshot, SNAPSHOT_CAP)
            require(row["snapshot_" + phase] == row["before"], "snapshot changed")
    require(identity["inputs"][HELPER]["before"]["sha256"] in RAW_HELPERS, "raw helper source pin changed")
    for row in identity["tools"]:
        require(row["command"] in ("cargo", "rustc", "actual rustc -v"), "unknown tool record")
        row[phase] = file_identity(Path(row["path"]), PE_CAP)
        require(row[phase] == row["before"], "tool changed")


def build(repo, architecture, paths):
    directory(repo / "target", create=True)
    evidence = paths["evidence"]
    require(not os.path.lexists(evidence), "existing evidence refused")
    evidence.mkdir()
    mapping = evidence / (APPLICATION + ".map")
    identity = dict(schema="frd.windows-publication-shipping.v1", architecture=architecture, target=ARCHITECTURES[architecture][0], target_argument="absent-native-host" if architecture == "x86_64" else "explicit-i686", profile="release", default_features="unchanged; no feature arguments",
                    commit=os.environ.get("GITHUB_SHA"), run_id=os.environ.get("GITHUB_RUN_ID"), run_attempt=os.environ.get("GITHUB_RUN_ATTEMPT"), job=os.environ.get("GITHUB_JOB"),
                    inputs={}, tools=[], build=None, build_complete=False, staged_complete=False, collection_complete=False, failure_exit=0, first_failure=None,
                    caps=dict(pe=PE_CAP, map=MAP_CAP, raw_each=RAW_CAP, snapshot_each=SNAPSHOT_CAP, input_each=INPUT_CAP, identity=JSON_CAP),
                    runtime="NOT_RUN", shipping_isa="NOT_REVIEWED", shipping_qualification="OPEN", protection="not-qualified; flags unchanged", descendants_quiescent="not proven")
    save(evidence, identity)
    try:
        for name, snapshot in SOURCES + tuple((name, None) for name in CONFIGS):
            cap = SNAPSHOT_CAP if snapshot else INPUT_CAP
            row = dict(snapshot=snapshot, before=file_identity(repo / name, cap))
            identity["inputs"][name] = row
            if snapshot:
                copy_bounded(repo / name, evidence / snapshot, cap)
                require(file_identity(evidence / snapshot, cap) == row["before"], "source snapshot mismatch")
        capture, windows_argv, identity["capture_source"] = helper_functions(repo)
        env = os.environ.copy()
        selected_env = ("RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTUP_TOOLCHAIN", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET",
                        "CARGO_PROFILE_RELEASE_LTO", "CARGO_PROFILE_RELEASE_OPT_LEVEL", "CARGO_PROFILE_RELEASE_STRIP", "CARGO_PROFILE_RELEASE_DEBUG", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "CARGO_PROFILE_RELEASE_PANIC",
                        "CARGO_TARGET_" + ARCHITECTURES[architecture][0].upper().replace("-", "_") + "_RUSTFLAGS")
        identity["compilation_environment"] = {name: env.get(name) for name in selected_env}
        require(not env.get("CARGO_BUILD_TARGET"), "preserve original build target selection")
        if env.get("CARGO_TARGET_DIR"):
            configured = Path(env["CARGO_TARGET_DIR"])
            configured = configured if configured.is_absolute() else repo / configured
            require(configured.absolute() == (repo / "target").absolute(), "stage uses fixed default target directory")
        for name in selected_env:
            if name in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS") or name.startswith("CARGO_PROFILE_") or name.endswith("_RUSTFLAGS"):
                require(not env.get(name), "unreviewed profile/CPU/wrapper override")
        for command in ("cargo", "rustc"):
            path = shutil.which(command)
            require(path is not None and ntpath.basename(path).lower() == command + ".exe", "fixed Rust executable missing")
            identity["tools"].append(dict(command=command, path=str(Path(path).resolve()), before=file_identity(Path(path).resolve(), PE_CAP)))
        inputs_stable(repo, evidence, identity, "before_build")
        record = capture("build", build_argv(str(Path(shutil.which("cargo")).absolute()), architecture, mapping), repo, env, evidence)
        identity["build"] = record
        if record["failure_exit"]:
            fail(identity, "build: " + record["first_failure"], record["failure_exit"])
        if identity["failure_exit"] == 0:
            require(record["capture_complete"] and record["exit_code"] == 0, "build terminal/EOF incomplete")
            invocation = rustc_invocation(read_bounded(evidence / "build.stderr", RAW_CAP), architecture, mapping, windows_argv)
            identity["actual_rustc"] = invocation
            identity["tools"].append(dict(command="actual rustc -v", path=invocation["compiler_path"], before=invocation["compiler_identity"]))
            pe_header(paths["built"], architecture)
            identity["built_pe"] = file_identity(paths["built"], PE_CAP)
            identity["link_map"] = file_identity(mapping, MAP_CAP)
            require(identity["link_map"]["bytes"] > 0, "empty map refused")
            identity["raw_identity"] = {name: file_identity(evidence / ("build." + name), RAW_CAP) for name in ("stdout", "stderr")}
            inputs_stable(repo, evidence, identity, "after_build")
            identity["build_complete"] = True
    except Exception as error:
        fail(identity, type(error).__name__ + ": " + str(error))
    finally:
        if set(identity["inputs"]) == {name for name, _ in SOURCES} | set(CONFIGS):
            try:
                inputs_stable(repo, evidence, identity, "build_final")
            except Exception as error:
                fail(identity, "build finalization: " + str(error))
        for name in ("build.stdout", "build.stderr"):
            if not os.path.lexists(evidence / name):
                try:
                    (evidence / name).open("xb").close()  # build=None明确NOT_RUN，无runtime伪空流。
                except OSError as error:
                    fail(identity, "NOT_RUN raw finalization: " + str(error))
        try:
            save(evidence, identity)
        except Exception as error:
            fail(identity, "identity finalization: " + str(error))
    return identity["failure_exit"]


def finalize(repo, architecture, paths):
    directory(repo / "target")
    directory(paths["evidence"])
    evidence = paths["evidence"]
    identity = unique_json(read_bounded(evidence / "identity.json", JSON_CAP))
    require(type(identity) is dict and identity.get("schema") == "frd.windows-publication-shipping.v1", "current build identity required")
    require(not identity.get("staged_complete") and not identity.get("collection_complete"), "repeated finalize refused without overwriting old result")
    try:
        require(identity["architecture"] == architecture and identity["target"] == ARCHITECTURES[architecture][0] and identity["profile"] == "release", "architecture/profile mismatch")
        require(identity["failure_exit"] == 0 and identity["build_complete"] and identity["build"]["capture_complete"] and identity["build"]["exit_code"] == 0, "failed/partial build cannot bind package")
        inputs_stable(repo, evidence, identity, "before_staged")
        mapping = evidence / (APPLICATION + ".map")
        require(file_identity(mapping, MAP_CAP) == identity["link_map"], "same-link map changed")
        require(file_identity(paths["built"], PE_CAP) == identity["built_pe"], "build PE changed")
        directory(repo / "target/package-test")
        pe_header(paths["staged"], architecture)
        staged = file_identity(paths["staged"], PE_CAP)
        require(staged == identity["built_pe"], "final staged PE differs from build")
        manifest_before = file_identity(paths["manifest"], JSON_CAP)
        manifest = unique_json(read_bounded(paths["manifest"], JSON_CAP))
        require(manifest["schema"] == "freeremotedesk.windows.ffmpeg-package.v1" and manifest["platform"] == "windows-" + architecture, "package manifest schema/platform mismatch")
        entries = manifest["files"]
        require(type(entries) is list and all(type(row) is dict and type(row.get("path")) is str for row in entries), "manifest file entries required")
        require(len({row["path"] for row in entries}) == len(entries), "duplicate manifest path")
        application = [row for row in entries if row["path"] == APPLICATION + ".exe"]
        require(len(application) == 1 and application[0].get("role") == "application", "unique manifest application entry required")
        digest = application[0].get("sha256")
        require(type(digest) is str and re.fullmatch("[0-9A-F]{64}", digest) and digest.lower() == staged["sha256"], "manifest application hash mismatch")
        # 现有PS verifier负责完整包合同；这里仅绑定其后固定main与manifest，不遍历manifest路径。
        require(file_identity(paths["manifest"], JSON_CAP) == manifest_before, "manifest changed during parse")
        for name in ("stdout", "stderr"):
            require(file_identity(evidence / ("build." + name), RAW_CAP) == identity["raw_identity"][name], "build raw stream changed")
        copy_bounded(paths["staged"], evidence / (APPLICATION + ".exe"), PE_CAP)
        copied = file_identity(evidence / (APPLICATION + ".exe"), PE_CAP)
        require(copied == staged == file_identity(paths["built"], PE_CAP) == file_identity(paths["staged"], PE_CAP), "PE changed during final copy")
        identity["final_map"] = file_identity(mapping, MAP_CAP)
        require(identity["final_map"] == identity["link_map"], "final map changed")
        require(file_identity(paths["manifest"], JSON_CAP) == manifest_before, "manifest changed during finalize")
        identity["staged_pe"] = staged
        identity["saved_pe"] = copied
        identity["manifest"] = dict(path="target/package-test/ffmpeg-manifest.json", identity=manifest_before, application_sha256=digest,
                                    preceding_workflow_precondition="ordinary Stage/Verify and applicable Pester succeeded; no package/client execution performed here")
        inputs_stable(repo, evidence, identity, "after_staged")
        identity["staged_complete"] = True
        identity["collection_complete"] = True
    except Exception as error:
        fail(identity, type(error).__name__ + ": " + str(error))
    try:
        save(evidence, identity)
    except Exception as error:
        fail(identity, "identity finalization: " + str(error))
    return identity["failure_exit"]


def run(mode, architecture):
    require(mode in ("build", "finalize") and architecture in ARCHITECTURES, "fixed build/finalize and x86/x86_64 only")
    require(os.name == "nt" and sys.platform == "win32", "actual Windows Python required")
    require(os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("FRD_WINDOWS_PUBLICATION_SHIPPING") == "1", "explicit CI opt-in required")
    repo = Path(__file__).resolve().parent.parent
    paths = locations(repo, architecture)
    return build(repo, architecture, paths) if mode == "build" else finalize(repo, architecture, paths)


def main():
    try:
        require(len(sys.argv) == 3, "fixed phase/architecture arguments required")
        return run(*sys.argv[1:])
    except Exception as error:
        print("Windows shipping collection unavailable: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
