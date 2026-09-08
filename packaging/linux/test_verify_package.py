"""合成包门禁回归；readelf/desktop 工具注入仅用于检查拒绝路径，不是 Linux 运行证明。"""
import importlib.util
from pathlib import Path
import shutil
import struct
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("verifier", Path(__file__).with_name("verify_package.py"))
v = importlib.util.module_from_spec(spec)
spec.loader.exec_module(v)
REPO = Path(__file__).resolve().parents[2]

class PackageValidation(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve() / "package"
        self.root.mkdir(mode=0o755)
        self.add("README.md", REPO / "packaging/linux/README.md")
        self.add("share/applications/freeremotedesk.desktop", REPO / "packaging/linux/freeremotedesk.desktop")
        for dest, source in {
            "Icon-Provenance.md": "assets/app-icon/README.md",
            "Material-Symbols-APACHE-2.0.txt": "assets/ui-icons/LICENSE-APACHE-2.0.txt",
            "Noto-Sans-SC-OFL.txt": "assets/fonts/noto-sans-sc/OFL.txt",
            "FreeRDP-APACHE-2.0.txt": "packaging/windows/licenses/FreeRDP-APACHE-2.0.txt",
            "FreeRDP-NOTICE.txt": "packaging/windows/licenses/FreeRDP-NOTICE.txt",
        }.items():
            self.add("share/licenses/FreeRemoteDesk/" + dest, REPO / source)
        for size in (16, 32, 48, 64, 128, 256, 512):
            name = f"hicolor/{size}x{size}/apps/freeremotedesk.png"
            self.add("share/icons/" + name, REPO / "assets/app-icon/linux" / name)
        self.codec = self.root / "codecs/ffmpeg-8.1.2/linux-x86_64"
        self.codec.mkdir(parents=True)
        for name in v.CODECS:
            (self.codec / name).write_bytes(b"fixture")
        shutil.copyfile(REPO / "third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1", self.codec / v.CODECS[0])
        (self.codec / "FFmpeg-NOTICE.txt").write_text(
            "FFmpeg 8.1.2,\nArchitecture: linux-x86_64\n"
            "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c\n")
        header = bytearray(64)
        header[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<HH", header, 16, 3, 62)
        (self.root / "freeremotedesk-linux").write_bytes(header)
        (self.root / "freeremotedesk-linux").chmod(0o755)

    def tearDown(self):
        self.temp.cleanup()

    def add(self, name, source):
        dest = self.root / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, dest)

    def verify(self, dynamic="(NEEDED) [libc.so.6]"):
        with patch.object(v.subprocess, "check_output", side_effect=[dynamic, "[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]", "(NEEDED) [libavutil.so.60]", "(NEEDED) [libc.so.6]", "(NEEDED) [libavcodec.so.62]"]), patch.object(v.subprocess, "run"):
            v.verify(str(self.root), "linux-x86_64", str(REPO))

    def test_expected_static_contract(self):
        self.verify()

    def test_protocol_system_zlib_dependency_is_accepted(self):
        self.verify("(NEEDED) [libc.so.6]\n(NEEDED) [libz.so.1]")

    def test_missing_binary_rejected(self):
        (self.root / "freeremotedesk-linux").unlink()
        with self.assertRaisesRegex(ValueError, "文件集合"):
            self.verify()

    def test_extra_directory_rejected(self):
        (self.root / "unexpected").mkdir()
        with self.assertRaisesRegex(ValueError, "目录集合"):
            self.verify()

    def test_symlink_resource_rejected(self):
        path = self.root / "README.md"
        path.unlink()
        path.symlink_to(REPO / "packaging/linux/README.md")
        with self.assertRaises(ValueError):
            self.verify()

    def test_writable_codec_rejected(self):
        (self.codec / "libavutil.so.60").chmod(0o666)
        with self.assertRaisesRegex(ValueError, "权限"):
            self.verify()

    def test_wrong_elf_architecture_rejected(self):
        path = self.root / "freeremotedesk-linux"
        header = bytearray(path.read_bytes())
        struct.pack_into("<H", header, 18, 183)
        path.write_bytes(header)
        with self.assertRaisesRegex(ValueError, "架构"):
            self.verify()

    def test_unknown_dependency_rejected(self):
        with self.assertRaisesRegex(ValueError, "动态依赖"):
            self.verify("(NEEDED) [libc.so.6]\n(NEEDED) [/tmp/custom.so]")

    def test_rpath_rejected(self):
        with self.assertRaisesRegex(ValueError, "RPATH"):
            self.verify("(NEEDED) [libc.so.6]\n(RUNPATH) [/home/build]")

    def test_modified_desktop_entry_rejected(self):
        (self.root / "share/applications/freeremotedesk.desktop").write_text("Exec=other")
        with self.assertRaisesRegex(ValueError, "固定源"):
            self.verify()

if __name__ == "__main__":
    unittest.main()
