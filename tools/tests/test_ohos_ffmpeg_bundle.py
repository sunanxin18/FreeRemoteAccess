#!/usr/bin/env python3
"""对真实 clean OHOS bundle 做失败注入；不执行目标 ELF。"""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
MODULE = Path(__file__).resolve().parents[1] / "ohos-ffmpeg-bundle.py"
spec = importlib.util.spec_from_file_location("ohos_bundle", MODULE)
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)


class ActualBundleMutations(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.good = Path(os.environ["FRD_OHOS_FFMPEG_TEST_BUNDLE"])
        cls.sdk = Path(os.environ.get("OHOS_SDK_ROOT", bundle.DEFAULT_SDK)).resolve()
        bundle.verify_bundle(cls.good, cls.sdk)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "bundle"
        shutil.copytree(self.good, self.path)
        self.manifest = json.loads((self.path / "manifest.json").read_text())

    def tearDown(self):
        self.temp.cleanup()

    def write_manifest(self):
        bundle.save_json(self.path / "manifest.json", self.manifest)

    def change_library(self, name, mutate):
        path = self.path / name
        data = bytearray(path.read_bytes())
        mutate(data)
        path.write_bytes(data)
        # 自洽 manifest 不能放行错误 bytes；测试固定 ELF 合同而非仅 hash。
        self.manifest["files"][name] = bundle.sha(path)
        try:
            self.manifest["elf"][name] = bundle.elf(path)
        except ValueError:
            pass
        self.write_manifest()

    def rejected(self, message):
        with self.assertRaisesRegex(ValueError, message):
            bundle.verify_bundle(self.path, self.sdk)

    def test_unknown_source_recipe_even_with_self_consistent_manifest(self):
        self.manifest['source']['recipe'] = 'unapproved-recipe'
        self.write_manifest()
        self.rejected('来源/recipe')

    def test_modified_flag_cannot_relabel_source(self):
        self.manifest['source']['modified'] = not self.manifest['source']['modified']
        self.write_manifest()
        self.rejected('来源/recipe')

    def test_extra_patch_cannot_self_authorize(self):
        name = 'extra.patch'
        (self.path / name).write_bytes(b'unapproved patch')
        self.manifest['files'][name] = bundle.sha(self.path / name)
        self.write_manifest()
        self.rejected('文件集合')

    def test_modified_patch_with_rewritten_file_hash_rejected(self):
        if self.manifest['schema'] != 2:
            self.skipTest('仅修改配方携带 patch')
        path = self.path / 'FFmpeg-CHANGES.diff'
        path.write_bytes(path.read_bytes() + b'\nextra hunk')
        self.manifest['files'][path.name] = bundle.sha(path)
        self.write_manifest()
        self.rejected('受信源码材料')

    def test_old_library_cannot_claim_applied_patch(self):
        if self.manifest['schema'] != 2:
            self.skipTest('需要修改配方实际产物')
        old = bundle.REPO / 'target/ffmpeg-ohos/bundle-attempt-qjsezbh7/libavcodec.so.62'
        target = self.path / old.name
        shutil.copyfile(old, target)
        self.manifest['files'][target.name] = bundle.sha(target)
        self.manifest['elf'][target.name] = bundle.elf(target)
        self.manifest['libraries'][target.name] = bundle.sha(target)
        self.write_manifest()
        self.rejected('kernel')

    def test_valid_actual_bundle(self):
        self.assertEqual(set(bundle.verify_bundle(self.path, self.sdk)), set(bundle.LIBS))

    def test_wrong_architecture_even_with_updated_hash(self):
        self.change_library(bundle.LIBS[0], lambda data: struct.pack_into("<H", data, 18, 62))
        self.rejected("AArch64")

    def test_gnu_dependency_even_with_self_consistent_manifest(self):
        def mutate(data):
            pos = data.index(b"libavutil.so.60\0")
            data[pos:pos + 15] = b"libc.so.6\0".ljust(15, b"\0")
        self.change_library(bundle.LIBS[1], mutate)
        self.rejected("闭包")

    def test_wrong_soname_even_with_self_consistent_manifest(self):
        def mutate(data):
            pos = data.index(b"libavutil.so.60\0")
            data[pos:pos + len(b"libavutil.so.60")] = b"libavutil.so.61"
        self.change_library(bundle.LIBS[0], mutate)
        self.rejected("SONAME")

    def test_missing_library(self):
        (self.path / bundle.LIBS[0]).unlink()
        self.rejected("文件集合")

    def test_extra_library(self):
        shutil.copyfile(self.path / bundle.LIBS[0], self.path / "libavutil.so.60.26.102")
        self.rejected("文件集合")

    def test_missing_getter_even_with_self_consistent_manifest(self):
        def mutate(data):
            pos = data.index(b"frd_ffmpeg_get_api_v1\0")
            data[pos] = ord("x")
        self.change_library(bundle.LIBS[2], mutate)
        self.rejected("getter")

    def test_symlink(self):
        path = self.path / bundle.LIBS[0]
        path.unlink()
        path.symlink_to(self.good / bundle.LIBS[0])
        self.rejected("symlink")

    def test_manifest_hash_mismatch(self):
        self.manifest["files"][bundle.LIBS[0]] = "0" * 64
        self.write_manifest()
        self.rejected("hash")

    def test_manifest_config_mismatch(self):
        self.manifest["configure"].append("--enable-gpl")
        self.write_manifest()
        self.rejected("configure")

    def test_config_bytes_mismatch_even_with_updated_hash(self):
        path = self.path / "FFmpeg-config.h"
        path.write_text(path.read_text().replace("#define HAVE_NEON 1", "#define HAVE_NEON 0"))
        self.manifest["files"][path.name] = bundle.sha(path)
        self.write_manifest()
        self.rejected("HAVE_NEON")

    def test_unsafe_rpath(self):
        def mutate(data):
            header = struct.unpack_from("<HHIQQQIHHHHHH", data, 16)
            for index in range(header[11]):
                section = struct.unpack_from("<IIQQQQIIQQ", data, header[5] + index * 64)
                if section[1] == 6:
                    for offset in range(section[4], section[4] + section[5], 16):
                        if struct.unpack_from("<q", data, offset)[0] == 1:
                            struct.pack_into("<q", data, offset, 29)
                            return
            self.fail("fixture 缺少 dynamic NEEDED")
        self.change_library(bundle.LIBS[2], mutate)
        self.rejected("RPATH")

    def test_dynamic_tags_after_null_cannot_hide_runtime_contract(self):
        def mutate(data):
            header = struct.unpack_from("<HHIQQQIHHHHHH", data, 16)
            for index in range(header[9]):
                segment = struct.unpack_from("<IIQQQQQQ", data, header[4] + index * 56)
                if segment[0] == 2:
                    struct.pack_into("<qQ", data, segment[2], 0, 0)
                    return
            self.fail("fixture 缺少 PT_DYNAMIC")
        self.change_library(bundle.LIBS[2], mutate)
        self.rejected("DT_NULL")

    def test_section_table_cannot_hide_runtime_dynamic_strings(self):
        def mutate(data):
            header = struct.unpack_from("<HHIQQQIHHHHHH", data, 16)
            for index in range(header[11]):
                offset = header[5] + index * 64
                section = struct.unpack_from("<IIQQQQIIQQ", data, offset)
                if section[1] == 6:
                    struct.pack_into("<I", data, offset + 40, 0)
                    return
            self.fail("fixture 缺少 SHT_DYNAMIC")
        self.change_library(bundle.LIBS[2], mutate)
        self.rejected("dynstr 不一致")

    def test_wrong_target_provenance(self):
        self.manifest["target"] = "aarch64-unknown-linux-gnu"
        self.write_manifest()
        self.rejected("目标")

    def test_private_system_dependency(self):
        def mutate(data):
            pos = data.index(b"libavutil.so.60\0")
            data[pos:pos + 15] = b"private.z.so\0".ljust(15, b"\0")
        self.change_library(bundle.LIBS[1], mutate)
        self.rejected("闭包")


class ActualPluginInputMutations(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.good = Path(os.environ["FRD_OHOS_FFMPEG_TEST_PREFIX"])
        cls.sdk = Path(os.environ.get("OHOS_SDK_ROOT", bundle.DEFAULT_SDK)).resolve()
        bundle.verify_inputs(cls.good, cls.sdk, bundle.REPO)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "prefix"
        shutil.copytree(self.good, self.path)
        self.manifest = json.loads((self.path / "ohos-ffmpeg-inputs.json").read_text())

    def tearDown(self):
        self.temp.cleanup()

    def rejected(self, message):
        bundle.save_json(self.path / "ohos-ffmpeg-inputs.json", self.manifest)
        with self.assertRaisesRegex(ValueError, message):
            bundle.verify_inputs(self.path, self.sdk, bundle.REPO)

    def test_header_mismatch(self):
        path = self.path / "include/libavcodec/avcodec.h"
        path.write_bytes(path.read_bytes() + b"\n/* stale input */\n")
        self.rejected("headers")

    def test_header_major_even_with_self_consistent_manifest(self):
        path = self.path / "include/libavcodec/version_major.h"
        path.write_text(path.read_text().replace("LIBAVCODEC_VERSION_MAJOR  62", "LIBAVCODEC_VERSION_MAJOR  61"))
        self.manifest["headers"] = bundle.tree_hashes(self.path / "include")
        self.rejected("header major")

    def test_sdk_provenance_mismatch(self):
        self.manifest["sdk"]["package_sha256"] = "0" * 64
        self.rejected("SDK")

    def test_stale_plugin_source(self):
        self.manifest["plugin_inputs"]["crates/frd-video-ffmpeg-plugin/src/decoder.rs"] = "0" * 64
        self.rejected("来源")

    def test_input_library_wrong_arch_even_with_updated_hash(self):
        path = self.path / "lib/libavcodec.so.62"
        data = bytearray(path.read_bytes())
        struct.pack_into("<H", data, 18, 62)
        path.write_bytes(data)
        self.manifest["libraries"][path.name] = bundle.sha(path)
        self.rejected("AArch64")


class ActualOhosExactLinkRegression(unittest.TestCase):
    def test_wrong_unversioned_aliases_do_not_redirect_actual_target_link(self):
        prefix = Path(os.environ["FRD_OHOS_FFMPEG_TEST_PREFIX"])
        good_bundle = Path(os.environ["FRD_OHOS_FFMPEG_TEST_BUNDLE"])
        sdk = Path(os.environ.get("OHOS_SDK_ROOT", bundle.DEFAULT_SDK)).resolve()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            candidate = root / "prefix"
            shutil.copytree(prefix, candidate, symlinks=True)
            codec_alias = candidate / "lib/libavcodec.so"
            util_alias = candidate / "lib/libavutil.so"
            codec_alias.unlink()
            codec_alias.symlink_to("libavutil.so.60")
            util_alias.unlink()
            # 版本库保持已验证字节；无版本 codec 错向，util 缺失。
            bundle.verify_inputs(candidate, sdk, bundle.REPO)
            tools = bundle.rust_identity()
            environment = os.environ.copy()
            environment.update({
                "FFMPEG_DIR": str(candidate), "OHOS_SDK_ROOT": str(sdk),
                "CARGO_TARGET_DIR": str(root / "cargo"),
                "RUSTC": tools["rustc"]["path"], "RUSTDOC": tools["rustdoc"]["path"],
                "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER": str(bundle.tool_paths(sdk)["cc"]),
            })
            command = [tools["cargo"]["path"], "build", "--locked", "--release", "--target", bundle.TARGET,
                       "-p", "frd-video-ffmpeg-plugin", "--features", "native-ffmpeg"]
            log = subprocess.run(command, cwd=bundle.REPO, env=environment, text=True,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            self.assertEqual(log.returncode, 0, log.stdout)
            plugin = root / "cargo" / bundle.TARGET / "release" / bundle.LIBS[2]
            self.assertEqual(bundle.elf(plugin)["needed"], sorted(bundle.DEPS[bundle.LIBS[2]]))
            self.assertEqual(bundle.sha(plugin), bundle.sha(good_bundle / bundle.LIBS[2]))
            build_outputs = list((root / "cargo" / bundle.TARGET / "release/build").glob("frd-video-ffmpeg-plugin-*/output"))
            self.assertEqual(len(build_outputs), 1)
            directives = build_outputs[0].read_text()
            self.assertIn("cargo:rustc-link-lib=dylib:+verbatim=libavcodec.so.62", directives)
            self.assertIn("cargo:rustc-link-lib=dylib:+verbatim=libavutil.so.60", directives)
            self.assertNotIn("cargo:rustc-link-lib=dylib=avcodec\n", directives)


if __name__ == "__main__":
    unittest.main()
