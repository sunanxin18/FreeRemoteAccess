"""合成包门禁回归；readelf/desktop 工具注入仅用于检查拒绝路径，不是 Linux 运行证明。"""
import importlib.util
import json
import os
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
        self.add(
            "share/fonts/freeremotedesk/NotoSansSC-VariableFont_wght.ttf",
            REPO / "assets/fonts/noto-sans-sc/NotoSansSC-VariableFont_wght.ttf",
        )
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
        self.platform = "linux-x86_64"
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

    def switch_profile(self, platform):
        destination = self.codec.with_name(platform)
        self.codec.rename(destination)
        self.codec = destination
        self.platform = platform
        (self.codec / "FFmpeg-NOTICE.txt").write_text(
            f"FFmpeg 8.1.2,\nArchitecture: {platform}\n"
            "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c\n")
        header = bytearray(64)
        elf_class, machine, _ = v.PROFILES[platform]
        header[:7] = b"\x7fELF" + bytes((elf_class, 1, 1))
        struct.pack_into("<HH", header, 16, 3, machine)
        (self.root / "freeremotedesk-linux").write_bytes(header)

    def verify(self, dynamic="(NEEDED) [libc.so.6]"):
        with patch.dict(os.environ, {k: val for k, val in os.environ.items() if k != "FRD_FFMPEG_SOURCE_RECIPE"}, clear=True), patch.object(v.subprocess, "check_output", side_effect=[dynamic, f"[Requesting program interpreter: {v.PROFILES[self.platform][2]}]", "(NEEDED) [libavutil.so.60]", "(NEEDED) [libc.so.6]", "(NEEDED) [libavcodec.so.62]"]), patch.object(v.subprocess, "run"):
            v.verify(str(self.root), self.platform, str(REPO))

    def test_all_three_upstream_profile_contracts(self):
        for platform in v.PROFILES:
            with self.subTest(platform=platform):
                if self.platform != platform:
                    self.switch_profile(platform)
                self.verify()

    def test_modified_recipe_on_x86_cannot_authorize_extra_materials(self):
        (self.codec / "FFmpeg-PROVENANCE.json").write_text(json.dumps({'recipe': 'frd-h264dc-aarch64-v1'}))
        with self.assertRaisesRegex(ValueError, "AArch64"):
            self.verify()

    def test_expected_static_contract(self):
        self.verify()

    def test_protocol_system_zlib_dependency_is_accepted(self):
        self.verify("(NEEDED) [libc.so.6]\n(NEEDED) [libz.so.1]")

    def test_gtk_runtime_dependencies_are_explicitly_accepted(self):
        dependencies = "\n".join(
            f"(NEEDED) [{name}]"
            for name in (
                "libgtk-4.so.1",
                "libgdk-4.so.1",
                "libgio-2.0.so.0",
                "libgobject-2.0.so.0",
                "libglib-2.0.so.0",
                "libpango-1.0.so.0",
                "libpangocairo-1.0.so.0",
                "libcairo.so.2",
                "libgraphene-1.0.so.0",
                "libepoxy.so.0",
            )
        )
        self.verify(f"(NEEDED) [libc.so.6]\n{dependencies}")

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



class ModifiedPackageValidation(PackageValidation):
    # 继承完整synthetic package资源构造；只新增真实ELF/完整来源材料路径。
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location('linux_recipe_tests', REPO / 'tools/tests/test_linux_ffmpeg_recipe.py')
        tests = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(tests)
        cls.fixtures = tests.ELFAndMaterials
        cls.fixtures.setUpClass()

    @classmethod
    def tearDownClass(cls):
        cls.fixtures.tearDownClass()

    def setUp(self):
        super().setUp()
        self.switch_profile('linux-aarch64')
        for path in self.codec.iterdir():
            path.unlink()
        for path in self.fixtures.good.iterdir():
            shutil.copyfile(path, self.codec / path.name)

    def test_all_three_upstream_profile_contracts(self):
        # modified不能被重命名为其他architecture，任何profile改写都拒绝。
        for platform in ('linux-x86', 'linux-x86_64'):
            with self.subTest(platform=platform):
                self.switch_profile(platform)
                with self.assertRaises(ValueError):
                    self.verify()

    def test_modified_recipe_on_x86_cannot_authorize_extra_materials(self):
        self.switch_profile('linux-x86_64')
        with self.assertRaises(ValueError):
            self.verify()

    def test_wrong_elf_architecture_rejected(self):
        header = bytearray((self.root / 'freeremotedesk-linux').read_bytes())
        struct.pack_into('<H', header, 18, 62)
        (self.root / 'freeremotedesk-linux').write_bytes(header)
        with self.assertRaisesRegex(ValueError, '架构'):
            self.verify()

    def test_modified_package_exact_material_collection(self):
        self.verify()
        for name in ('FFmpeg-CHANGES.diff', 'FFmpeg-SOURCE-RECIPE.json', 'FFmpeg-PROVENANCE.json',
                     self.fixtures.bundle.source.asset_name('frd-h264dc-aarch64-v1', 'linux-aarch64')):
            path = self.codec / name
            original = path.read_bytes()
            path.unlink()
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.verify()
            path.write_bytes(original + b'changed')
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.verify()
            path.write_bytes(original)
        (self.codec / 'arbitrary.zip').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, '文件集合'):
            self.verify()


if __name__ == "__main__":
    unittest.main()
