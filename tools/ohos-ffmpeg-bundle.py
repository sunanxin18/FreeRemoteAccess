#!/usr/bin/env python3
"""官方 API26 OHOS ARM64 FFmpeg 独立构建和静态产物门；不执行目标代码。"""
import argparse
import ast
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import shlex
import struct
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

TARGET = "aarch64-unknown-linux-ohos"
VERSION = "8.1.2"
ARCHIVE_SHA = "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c"
URL = f"https://ffmpeg.org/releases/ffmpeg-{VERSION}.tar.xz"
SDK_VERSION = "26.0.0.105"
DEFAULT_SDK = "/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony"
LIBS = ("libavutil.so.60", "libavcodec.so.62", "libfreeremotedesk_ffmpeg.so")
DEPS = {
    LIBS[0]: ["libc.so"],
    LIBS[1]: [LIBS[0], "libc.so"],
    LIBS[2]: [LIBS[1], LIBS[0], "libc.so"],
}
CONFIG_FILES = ("FFmpeg-config.h", "FFmpeg-config_components.h", "FFmpeg-config.mak", "configure-args.json")
MATERIALS = ("FFmpeg-LGPL-2.1-or-later.txt", "FFmpeg-NOTICE.txt", "FFmpeg-SOURCE.txt")
FILES = set(LIBS + CONFIG_FILES + MATERIALS + ("manifest.json",))
TOOLS = {"cc": "aarch64-unknown-linux-ohos-clang", "cxx": "aarch64-unknown-linux-ohos-clang++",
         "ar": "llvm-ar", "nm": "llvm-nm", "ranlib": "llvm-ranlib", "strip": "llvm-strip",
         "clang": "clang", "readelf": "llvm-readelf"}
REPO = Path(__file__).resolve().parent.parent
_recipe_spec = importlib.util.spec_from_file_location('ffmpeg_source_recipe', Path(__file__).with_name('ffmpeg-source-recipe.py'))
recipes = importlib.util.module_from_spec(_recipe_spec)
_recipe_spec.loader.exec_module(recipes)


def files_for_recipe(recipe):
    recipes.metadata(recipe)
    return FILES | (set(recipes.EXTRA) if recipe == recipes.MODIFIED else set())


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def regular(path):
    require(path.is_file() and not path.is_symlink(), f"要求普通文件（拒绝 symlink）：{path}")


def read_json(path):
    regular(path)
    return json.loads(path.read_text())


def save_json(path, obj):
    path.write_text(json.dumps(obj, indent=2, sort_keys=True) + "\n")


def tree_hashes(root):
    return {str(p.relative_to(root)): sha(p) for p in sorted(root.rglob("*")) if p.is_file()}


def plugin_inputs(repo, recipe="upstream"):
    result = {name: sha(repo / name) for name in ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml", ".cargo/config.toml")}
    for crate in ("frd-video-ffmpeg-plugin", "frd-video-ffmpeg", "frd-core", "frd-media-api", "frd-frame"):
        for path in sorted((repo / "crates" / crate).rglob("*")):
            if path.is_file():
                result[str(path.relative_to(repo))] = sha(path)
    for name in ("tools/build-ffmpeg-ohos.sh", "tools/ohos-ffmpeg-bundle.py"):
        result[name] = sha(repo / name)
    for name in ("tools/ffmpeg-source-recipe.py", str(recipes.PIN), str(recipes.PATCH)):
        result[name] = sha(repo / name)
    recipes.metadata(recipe, repo=repo)
    return result


def tool_paths(sdk):
    return {key: sdk / "native/llvm/bin" / name for key, name in TOOLS.items()}


def sdk_identity(sdk):
    metadata = sdk / "native/oh-uni-package.json"
    package = read_json(metadata)
    require(package.get("apiVersion") == "26" and package.get("version") == SDK_VERSION,
            "仅支持官方 Native API26 / 26.0.0.105 SDK")
    tools = tool_paths(sdk)
    for path in tools.values():
        require(path.is_file() and os.access(path, os.X_OK), f"SDK 缺少可执行工具：{path}")
    version = subprocess.check_output([str(tools["cc"]), "--version"], text=True)
    require("OHOS" in version and f"Target: {TARGET}" in version, "编译器不是官方 OHOS ARM64 clang")
    libc = sdk / "native/sysroot/usr/lib/aarch64-linux-ohos/libc.so"
    regular(libc)
    # libc 来自公开 NDK sysroot；libdl/libm 是静态 archive，实际三库仅依赖 libc.so。
    require(elf(libc)["soname"] == "libc.so", "SDK 公共 libc SONAME 不匹配")
    return {"root": str(sdk), "package_sha256": sha(metadata), "package": package,
            "compiler_version": version.strip(),
            "tools": {key: {"path": str(path), "sha256": sha(path)} for key, path in tools.items()},
            "sysroot_sha256": hashlib.sha256(json.dumps(tree_hashes(sdk / "native/sysroot"), sort_keys=True).encode()).hexdigest(),
            "public_dependencies": {"libc.so": {"path": str(libc), "sha256": sha(libc)}}}


def configure_args(sdk):
    tools = tool_paths(sdk)
    return ["--prefix=/usr", "--arch=aarch64", "--target-os=linux", "--enable-cross-compile"] + [
        f"--{key}={tools[key]}" for key in ("cc", "cxx", "ar", "nm", "ranlib", "strip")] + [
        f"--sysroot={sdk}/native/sysroot", "--disable-static", "--enable-shared", "--enable-pic",
        "--disable-programs", "--disable-doc", "--disable-everything", "--enable-decoder=hevc,h264",
        "--enable-parser=hevc,h264", "--disable-gpl", "--disable-nonfree", "--disable-version3",
        "--disable-autodetect", "--disable-network", "--disable-debug", "--enable-stripping",
        "--disable-avdevice", "--disable-avformat", "--disable-avfilter", "--disable-swresample",
        "--disable-swscale", "--disable-x86asm"]


def elf(path):
    """解析目标 ELF 本身，拒绝宿主 Mach-O/GNU 对象；不依赖待验 manifest。"""
    data = path.read_bytes()
    require(len(data) >= 64 and data[:7] == b"\x7fELF\x02\x01\x01", f"非 ELF64 little-endian：{path}")
    h = struct.unpack_from("<HHIQQQIHHHHHH", data, 16)
    require(h[0] == 3 and h[1] == 183, f"非 AArch64 ET_DYN：{path}")
    phoff, phsize, phcount = h[4], h[8], h[9]
    require(phsize == 56 and 0 < phcount < 65535 and phoff + phcount * phsize <= len(data), "ELF program table 越界")
    programs = [struct.unpack_from("<IIQQQQQQ", data, phoff + i * 56) for i in range(phcount)]
    loads = [p for p in programs if p[0] == 1]
    dynamic = [p for p in programs if p[0] == 2]
    require(loads and len(dynamic) == 1 and not any(p[0] == 3 for p in programs), "ELF PT_LOAD/PT_DYNAMIC/INTERP 不匹配")
    for segment in programs:
        require(segment[2] + segment[5] <= len(data), "ELF segment bytes 越界")
    require(dynamic[0][5] % 16 == 0, "ELF PT_DYNAMIC entry 不匹配")
    dynamic_entries = list(struct.iter_unpack("<qQ", data[dynamic[0][2]:dynamic[0][2] + dynamic[0][5]]))
    terminators = [i for i, (tag, _) in enumerate(dynamic_entries) if tag == 0]
    require(terminators and all(entry == (0, 0) for entry in dynamic_entries[terminators[0]:]),
            "ELF DT_NULL 后存在动态条目或缺少终止符")
    dynamic_entries = dynamic_entries[:terminators[0]]
    dyn_tags = {}
    for tag, value in dynamic_entries:
        if tag in (5, 6, 10, 11):
            require(tag not in dyn_tags, "ELF 重复 dynamic table pointer")
            dyn_tags[tag] = value

    def load_offset(address, size):
        matches = [p[2] + address - p[3] for p in loads if p[3] <= address and address + size <= p[3] + p[5]]
        require(len(matches) == 1, "ELF dynamic pointer 不在唯一 PT_LOAD 内")
        return matches[0]

    require(set(dyn_tags) == {5, 6, 10, 11} and dyn_tags[11] == 24, "ELF 动态字符串/符号指针缺失")
    require(load_offset(dynamic[0][3], dynamic[0][5]) == dynamic[0][2], "ELF PT_DYNAMIC 不在匹配 PT_LOAD 内")
    str_offset = load_offset(dyn_tags[5], dyn_tags[10])
    shoff, shsize, count = h[5], h[10], h[11]
    require(shsize == 64 and 0 < count < 65535 and shoff + count * shsize <= len(data), "ELF section table 越界")
    sections = [struct.unpack_from("<IIQQQQIIQQ", data, shoff + i * 64) for i in range(count)]

    def section(index):
        require(0 <= index < count, "ELF section index 越界")
        s = sections[index]
        require(s[4] + s[5] <= len(data), "ELF section bytes 越界")
        return data[s[4]:s[4] + s[5]]

    def string(strings, offset):
        require(offset < len(strings), "ELF string index 越界")
        end = strings.find(b"\0", offset)
        require(end >= 0, "ELF string 未终止")
        return strings[offset:end].decode("ascii")

    soname, needed, exports, build_ids = [], [], [], []
    dynamic_sections = 0
    symbol_sections = 0
    for i, s in enumerate(sections):
        if s[1] == 6:  # SHT_DYNAMIC
            dynamic_sections += 1
            require(s[4] == dynamic[0][2] and s[5] == dynamic[0][5], "ELF section/program dynamic 不一致")
            require(s[6] < count and sections[s[6]][4] == str_offset and sections[s[6]][5] == dyn_tags[10],
                    "ELF section/program dynstr 不一致")
            strings = section(s[6])
            require(b"GLIBC_" not in strings, "拒绝 GNU libc symbol version")
            raw = section(i)
            require(s[9] == 16 and len(raw) % 16 == 0, "ELF dynamic entry 不匹配")
            for tag, value in dynamic_entries:
                if tag in (1, 14, 15, 29):
                    text = string(strings, value)
                    require(tag not in (15, 29), "拒绝 RPATH/RUNPATH；包使用标准 native-lib 目录")
                    if tag == 1:
                        needed.append(text)
                    else:
                        soname.append(text)
        elif s[1] == 11:  # SHT_DYNSYM
            symbol_sections += 1
            require(s[4] == load_offset(dyn_tags[6], s[5]), "ELF section/program dynsym 不一致")
            require(s[6] < count and sections[s[6]][4] == str_offset and sections[s[6]][5] == dyn_tags[10],
                    "ELF dynsym/dynstr 不一致")
            strings, raw = section(s[6]), section(i)
            require(s[9] == 24 and len(raw) % 24 == 0, "ELF dynsym entry 不匹配")
            for name, info, other, index, value, size in struct.iter_unpack("<IBBHQQ", raw):
                if index and info >> 4 in (1, 2) and info & 15 == 2 and other & 3 == 0:
                    export = string(strings, name)
                    if export == "frd_ffmpeg_get_api_v1":
                        require(any(p[1] & 1 and p[3] <= value < p[3] + p[5] for p in loads),
                                "getter 不在可执行 PT_LOAD 内")
                    exports.append(export)
        elif s[1] == 7:  # SHT_NOTE
            raw, pos = section(i), 0
            while pos + 12 <= len(raw):
                namesz, descsz, kind = struct.unpack_from("<III", raw, pos)
                pos += 12
                name = raw[pos:pos + namesz]
                pos += (namesz + 3) & ~3
                desc = raw[pos:pos + descsz]
                pos += (descsz + 3) & ~3
                require(pos <= len(raw), "ELF note 越界")
                if name == b"GNU\0" and kind == 3:
                    build_ids.append(desc.hex())
    require(dynamic_sections == 1 and symbol_sections == 1, "ELF 动态 section 集合不匹配")
    require(len(soname) <= 1 and len(needed) == len(set(needed)), "重复 SONAME/DT_NEEDED")
    return {"soname": soname[0] if soname else None, "needed": sorted(needed),
            "exports": sorted(exports), "build_ids": build_ids}


def config_check(directory, sdk):
    require(read_json(directory / "configure-args.json") == configure_args(sdk), "configure 参数与固定 OHOS recipe 不符")
    text = (directory / "FFmpeg-config.h").read_text()
    configuration = re.search(r'^#define FFMPEG_CONFIGURATION (".*")$', text, re.M)
    require(configuration is not None and shlex.split(ast.literal_eval(configuration[1])) == configure_args(sdk),
            "FFmpeg 编译配置字符串与固定 recipe 不符")
    for name, value in {"ARCH_AARCH64": 1, "HAVE_NEON": 1, "HAVE_NEON_EXTERNAL": 1, "HAVE_NEON_INLINE": 1,
                        "CONFIG_GPL": 0, "CONFIG_NONFREE": 0, "CONFIG_VERSION3": 0, "CONFIG_NETWORK": 0,
                        "CONFIG_STATIC": 0, "CONFIG_SHARED": 1}.items():
        require(re.search(rf"^#define {name} {value}$", text, re.M), f"FFmpeg 配置不符：{name}")
    components = (directory / "FFmpeg-config_components.h").read_text()
    enabled = set(re.findall(r"^#define (CONFIG_\w+) 1$", components, re.M))
    require(enabled == {"CONFIG_H264_DECODER", "CONFIG_HEVC_DECODER", "CONFIG_H264_PARSER", "CONFIG_HEVC_PARSER"},
            f"FFmpeg component 集合不符：{enabled}")
    make = (directory / "FFmpeg-config.mak").read_text()
    require("prefix=/usr\n" in make and "ARCH=aarch64\n" in make, "FFmpeg 安装/架构配置不符")


def base_manifest(sdk, identity, rust, recipe="upstream"):
    return {"schema": 2 if recipe == recipes.MODIFIED else 1, "target": TARGET, "target_os": "linux", "target_env": "ohos", "ffmpeg_version": VERSION,
            "source": recipes.metadata(recipe),
            "sdk": identity, "rust": rust, "configure": configure_args(sdk),
            "logical_prefix": "/usr", "plugin_abi": 1, "thread_policy": [1, 0],
            "plugin_inputs": plugin_inputs(REPO, recipe)}


def manifest_check(manifest, sdk, repo=None):
    recipe = recipes.identify(manifest)
    require(manifest.get("target") == TARGET and manifest.get("target_os") == "linux"
            and manifest.get("target_env") == "ohos", "来源目标不匹配")
    require(manifest.get("ffmpeg_version") == VERSION, "FFmpeg 来源不匹配")
    require(manifest.get("configure") == configure_args(sdk) and manifest.get("logical_prefix") == "/usr",
            "manifest configure 不匹配")
    require(manifest.get("plugin_abi") == 1 and manifest.get("thread_policy") == [1, 0], "OHOS ABI/线程策略不匹配")
    require(manifest.get("sdk") == sdk_identity(sdk), "SDK 来源 bytes/工具不匹配")
    rust = manifest.get("rust", {})
    for key in ("rustc", "rustdoc", "cargo"):
        require(rust.get(key, {}).get("version", "").startswith(f"{key} 1.96.0 "), "要求 coherent Rust 1.96.0")
        if repo:
            require(sha(rust[key]["path"]) == rust[key]["sha256"], f"Rust 工具 bytes 不匹配：{key}")
    if repo:
        require(manifest.get("plugin_inputs") == plugin_inputs(repo, recipe), "plugin/workspace/recipe 来源不匹配")


def verify_inputs(prefix, sdk, repo):
    m = read_json(prefix / "ohos-ffmpeg-inputs.json")
    manifest_check(m, sdk, repo)
    config_check(prefix, sdk)
    recipe = recipes.identify(m)
    verify_source_materials(prefix, recipe)
    for path in (prefix / "include").rglob("*"):
        require(not path.is_symlink(), "FFmpeg header 拒绝 symlink")
    require(m.get("headers") == tree_hashes(prefix / "include"), "FFmpeg headers bytes 不匹配")
    for name, macro, major, header in (("libavcodec", "LIBAVCODEC_VERSION_MAJOR", 62, "version_major.h"),
                                      ("libavutil", "LIBAVUTIL_VERSION_MAJOR", 60, "version.h")):
        version = (prefix / "include" / name / header).read_text()
        require(re.search(rf"^#define {macro} +{major}$", version, re.M), "FFmpeg header major 不匹配")
    for name in LIBS[:2]:
        path = prefix / "lib" / name
        # make install 的版本 symlink 仅允许在私有编译输入中；发布库必须普通文件。
        require(path.resolve().parent == (prefix / "lib").resolve(), "输入库逃逸 lib 目录")
        if recipe == recipes.MODIFIED and name == "libavcodec.so.62":
            recipes.kernel_identity(path.resolve(), m.get("h264_dc_kernel", {}))
        info = elf(path)
        require(info["soname"] == name and info["needed"] == sorted(DEPS[name]), f"OHOS 输入 ELF 闭包不匹配：{name}")
        configuration = re.search(r'^#define FFMPEG_CONFIGURATION (".*")$', (prefix / "FFmpeg-config.h").read_text(), re.M)
        require(ast.literal_eval(configuration[1]).encode() + b"\0" in path.read_bytes(), "OHOS 输入库/config 字节不匹配")
        require(m["libraries"][name] == sha(path), f"OHOS 输入库 bytes 不匹配：{name}")
    for name in CONFIG_FILES:
        regular(prefix / name)
        require(m["config_files"][name] == sha(prefix / name), "输入配置 bytes 不匹配")


def verify_bundle(bundle, sdk):
    require(bundle.is_dir() and not bundle.is_symlink(), "bundle 必须为普通目录")
    m = read_json(bundle / "manifest.json")
    recipe = recipes.identify(m)
    files = files_for_recipe(recipe)
    require({p.name for p in bundle.iterdir()} == files, "bundle 文件集合不匹配")
    for path in bundle.iterdir():
        regular(path)
    m = read_json(bundle / "manifest.json")
    manifest_check(m, sdk)
    config_check(bundle, sdk)
    require(set(m.get("files", {})) == files - {"manifest.json"}, "manifest 文件集合不匹配")
    for name in files - {"manifest.json"}:
        require(m["files"][name] == sha(bundle / name), f"manifest hash 不匹配：{name}")
    summaries = {}
    for name in LIBS:
        info = elf(bundle / name)
        require(info["soname"] == name, f"SONAME 不匹配：{name}")
        require(info["needed"] == sorted(DEPS[name]), f"依赖闭包不匹配：{name}: {info['needed']}")
        if name == LIBS[2]:
            require("frd_ffmpeg_get_api_v1" in info["exports"], "缺少已定义 getter export")
        if name in LIBS[:2]:
            configuration = re.search(r'^#define FFMPEG_CONFIGURATION (".*")$',
                                      (bundle / "FFmpeg-config.h").read_text(), re.M)
            require(configuration is not None, "缺少 FFMPEG_CONFIGURATION")
            require(ast.literal_eval(configuration[1]).encode() + b"\0" in (bundle / name).read_bytes(),
                    "库 bytes 缺少所记录的 configure 字符串")
        summaries[name] = info
    require(m.get("elf") == summaries, "manifest ELF 描述与 bytes 不匹配")
    # 来源材料也要核对固定内容，不能用改写的 manifest 自证许可/源码身份。
    require((bundle / "FFmpeg-LGPL-2.1-or-later.txt").read_bytes() ==
            (REPO / "third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1").read_bytes(), "LGPL 许可不匹配")
    require((bundle / "FFmpeg-NOTICE.txt").read_text() == notice(recipe), "NOTICE 不匹配")
    require((bundle / "FFmpeg-SOURCE.txt").read_text() == source_material(recipe), "源码取得材料不匹配")
    verify_source_materials(bundle, recipe)
    if recipe == recipes.MODIFIED:
        recipes.kernel_identity(bundle / "libavcodec.so.62", m.get("h264_dc_kernel", {}))
    return summaries


def verify_source_materials(directory, recipe):
    for name in recipes.EXTRA:
        path = directory / name
        if recipe == recipes.MODIFIED:
            regular(path)
            require(path.read_bytes() == recipes.materials()[name], "受信源码材料 bytes 不匹配")
        else:
            require(not path.exists() and not path.is_symlink(), "原样来源不能携带 patch 材料")


def notice(recipe="upstream"):
    label = "unmodified upstream source" if recipe == "upstream" else "local AArch64 8-bit H264 luma DC patch"
    return (f"FFmpeg {VERSION}; LGPL-2.1-or-later; {label}.\n"
            "H.264/HEVC decoder/parser only; shared PIC; AArch64 NEON enabled.\n"
            "No GPL/nonfree/version3/network/programs/autodetect.\n"
            "OHOS ARM64 plugin ABI v1; software thread_count=1, thread_type=0.\n"
            "Build/ELF proof only; normal HAP loading, pixel parity, throughput and live interoperability pending.\n")


def source_material(recipe="upstream"):
    if recipe == recipes.MODIFIED:
        return (f"Base upstream source: {URL}\nSHA-256: {ARCHIVE_SHA}\n"
                f"Modified recipe: {recipes.MODIFIED}; local patch is not signed by upstream.\n"
                f"FFmpeg-CHANGES.diff SHA-256: {recipes.PATCH_SHA}\n"
                f"FFmpeg-SOURCE-RECIPE.json SHA-256: {recipes.PIN_SHA}\n"
                f"Corresponding source asset: {recipes.ASSET}\n"
                "Distributors must publish this complete base archive + patch + recipe + builder asset beside each binary on the same release/download page.\n"
                "Extract the pinned base archive into a fresh tree; verify preimages in FFmpeg-SOURCE-RECIPE.json.\n"
                "Apply: patch --dry-run --batch --forward --fuzz=0 -p1 < FFmpeg-CHANGES.diff, then repeat without --dry-run.\n"
                "Reject offsets/fuzz and verify postimages and unchanged oracle before configure.\n"
                "Rebuild from matching FreeRemoteDesk source with tools/build-ffmpeg-ohos.sh --archive ffmpeg-8.1.2.tar.xz --source-recipe frd-h264dc-aarch64-v1.\n"
                "Exact configure: configure-args.json; SDK/tool/Rust/workspace/output digests: manifest.json.\n"
                "Retain LGPL license, patch, recipe and these source materials when redistributing.\n")
    return (f"Upstream source: {URL}\nSHA-256: {ARCHIVE_SHA}\n"
            "Unmodified source; no patches. Retrieve the exact archive using:\n"
            f"curl --fail --location '{URL}' -o ffmpeg-{VERSION}.tar.xz\n"
            "Verify the SHA-256 above before extraction.\n"
            "Exact configure arguments: configure-args.json. Build configuration: FFmpeg-config* files.\n"
            "Recipe: tools/build-ffmpeg-ohos.sh and tools/ohos-ffmpeg-bundle.py in FreeRemoteDesk.\n"
            "SDK/tool/Rust/workspace inputs and output digests: manifest.json.\n"
            "Rebuild shared libraries with this recipe; retain LGPL license and these source materials when redistributing.\n")


def rust_identity():
    result = {}
    for name in ("cargo", "rustc", "rustdoc"):
        path = Path(subprocess.check_output(["rustup", "which", "--toolchain", "1.96.0", name], text=True).strip())
        version = subprocess.check_output([str(path), "--version"], text=True).strip()
        require(version.startswith(f"{name} 1.96.0 "), f"缺少 Rust 1.96.0 {name}")
        result[name] = {"path": str(path), "version": version, "sha256": sha(path)}
    return result


def run(command, cwd, log, env=None):
    print(f"执行：{command[0]}；日志：{log}", flush=True)
    with log.open("w") as output:
        subprocess.run([str(arg) for arg in command], cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT, check=True)


def build(args, sdk):
    require(sys.version_info >= (3, 12), "构建需要 Python 3.12 或更新版本（安全 tar 解包）")
    require(platform.system() == "Darwin" and platform.machine() == "arm64", "仅支持 macOS ARM64 构建主机")
    require(args.target == TARGET, "仅支持 aarch64-unknown-linux-ohos")
    identity, rust = sdk_identity(sdk), rust_identity()
    root = Path(args.build_root).resolve()
    root.mkdir(parents=True, exist_ok=True)
    archive = Path(args.archive).resolve() if args.archive else root / f"ffmpeg-{VERSION}.tar.xz"
    if not archive.exists():
        require(not args.archive, "指定源码 archive 不存在")
        with tempfile.NamedTemporaryFile(dir=root, delete=False) as output:
            temporary = Path(output.name)
        try:
            with urllib.request.urlopen(URL, timeout=120) as response, temporary.open("wb") as output:
                shutil.copyfileobj(response, output)
            require(sha(temporary) == ARCHIVE_SHA, "FFmpeg archive SHA256 不匹配")
            os.replace(temporary, archive)
        finally:
            temporary.unlink(missing_ok=True)
    regular(archive)
    require(sha(archive) == ARCHIVE_SHA, "FFmpeg archive SHA256 不匹配")
    attempt = Path(tempfile.mkdtemp(prefix="attempt-", dir=root))
    print(f"私有本次构建：{attempt}", flush=True)
    source, _ = recipes.prepare(archive, attempt / "source", args.source_recipe)
    dest = attempt / "dest"
    prefix = dest / "usr"
    config = configure_args(sdk)
    # 禁止环境偷偷注入主机库、编译/链接选项或旧 Cargo artifact。
    environment = os.environ.copy()
    for name in list(environment):
        if name in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CFLAGS", "CXXFLAGS", "LDFLAGS", "LIBRARY_PATH", "CPATH",
                    "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "DYLD_LIBRARY_PATH", "LD_LIBRARY_PATH", "CC", "CXX", "AR",
                    "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTFLAGS", "CARGO_BUILD_RUSTC",
                    "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER") or name.startswith(("CARGO_TARGET_", "CC_", "CXX_", "AR_", "CFLAGS_", "CXXFLAGS_")):
            del environment[name]
    run(["./configure", *config], source, attempt / "configure.log", environment)
    run(["make", "-j", str(args.jobs)], source, attempt / "make.log", environment)
    run(["make", f"DESTDIR={dest}", "install"], source, attempt / "install.log", environment)
    for src, dst in zip(("config.h", "config_components.h", "ffbuild/config.mak"), CONFIG_FILES[:3]):
        shutil.copyfile(source / src, prefix / dst)
    save_json(prefix / "configure-args.json", config)
    config_check(prefix, sdk)
    require((source / "COPYING.LGPLv2.1").read_bytes() ==
            (REPO / "third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1").read_bytes(), "上游 LGPL 许可与仓库固定许可不符")
    # 记录实际 AArch64 汇编对象与 NEON dispatch 符号；并非设备运行/性能证明。
    objects = sorted(str(p.relative_to(source)) for p in source.rglob("*.o") if "/aarch64/" in str(p.relative_to(source)))
    require(any("h264" in p for p in objects) and any("hevc" in p for p in objects), "缺少 H264/HEVC AArch64 编译对象")
    manifest = base_manifest(sdk, identity, rust, args.source_recipe)
    if args.source_recipe == recipes.MODIFIED:
        for name, data in recipes.materials().items():
            (prefix / name).write_bytes(data)
        manifest["h264_dc_kernel"] = recipes.kernel_identity(source / "libavcodec/libavcodec.so.62")
    manifest["assembly_objects"] = {name: sha(source / name) for name in objects}
    manifest["headers"] = tree_hashes(prefix / "include")
    manifest["libraries"] = {name: sha(prefix / "lib" / name) for name in LIBS[:2]}
    manifest["config_files"] = {name: sha(prefix / name) for name in CONFIG_FILES}
    save_json(prefix / "ohos-ffmpeg-inputs.json", manifest)
    verify_inputs(prefix, sdk, REPO)
    environment.update({"FFMPEG_DIR": str(prefix), "OHOS_SDK_ROOT": str(sdk), "RUSTC": rust["rustc"]["path"],
                        "RUSTDOC": rust["rustdoc"]["path"], "CARGO_TARGET_DIR": str(attempt / "cargo"),
                        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER": str(tool_paths(sdk)["cc"]),
                        "CC_aarch64_unknown_linux_ohos": str(tool_paths(sdk)["cc"]),
                        "AR_aarch64_unknown_linux_ohos": str(tool_paths(sdk)["ar"])})
    command = [rust["cargo"]["path"], "build", "--locked", "--release", "--target", TARGET,
               "-p", "frd-video-ffmpeg-plugin", "--features", "native-ffmpeg"]
    manifest["cargo_command"] = command
    run(command, REPO, attempt / "plugin.log", environment)
    bundle = attempt / "bundle"
    bundle.mkdir()
    for name in LIBS[:2]:
        shutil.copyfile(prefix / "lib" / name, bundle / name)
    shutil.copyfile(attempt / "cargo" / TARGET / "release" / LIBS[2], bundle / LIBS[2])
    for name in CONFIG_FILES:
        shutil.copyfile(prefix / name, bundle / name)
    shutil.copyfile(REPO / "third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1", bundle / MATERIALS[0])
    (bundle / MATERIALS[1]).write_text(notice(args.source_recipe))
    (bundle / MATERIALS[2]).write_text(source_material(args.source_recipe))
    if args.source_recipe == recipes.MODIFIED:
        for name, data in recipes.materials().items():
            (bundle / name).write_bytes(data)
        asset = attempt / recipes.ASSET
        recipes.write_asset(archive, asset)
        manifest["corresponding_source"] = {"asset": asset.name, "sha256": sha(asset)}
    manifest["files"] = {p.name: sha(p) for p in bundle.iterdir()}
    manifest["elf"] = {name: elf(bundle / name) for name in LIBS}
    save_json(bundle / "manifest.json", manifest)
    verify_bundle(bundle, sdk)
    for name in LIBS:
        run([tool_paths(sdk)["readelf"], "-h", "-d", "--dyn-syms", bundle / name], REPO, attempt / f"{name}-elf.txt")
    # 不替换已有目录：每次以 unique attempt 发布完整已验证包；失败保留旧包和诊断。
    published = root / f"bundle-{attempt.name}"
    os.rename(bundle, published)
    print(f"已验证静态 bundle：{published}\n未执行目标代码；不是 DecoderReady。", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", default=os.environ.get("OHOS_SDK_ROOT", DEFAULT_SDK))
    commands = parser.add_subparsers(dest="action", required=True)
    b = commands.add_parser("build", help="独立 clean 构建（输出路径每次唯一）")
    b.add_argument("--sdk", default=argparse.SUPPRESS)
    b.add_argument("--build-root", default=str(REPO / "target/ffmpeg-ohos"))
    b.add_argument("--archive", help="可选已缓存、仍须验证固定 hash 的上游源码")
    b.add_argument("--source-recipe", choices=recipes.CHOICES, default="upstream")
    b.add_argument("--target", default=TARGET)
    b.add_argument("--jobs", type=int, default=2)
    v = commands.add_parser("verify", help="静态 bundle 门，不执行 ELF")
    v.add_argument("bundle", type=Path)
    i = commands.add_parser("verify-inputs", help="plugin 的 OHOS SDK/库/header/config/source 编译输入门")
    i.add_argument("prefix", type=Path)
    i.add_argument("--repo", type=Path, required=True)
    args = parser.parse_args()
    sdk = Path(args.sdk).resolve()
    try:
        if args.action == "build":
            require(args.jobs > 0, "jobs 必须大于零")
            build(args, sdk)
        elif args.action == "verify":
            print(json.dumps(verify_bundle(args.bundle, sdk), indent=2))
        else:
            verify_inputs(args.prefix, sdk, args.repo)
    except (ValueError, OSError, KeyError, UnicodeError, SyntaxError, struct.error, subprocess.CalledProcessError) as error:
        print(f"OHOS FFmpeg 验证/构建失败：{error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
