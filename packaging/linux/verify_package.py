#!/usr/bin/env python3
"""静态包门禁；不执行被检查的 ELF，也不把静态检查称为运行证明。"""
import os
import hashlib
from pathlib import Path
import re
import stat
import struct
import subprocess
import sys

PROFILES = {
    "linux-x86": (1, 3, "/lib/ld-linux.so.2"),
    "linux-x86_64": (2, 62, "/lib64/ld-linux-x86-64.so.2"),
    "linux-aarch64": (2, 183, "/lib/ld-linux-aarch64.so.1"),
}
CODECS = ("FFmpeg-LGPL-2.1-or-later.txt", "FFmpeg-NOTICE.txt", "libavcodec.so.62",
          "libavutil.so.60", "libfreeremotedesk_ffmpeg.so")
SYSTEM_LIBRARIES = {
    # flate2/libz-sys 由 Apple 协议及 IronRDP SSPI 依赖图引入。
    "libz.so.1",
    "libc.so.6", "libm.so.6", "libgcc_s.so.1", "libstdc++.so.6", "libpthread.so.0", "libdl.so.2",
    "librt.so.1", "libutil.so.1", "libudev.so.1", "libfontconfig.so.1", "libfreetype.so.6",
    "libX11.so.6", "libX11-xcb.so.1", "libxcb.so.1", "libXcursor.so.1", "libXi.so.6",
    "libXrandr.so.2", "libXrender.so.1", "libXfixes.so.3", "libxkbcommon.so.0",
    "libxkbcommon-x11.so.0", "libwayland-client.so.0", "libwayland-cursor.so.0",
    "libwayland-egl.so.1", "libEGL.so.1", "libGL.so.1", "libvulkan.so.1",
    # GTK4/GLArea 产品壳的显式运行时依赖；仅允许发行版提供这些固定 SONAME。
    "libgtk-4.so.1", "libgdk-4.so.1", "libgdk_pixbuf-2.0.so.0",
    "libgio-2.0.so.0", "libgobject-2.0.so.0", "libglib-2.0.so.0",
    "libpango-1.0.so.0", "libpangocairo-1.0.so.0", "libpangoft2-1.0.so.0",
    "libcairo.so.2", "libcairo-gobject.so.2", "libgraphene-1.0.so.0",
    "libepoxy.so.0", "libharfbuzz.so.0", "libfribidi.so.0", "libthai.so.0",
    "libdatrie.so.1", "libXdamage.so.1", "libXcomposite.so.1", "libXext.so.6",
    "ld-linux.so.2", "ld-linux-x86-64.so.2", "ld-linux-aarch64.so.1",
}

def require(condition, message):
    if not condition:
        raise ValueError(message)


def verify(package, platform, repo):
    require(platform in PROFILES, "不支持的 Linux package profile")
    raw = os.path.abspath(package) if not os.path.isabs(package) else package
    # 不用 resolve 隐藏链接；词法路径中的 . 和 .. 同样拒绝。
    original = package.split("/")
    require(not any(p in (".", "..") for p in original), "包路径不得包含 . 或 ..")
    root = Path(raw)
    for component in (root, *root.parents):
        require(not component.is_symlink(), f"路径组件不得为符号链接：{component}")
    require(root.is_dir(), "缺少完整客户端包目录")
    repo = Path(repo)
    fixed = {
        "README.md": repo / "packaging/linux/README.md",
        "share/applications/freeremotedesk.desktop": repo / "packaging/linux/freeremotedesk.desktop",
        "share/licenses/FreeRemoteDesk/Icon-Provenance.md": repo / "assets/app-icon/README.md",
        "share/licenses/FreeRemoteDesk/Material-Symbols-APACHE-2.0.txt": repo / "assets/ui-icons/LICENSE-APACHE-2.0.txt",
        "share/licenses/FreeRemoteDesk/Noto-Sans-SC-OFL.txt": repo / "assets/fonts/noto-sans-sc/OFL.txt",
        "share/fonts/freeremotedesk/NotoSansSC-VariableFont_wght.ttf": repo / "assets/fonts/noto-sans-sc/NotoSansSC-VariableFont_wght.ttf",
    }
    for name in ("FreeRDP-APACHE-2.0.txt", "FreeRDP-NOTICE.txt"):
        fixed[f"share/licenses/FreeRemoteDesk/{name}"] = repo / "packaging/windows/licenses" / name
    for size in (16, 32, 48, 64, 128, 256, 512):
        relative = f"hicolor/{size}x{size}/apps/freeremotedesk.png"
        fixed[f"share/icons/{relative}"] = repo / "assets/app-icon/linux" / relative
    codec_prefix = f"codecs/ffmpeg-8.1.2/{platform}"
    files = set(fixed) | {"freeremotedesk-linux"} | {f"{codec_prefix}/{name}" for name in CODECS}
    dirs = {"."}
    for name in files:
        dirs.update(str(p) for p in Path(name).parents)
    owner = root.stat().st_uid
    actual_files, actual_dirs = set(), {"."}
    for entry in (root, *root.rglob("*")):
        metadata = entry.lstat()
        relative = str(entry.relative_to(root))
        require(metadata.st_uid == owner, f"包所有者不一致：{relative}")
        require(metadata.st_mode & 0o7022 == 0, f"包对象权限过宽：{relative}")
        require(not stat.S_ISLNK(metadata.st_mode), f"包不得包含符号链接：{relative}")
        if stat.S_ISDIR(metadata.st_mode):
            actual_dirs.add(relative)
        else:
            require(stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1,
                    f"包必须是普通独立文件：{relative}")
            actual_files.add(relative)
    require(actual_files == files, f"包文件集合不匹配：缺少 {files - actual_files}，多出 {actual_files - files}")
    require(actual_dirs == dirs, f"包目录集合不匹配：{actual_dirs ^ dirs}")
    for name, source in fixed.items():
        require((root / name).read_bytes() == source.read_bytes(), f"包资源不是当前固定源：{name}")
    font = root / "share/fonts/freeremotedesk/NotoSansSC-VariableFont_wght.ttf"
    require(font.stat().st_size == 17773248, "随包 Noto Sans SC 字体大小不匹配")
    require(hashlib.sha256(font.read_bytes()).hexdigest() ==
            "e80613a35583f59b46dbf6cc2eb640f3db0bb0f53fa7f6fbaa7b09faf20e5172",
            "随包 Noto Sans SC 字体摘要不匹配")
    require(stat.S_IMODE(font.stat().st_mode) == 0o644, "随包字体权限不匹配")
    notice = (root / codec_prefix / "FFmpeg-NOTICE.txt").read_text()
    require("FFmpeg 8.1.2," in notice and f"Architecture: {platform}\n" in notice,
            "FFmpeg notice 版本或架构不匹配")
    require("464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c" in notice,
            "FFmpeg 来源摘要不匹配")
    require((root / codec_prefix / "FFmpeg-LGPL-2.1-or-later.txt").read_bytes() ==
            (repo / "third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1").read_bytes(), "FFmpeg 许可证不匹配")
    binary = root / "freeremotedesk-linux"
    require(binary.stat().st_mode & 0o111 != 0, "客户端没有执行权限")
    with binary.open("rb") as stream:
        header = stream.read(64)
    elf_class, machine, interpreter = PROFILES[platform]
    require(len(header) >= 52 and header[:4] == b"\x7fELF" and header[4] == elf_class
            and header[5] == 1 and header[6] == 1, "客户端 ELF 类别或字节序错误")
    elf_type, actual_machine = struct.unpack_from("<HH", header, 16)
    require(actual_machine == machine and elf_type in (2, 3), "客户端 ELF 架构或类型错误")
    dynamic = subprocess.check_output(["readelf", "-d", str(binary)], text=True, env={**os.environ, "LC_ALL": "C"})
    needed = re.findall(r"\(NEEDED\).*\[([^\]]+)\]", dynamic)
    require(needed and "libc.so.6" in needed, "客户端缺少动态运行库依赖")
    require(set(needed) <= SYSTEM_LIBRARIES, f"客户端含未审核的动态依赖：{set(needed) - SYSTEM_LIBRARIES}")
    require(not re.search(r"\((?:RPATH|RUNPATH)\)", dynamic), "客户端不得依赖构建机 RPATH/RUNPATH")
    program = subprocess.check_output(["readelf", "-l", str(binary)], text=True, env={**os.environ, "LC_ALL": "C"})
    require(re.findall(r"Requesting program interpreter: ([^\]]+)\]", program) == [interpreter],
            "客户端 ELF interpreter 与目标架构不匹配")
    for name in ("libavcodec.so.62", "libavutil.so.60", "libfreeremotedesk_ffmpeg.so"):
        library_dynamic = subprocess.check_output(
            ["readelf", "-d", str(root / codec_prefix / name)], text=True,
            env={**os.environ, "LC_ALL": "C"})
        library_needed = set(re.findall(r"\(NEEDED\).*\[([^\]]+)\]", library_dynamic))
        allowed = SYSTEM_LIBRARIES | {"libavcodec.so.62", "libavutil.so.60"}
        require(library_needed <= allowed, f"随包库含未审核动态依赖：{name}: {library_needed - allowed}")
    subprocess.run(["desktop-file-validate", str(root / "share/applications/freeremotedesk.desktop")], check=True)


if __name__ == "__main__":
    try:
        require(len(sys.argv) == 4, "用法：verify_package.py <package> <platform> <repo>")
        verify(*sys.argv[1:])
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(f"Linux 完整客户端包验证失败：{error}")
