"""显式 RDP 验证参数经过真实构建脚本，不执行构建或打包。"""
import os
from pathlib import Path
import subprocess
import sys
import warnings
import zipfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

class RdpBuildArgs(unittest.TestCase):
    def test_rust_builder_forwards_exact_feature(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            sdk = root / 'sdk'
            (sdk / 'native/sysroot').mkdir(parents=True)
            for name in ('llvm/bin/aarch64-unknown-linux-ohos-clang',
                         'llvm/bin/aarch64-unknown-linux-ohos-clang++',
                         'llvm/bin/llvm-ar', 'build-tools/cmake/bin/cmake'):
                tool = sdk / 'native' / name
                tool.parent.mkdir(parents=True, exist_ok=True)
                tool.write_text('#!/bin/sh\nexit 0\n')
                tool.chmod(0o755)
            cargo = root / 'capture-cargo'
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            cargo.chmod(0o755)
            result = subprocess.run([str(ROOT / 'tools/build-harmony.sh'), '--sdk', str(sdk),
                '--build', '--release', '-p', 'frd-shell-ohos-ffi',
                '--normal-hap-rdp-validation'], env={**os.environ, 'CARGO': str(cargo)},
                text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ['build', '--locked', '--target',
                'aarch64-unknown-linux-ohos', '--release', '--features',
                'frd-shell-ohos-ffi/normal-hap-rdp-validation', '-p', 'frd-shell-ohos-ffi'])
            result = subprocess.run([str(ROOT / 'tools/build-harmony.sh'), '--sdk', str(sdk),
                '--normal-hap-rdp-validation'], env={**os.environ, 'CARGO': str(cargo)},
                text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ['check', '--locked', '--target',
                'aarch64-unknown-linux-ohos', '--features',
                'frd-shell-ohos-ffi/normal-hap-rdp-validation', '-p', 'frd-shell-ohos-ffi'])
            result = subprocess.run([str(ROOT / 'tools/build-harmony.sh'), '--sdk', str(sdk)],
                env={**os.environ, 'CARGO': str(cargo)}, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ['check', '--locked', '--target',
                'aarch64-unknown-linux-ohos', '-p', 'frd-core', '-p', 'frd-frame',
                '-p', 'frd-app', '-p', 'frd-shell-runtime'])


    def test_missing_value_options_are_rejected(self):
        for name, options in (('build-harmony.sh', ('--sdk', '-p')),
                              ('build-harmony-hap.sh', ('--ffmpeg-bundle', '--ffmpeg-manifest-sha256'))):
            for option in options:
                result = subprocess.run([str(ROOT / 'tools' / name), option],
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)

    def test_hap_builder_forwards_to_rust_builder_and_keeps_checker(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            (root / 'tools').mkdir()
            script = root / 'tools/build-harmony-hap.sh'
            script.write_bytes((ROOT / 'tools/build-harmony-hap.sh').read_bytes())
            script.chmod(0o755)
            log = root / 'argv.log'
            capture = '#!/bin/sh\nprintf "%s\\n" "$0" "$@" >> "$FRD_ARGS_TEST_LOG"\n'
            for path in (root / 'tools/build-harmony.sh', root / 'bin/python3',
                         root / 'studio/tools/ohpm/bin/ohpm',
                         root / 'studio/tools/hvigor/bin/hvigorw'):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(capture)
                path.chmod(0o755)
            library = root / 'target/aarch64-unknown-linux-ohos/debug/libfrd_shell_ohos_ffi.so'
            library.parent.mkdir(parents=True)
            library.write_bytes(b'actual-rdp-build')
            hap = root / 'apps/freeremotedesk-ohos/entry/build/default/outputs/default/entry-default-unsigned.hap'
            hap.parent.mkdir(parents=True)
            def write_hap(data, duplicate=False):
                with warnings.catch_warnings():
                    warnings.simplefilter('ignore', UserWarning)
                    with zipfile.ZipFile(hap, 'w') as package:
                        package.writestr('libs/arm64-v8a/libfrd_shell_ohos_ffi.so', data)
                        if duplicate:
                            package.writestr('libs/arm64-v8a/libfrd_shell_ohos_ffi.so', data)
            write_hap(b'STRIPPED:actual-rdp-build')
            strip = root / 'studio/sdk/default/openharmony/native/llvm/bin/llvm-strip'
            strip.parent.mkdir(parents=True)
            strip.write_text('#!' + sys.executable + '\n' +
                'from pathlib import Path\nimport os, sys\n' +
                'assert sys.argv[1] == "--keep-section=.permission"\n' +
                'assert len(sys.argv) == 4\n' +
                'assert sys.argv[3].startswith("-o") and len(sys.argv[3]) > 2\n' +
                'assert Path(sys.argv[2]).name == "libfrd_shell_ohos_ffi.so"\n' +
                'Path(sys.argv[3][2:]).write_bytes(b"STRIPPED:" + Path(sys.argv[2]).read_bytes())\n' +
                'with open(os.environ["FRD_ARGS_TEST_LOG"], "a") as log: log.write("\\n".join(sys.argv) + "\\n")\n')
            strip.chmod(0o755)
            # 仅替代耗时工具；产品脚本内的 ZIP/FFI 字节检查由真实 Python 执行。
            (root / 'bin/python3').write_text(capture + 'if [ "$1" = "-" ]; then exec "$FRD_ARGS_TEST_PYTHON" "$@"; fi\n')
            env = {**os.environ, 'DEVECO_HOME': str(root / 'studio'),
                   'PATH': str(root / 'bin') + ':' + os.environ['PATH'],
                   'FRD_ARGS_TEST_LOG': str(log), 'FRD_ARGS_TEST_PYTHON': sys.executable,
                   'CARGO_TARGET_DIR': str(root / 'target'),
                   'DEVECO_SDK_HOME': str(root / 'studio/sdk')}
            result = subprocess.run([str(script), '--normal-hap-rdp-validation',
                '--ffmpeg-bundle', str(root / 'accepted-bundle'),
                '--ffmpeg-manifest-sha256', 'a' * 64], env=env,
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = log.read_text().splitlines()
            index = calls.index(str(root / 'tools/build-harmony.sh'))
            self.assertEqual(calls[index+1:index+7], ['--sdk', str(root / 'studio/sdk/default/openharmony'),
                '--build', '-p', 'frd-shell-ohos-ffi', '--normal-hap-rdp-validation'])
            self.assertIn(str(root / 'tools/stage-harmony-ffmpeg.py'), calls)
            self.assertIn(str(root / 'tools/check-harmony-hap.py'), calls)
            self.assertIn('assembleHap', calls)
            self.assertEqual(library.read_bytes(), b'actual-rdp-build')
            argv = [str(script), '--normal-hap-rdp-validation', '--ffmpeg-bundle',
                    str(root / 'accepted-bundle'), '--ffmpeg-manifest-sha256', 'a' * 64]
            with self.subTest(release=True):
                release_library = root / 'target/aarch64-unknown-linux-ohos/release/libfrd_shell_ohos_ffi.so'
                release_library.parent.mkdir(parents=True)
                release_library.write_bytes(b'actual-rdp-build')
                result = subprocess.run(argv + ['--release'], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('buildMode=release', log.read_text().splitlines())
                self.assertEqual(release_library.read_bytes(), b'actual-rdp-build')
            with self.subTest(canonical_directory=True):
                result = subprocess.run(argv, env={**env, 'CARGO_TARGET_DIR': str(root / 'target/../target')},
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
            for payload, duplicate in ((b'STRIPPED:stale-default-or-fixture', False),
                                       (b'actual-rdp-build', False),
                                       (b'STRIPPED:actual-rdp-build', True)):
                with self.subTest(payload=payload, duplicate=duplicate):
                    write_hap(payload, duplicate)
                    result = subprocess.run(argv, env=env, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, '不得接受旧库或重复条目')
            with self.subTest(malformed=True):
                hap.write_bytes(b'not a zip')
                result = subprocess.run(argv, env=env, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
            with self.subTest(target_directory=True):
                log.unlink()
                result = subprocess.run(argv, env={**env, 'CARGO_TARGET_DIR': str(root / 'elsewhere')},
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 2)
                self.assertFalse(log.exists(), '必须在暂存之前拒绝目录不匹配')

    def test_help_documents_both_entrypoints_and_unknown_options_fail(self):
        for name in ('build-harmony.sh', 'build-harmony-hap.sh'):
            script = str(ROOT / 'tools' / name)
            result = subprocess.run([script, '--help'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0)
            self.assertIn('--normal-hap-rdp-validation', result.stdout)
            result = subprocess.run([script, '--unknown-mode'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)

if __name__ == '__main__':
    unittest.main()
