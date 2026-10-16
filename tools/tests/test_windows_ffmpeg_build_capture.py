"""真实宿主 spawn 与小型 PE 数据容器；不构建 shipping FFmpeg，不执行 PE。"""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def module(name, relative=None):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'tools' / (relative or name + '.py'))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def workdir(name):
    parent = os.environ.get('FRD_WINDOWS_RECIPE_TEST_ARTIFACTS')
    if parent:
        Path(parent).mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix=name + '-', dir=parent))


class NaturalSpawn(unittest.TestCase):
    def setUp(self):
        self.capture = module('windows-ffmpeg-build-capture')
        self.root = workdir('spawn')
        self.calls = self.root / 'calls'
        self.calls.mkdir()

    def tearDown(self):
        if not os.environ.get('FRD_WINDOWS_RECIPE_TEST_ARTIFACTS'):
            shutil.rmtree(self.root)

    def test_actual_argv_cwd_driver_name_and_partial_nonzero_unchanged(self):
        driver = self.root / 'preserved-driver-name'
        driver.symlink_to(Path(sys.executable).absolute())
        output = self.root / 'partial object.o'
        script = self.root / 'probe.py'
        script.write_text('import json,os,sys\nfrom pathlib import Path\n'
                          'Path(sys.argv[1]).write_bytes(b"partial")\n'
                          'sys.stdout.buffer.write(json.dumps({"cwd":os.getcwd(), "argv":sys.argv}).encode())\n'
                          'sys.stderr.buffer.write(b"original error\\x00bytes")\n'
                          'sys.exit(7)\n')
        argv = [str(driver), str(script), str(output), '-o', str(output), 'space argument', '-DKEEP=1', '@absent.rsp']
        code = self.capture.spawn(argv, self.root, self.calls, expected_tool=self.capture.artifact(driver))
        self.assertEqual(code, 7)
        path = next(self.calls.glob('*/finished.json'))
        data = json.loads(path.read_bytes())
        self.assertEqual(data['argv'], argv)
        self.assertEqual(data['cwd'], str(self.root))
        actual = json.loads((path.parent / 'stdout.bin').read_bytes())
        self.assertEqual(actual, {'cwd': str(self.root), 'argv': argv[1:]})
        self.assertEqual((path.parent / 'stderr.bin').read_bytes(), b'original error\0bytes')
        self.assertEqual((path.parent / 'module.after').read_bytes(), b'partial')
        with self.assertRaisesRegex(ValueError, '失败'):
            self.capture.load_call(path)

    def test_missing_tool_exec_error_and_tool_identity_drift(self):
        broken = self.root / 'broken-tool'
        broken.write_bytes(b'not executable format')
        broken.chmod(0o755)
        code = self.capture.spawn([str(broken)], self.root, self.calls, expected_tool=self.capture.artifact(broken))
        self.assertEqual(code, 127)
        path = next(self.calls.glob('*/finished.json'))
        self.assertIsNotNone(json.loads(path.read_bytes())['error'])
        old = self.capture.artifact(broken)
        broken.write_bytes(b'changed')
        count = len(list(self.calls.iterdir()))
        with self.assertRaisesRegex(ValueError, '身份漂移'):
            self.capture.spawn([str(broken)], self.root, self.calls, expected_tool=old)
        self.assertEqual(len(list(self.calls.iterdir())), count)

    def test_shared_map_is_rejected_before_tool_spawn(self):
        script = self.root / 'must-not-execute.py'
        marker = self.root / 'executed'
        script.write_text('from pathlib import Path\nPath(' + repr(str(marker)) + ').touch()\n')
        argv = [sys.executable, str(script), '-o', str(self.root / 'one.dll'),
                '-Wl,--Map=' + str(self.root / 'shared.map')]
        with self.assertRaisesRegex(ValueError, '拒绝共享'):
            self.capture.spawn(argv, self.root, self.calls, expected_tool=self.capture.artifact(sys.executable))
        self.assertFalse(marker.exists())
        self.assertEqual(list(self.calls.iterdir()), [])

    def test_endpoint_rejects_natural_flags_without_rewrite_or_recompile(self):
        bundle = module('windows-ffmpeg-bundle')
        obj = self.root / 'kernel.o'
        argv = ['/fixed/aarch64-w64-mingw32-clang', '-I.', '-O3', '-c', '-o', str(obj),
                'libavcodec/aarch64/h264idct_neon.S']
        call = {'argv': argv[:], 'cwd': str(self.root)}
        with self.assertRaisesRegex(ValueError, '自然 AS'):
            bundle.endpoint_request(self.root, self.root / 'archive', obj, call, self.root / 'evidence')
        self.assertEqual(call['argv'], argv)
        self.assertFalse(obj.exists())
        self.assertFalse((self.root / 'evidence').exists())

    def test_fixed_tool_cli_and_initialize_fail_before_capture_tree(self):
        tree = self.root / 'source'
        tree.mkdir()
        original = self.capture.TOOLS
        self.capture.TOOLS = {'aarch64-w64-mingw32-clang': self.root / 'absent-fixed-tool'}
        try:
            with self.assertRaises(ValueError):
                self.capture.initialize(self.root, tree)
        finally:
            self.capture.TOOLS = original
        self.assertFalse((self.root / 'tool-capture').exists())
        result = subprocess.run([sys.executable, str(ROOT / 'tools/windows-ffmpeg-build-capture.py'),
                                 'initialize', '--attempt', str(self.root), '--tree', str(tree),
                                 '--tool', '/usr/bin/true'], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / 'tool-capture').exists())


class ProducerConsumer(unittest.TestCase):
    def setUp(self):
        if not shutil.which('clang') or not shutil.which('lld-link'):
            self.skipTest('需要已安装 clang/lld；不下载')
        self.strip = next((p for p in (
            Path('/opt/homebrew/opt/llvm/bin/llvm-strip'),
            Path('/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native/llvm/bin/llvm-strip'))
            if p.is_file()), None)
        if not self.strip:
            self.skipTest('需要已安装 llvm-strip；不下载')
        self.root = workdir('producer-consumer')
        self.bundle = module('windows-ffmpeg-bundle')
        self.capture = self.bundle.capture
        self.kernel = self.bundle.kernel
        self.fixture = module('windows_fixture', 'tests/generate_windows_kernel_fixture.py')
        self.archive = Path(os.environ.get('FRD_FFMPEG_TEST_ARCHIVE', ROOT / 'target/ffmpeg-macos/ffmpeg-8.1.2.tar.xz'))
        if not self.archive.is_file():
            self.skipTest('需要已有固定 archive；不下载')
        self.calls = self.root / 'calls'
        self.calls.mkdir()
        # 生成器内部的初次编译也保留原 argv、tool bytes、退出和完整输出。
        real_run = subprocess.run
        def fixture_run(argv, **kwargs):
            result = real_run(argv, **kwargs)
            self.capture.write(self.root / 'fixture-generation.json', {
                'argv': argv, 'cwd': str(kwargs.get('cwd', Path.cwd())), 'returncode': result.returncode,
                'stdout': result.stdout, 'stderr': result.stderr, 'tool': self.capture.artifact(argv[0])})
            return result
        with mock.patch.object(self.fixture.subprocess, 'run', fixture_run):
            self.obj, _ = self.fixture.generate_source(self.root / 'source', self.archive)
        self.tree = self.obj.parent
        self.linked, self.map, self.final = (self.root / name for name in ('linked.dll', 'linked.dll.map', 'final.dll'))
        self.compiler = Path(shutil.which('clang')).absolute()
        self.linker = Path(shutil.which('lld-link')).absolute()
        compile_argv = [str(self.compiler), '--target=aarch64-w64-mingw32', '-I', str(self.tree),
                        '-c', '-x', 'assembler-with-cpp', str(self.tree / self.kernel.SOURCE), '-o', str(self.obj)]
        link_argv = [str(self.linker), '/dll', '/noentry', '/nodefaultlib', '/machine:arm64', '/opt:noref,noicf',
                     '/timestamp:0', '/noimplib', '/lldmap:' + str(self.map), '/out:' + str(self.linked), str(self.obj)]
        self.receipts = {}
        for kind, argv in (('compile', compile_argv), ('link', link_argv),
                           ('strip', [str(self.strip), '-o', str(self.final), str(self.linked)])):
            before = set(self.calls.glob('*/finished.json'))
            exitcode = self.capture.spawn(argv, self.tree, self.calls, expected_tool=self.capture.artifact(argv[0]))
            self.assertEqual(exitcode, 0)
            self.receipts[kind] = next(iter(set(self.calls.glob('*/finished.json')) - before))
        (self.root / 'fixture-boundary.txt').write_text(
            'Host synthetic configuration and fixed-source data container only; not shipping FFmpeg or native execution.\n'
            'Strip uses explicit -o to exercise the supported existing endpoint; actual install-copy path is a separate refusal.\n')

    def tearDown(self):
        if hasattr(self, 'root') and not os.environ.get('FRD_WINDOWS_RECIPE_TEST_ARTIFACTS'):
            shutil.rmtree(self.root)

    def feed(self, name):
        return self.bundle.feed(self.tree, self.archive, self.obj, self.linked, self.map, self.final,
                                self.receipts, self.root / name)

    def test_actual_spawn_to_existing_consumer_and_identity_drift(self):
        record, checked = self.feed('positive')
        self.assertEqual(checked['extent_kind'], 'fixed_recipe_with_source_endpoint')
        self.assertEqual(record['object_kernel']['object'], self.kernel.artifact(self.obj))
        self.assertEqual(checked['final'], self.kernel.artifact(self.final))
        # 逐一改变链中的原材料；保持其余对象相同，现 consumer/producer 必须拒绝。
        for label, path in (('object', self.obj), ('config', self.tree / 'config.h'), ('config-mak', self.tree / 'ffbuild/config.mak'), ('map', self.map),
                            ('linked', self.linked), ('final', self.final)):
            original = path.read_bytes()
            path.write_bytes(original + b'tamper')
            with self.subTest(label=label), self.assertRaises(ValueError):
                self.feed('changed-' + label)
            path.write_bytes(original)
        for kind in ('compile', 'link', 'strip'):
            receipt = self.receipts[kind]
            data = json.loads(receipt.read_bytes())
            snapshot = Path(data['saved'][next(iter(data['saved']))]['path'])
            original = snapshot.read_bytes()
            snapshot.write_bytes(original + b'tamper')
            with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, '身份漂移'):
                self.feed('changed-' + kind + '-snapshot')
            snapshot.write_bytes(original)
        self.assertEqual(self.feed('restored')[1]['kernel'], checked['kernel'])

    def test_later_real_link_keeps_selected_avcodec_map_for_existing_consumer(self):
        later = self.root / 'avformat.dll'
        later_map = Path(str(later) + '.map')
        argv = [str(self.linker), '/dll', '/noentry', '/nodefaultlib', '/machine:arm64', '/opt:noref,noicf',
                '/timestamp:0', '/noimplib', '/lldmap:' + str(later_map), '/out:' + str(later), str(self.obj)]
        before = set(self.calls.glob('*/finished.json'))
        self.assertEqual(self.capture.spawn(argv, self.tree, self.calls,
                         expected_tool=self.capture.artifact(self.linker)), 0)
        later_receipt = next(iter(set(self.calls.glob('*/finished.json')) - before))
        selected = self.bundle.link_map_from_receipt(self.linked, self.receipts['link'])
        self.assertEqual(selected, self.map)
        self.assertNotEqual(selected, later_map)
        with self.assertRaisesRegex(ValueError, 'module 不匹配'):
            self.bundle.link_map_from_receipt(self.linked, later_receipt)
        record, checked = self.bundle.feed(self.tree, self.archive, self.obj, self.linked, selected,
                                           self.final, self.receipts, self.root / 'selected-after-later-link')
        self.assertEqual(record['map'], self.kernel.artifact(self.map))
        self.assertEqual(checked['final'], self.kernel.artifact(self.final))

    def test_unknown_capture_members_and_inplace_strip_path_are_refused(self):
        path = self.receipts['compile']
        original = path.read_bytes()
        data = json.loads(original)
        data['unknown'] = True
        path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, 'unknown'):
            self.feed('unknown')
        path.write_bytes(original)
        strip_receipt = self.receipts['strip']
        data = json.loads(strip_receipt.read_bytes())
        self.assertNotEqual(data['argv'][-1], str(self.final))
        # 真正安装行为：复制后对 installed path 原位 strip，不重写为 linked path。
        installed = self.root / 'installed.dll'
        shutil.copyfile(self.linked, installed)
        argv = [str(self.strip), str(installed)]
        before = set(self.calls.glob('*/finished.json'))
        self.assertEqual(self.capture.spawn(argv, self.tree, self.calls,
                         expected_tool=self.capture.artifact(self.strip)), 0)
        self.receipts['strip'] = next(iter(set(self.calls.glob('*/finished.json')) - before))
        with self.assertRaisesRegex(ValueError, 'installed-copy'):
            self.bundle.feed(self.tree, self.archive, self.obj, self.linked, self.map, installed,
                             self.receipts, self.root / 'installed-copy-rejected')
        preserved = json.loads(self.receipts['strip'].read_bytes())
        self.assertEqual(preserved['argv'], argv)
        self.assertEqual(preserved['inputs'][0]['path'], str(installed))



def builder_map_token():
    import re
    builder = (ROOT / 'tools/build-ffmpeg-windows.ps1').read_text()
    match = re.search(r"'(--extra-ldsoflags=-Wl,--Map=[^']+)'", builder)
    if match is None:
        raise AssertionError('builder 必须给出固定单引号 map token')
    return match[1]


def fixed_configure_control(directory, option):
    """执行固定归档的真实函数/完整选项循环/完整配置 heredoc；不运行工具链探测。"""
    import re
    import shlex
    import tarfile
    capture = module('windows-ffmpeg-build-capture')
    recipe = module('ffmpeg-source-recipe')
    archive = Path(os.environ.get('FRD_FFMPEG_TEST_ARCHIVE', ROOT / 'target/ffmpeg-macos/ffmpeg-8.1.2.tar.xz'))
    if not archive.is_file():
        raise unittest.SkipTest('需要已有固定 archive；不下载')
    recipe.require(recipe.digest(archive.read_bytes()) == recipe.ARCHIVE_SHA, 'fixed configure archive pin 不匹配')
    with tarfile.open(archive) as package:
        original = package.extractfile('ffmpeg-8.1.2/configure').read().decode()
    (directory / 'fixed-configure-original').write_text(original)
    fragments = []
    for name in ('append', 'add_ldflags', 'add_ldexeflags', 'add_ldsoflags'):
        matches = list(re.finditer(r'(?ms)^' + name + r'\(\)\{\n.*?^\}\n', original))
        if len(matches) != 1:
            raise AssertionError('固定 configure function 边界不唯一：' + name)
        fragments.append((name, matches[0].group()))
    filter_assignment = re.search(r'(?m)^ldflags_filter=echo\n', original).group()
    fragments.append(('initial-filter', filter_assignment))
    begin = original.index('for opt do\n')
    end = original.index('\ndone\n', begin) + len('\ndone\n')
    fragments.append(('complete-option-loop', original[begin:end]))
    begin = original.index('cat > ffbuild/config.mak <<EOF\n')
    end = original.index('\nEOF\n', begin) + len('\nEOF\n')
    fragments.append(('complete-config-write', original[begin:end]))
    closing = re.search(r'(?m)^echo "endif # FFMPEG_CONFIG_MAK" >> ffbuild/config.mak\n', original).group()
    fragments.append(('config-closing-write', closing))
    spans = []
    for name, fragment in fragments:
        offset = original.index(fragment)
        spans.append({'name': name, 'line': original.count('\n', 0, offset) + 1,
                      'size': len(fragment.encode()), 'sha256': recipe.digest(fragment.encode())})
    script = directory / 'configure-control.sh'
    # context 只供完整原 heredoc 写出的 SRC_PATH 使用；不设置、过滤或重写 LDSOFLAGS。
    context = ('set -x\nprintf "CONTROL_PID=%s\n" "$$"\nsource_path=' +
               shlex.quote(str(directory / 'rules')) + '\n')
    script.write_text(original.splitlines()[0] + '\n' + context + ''.join(f for _, f in fragments))
    script.chmod(0o755)
    (directory / 'ffbuild').mkdir()
    # 此固定 token 无单引号；与实际 Quote-Bash 的外层单引号完全一致。
    if "'" in option:
        raise AssertionError('固定 map token 不应含单引号')
    command = './configure-control.sh ' + "'" + option + "'"
    result = subprocess.run(['bash', '-c', command], cwd=directory, capture_output=True)
    capture.write(directory / 'configure-control.json', {'argv': ['bash', '-c', command],
        'cwd': str(directory), 'returncode': result.returncode,
        'stdout': result.stdout.decode(), 'stderr': result.stderr.decode(), 'builder_token': option,
        'tool': capture.artifact(shutil.which('bash')), 'script_shell': capture.artifact('/bin/sh'),
        'control': capture.artifact(script), 'archive': capture.artifact(archive), 'fragments': spans})
    if result.returncode:
        raise AssertionError(result.stderr.decode())
    config = (directory / 'ffbuild/config.mak').read_text()
    value = re.search(r'(?m)^LDSOFLAGS=(.*)$', config).group(1).strip()
    pid = re.search(r'CONTROL_PID=([0-9]+)', result.stdout.decode()).group(1)
    return value, pid


class FixedConfigureToken(unittest.TestCase):
    def test_old_token_pid_red_and_current_token_survives_same_real_configure_path(self):
        root = workdir('configure-token')
        try:
            old = root / 'old-token'
            current = root / 'current-token'
            old.mkdir()
            current.mkdir()
            # 原 Fix1 token 作为永久反控，保持和生产 token 相同的真实 configure 路径。
            value, pid = fixed_configure_control(old, '--extra-ldsoflags=-Wl,--Map=$$@.map')
            self.assertEqual(value, '-Wl,--Map=' + pid + '@.map')
            value, _ = fixed_configure_control(current, builder_map_token())
            self.assertEqual(value, '-Wl,--Map=$$@.map')
        finally:
            if not os.environ.get('FRD_WINDOWS_RECIPE_TEST_ARTIFACTS'):
                shutil.rmtree(root)


class LinkMapOwnership(unittest.TestCase):
    """归档中真实 library/common make 规则：两个 link 的顺序与确定性交叠窗口。"""
    def setUp(self):
        self.root = workdir('map-ownership')
        self.capture = module('windows-ffmpeg-build-capture')
        self.bundle = module('windows-ffmpeg-bundle')

    def tearDown(self):
        if not os.environ.get('FRD_WINDOWS_RECIPE_TEST_ARTIFACTS'):
            shutil.rmtree(self.root)

    def exercise(self, mode):
        import re
        import shlex
        import tarfile
        directory = self.root / mode
        directory.mkdir()
        calls = directory / 'calls'
        calls.mkdir()
        archive = Path(os.environ.get('FRD_FFMPEG_TEST_ARCHIVE', ROOT / 'target/ffmpeg-macos/ffmpeg-8.1.2.tar.xz'))
        if not archive.is_file() or not shutil.which('make'):
            self.skipTest('需要已有固定 archive 与 make；不下载')
        self.assertEqual(self.bundle.source.recipes.digest(archive.read_bytes()), self.bundle.source.recipes.ARCHIVE_SHA)
        rules = directory / 'rules/ffbuild'
        rules.mkdir(parents=True)
        with tarfile.open(archive) as package:
            for name in ('library.mak', 'common.mak', 'arch.mak'):
                (rules / name).write_bytes(package.extractfile('ffmpeg-8.1.2/ffbuild/' + name).read())
        # 与纯 token 控制相同的原 configure append/eval/完整写出路径，再消费原 make。
        value, _ = fixed_configure_control(directory, builder_map_token())
        self.assertEqual(value, '-Wl,--Map=$$@.map')
        probe = directory / 'link-probe.py'
        probe.write_text('#!' + sys.executable + '\n' + '''import json,sys,time
from pathlib import Path
args=sys.argv[1:]
output=Path(args[args.index('-o')+1])
map_path=Path(next(a[len('-Wl,--Map='):] for a in args if a.startswith('-Wl,--Map=')))
if ''' + repr(mode) + ''' == 'overlap' and 'avformat' in str(output):
    deadline=time.monotonic()+10
    while not Path('codec-snapshot-ready').exists():
        if time.monotonic()>deadline: raise RuntimeError('没有进入预定 codec snapshot 窗口')
        time.sleep(.01)
output.write_text('module='+str(output)+'\\n')
map_path.write_text('map-owner='+str(output)+'\\n')
if 'avformat' in str(output): Path('format-link-complete').write_text('done')
print(json.dumps({'argv':sys.argv, 'cwd':str(Path.cwd())}))
''')
        probe.chmod(0o755)
        driver = directory / 'capture-driver.py'
        driver.write_text('#!' + sys.executable + '\n' + '''import importlib.util,sys,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('map_capture', ''' + repr(str(ROOT / 'tools/windows-ffmpeg-build-capture.py')) + ''')
c=importlib.util.module_from_spec(spec);spec.loader.exec_module(c)
original_copy=c.shutil.copyfile
def copyfile(source,destination,*args,**kwargs):
    if ''' + repr(mode) + ''' == 'overlap' and Path(destination).name == 'map.after' and 'avcodec' in str(sys.argv):
        Path('codec-snapshot-ready').write_text('ready')
        deadline=time.monotonic()+10
        while not Path('format-link-complete').exists():
            if time.monotonic()>deadline: raise RuntimeError('没有交叠的第二次 link')
            time.sleep(.01)
    return original_copy(source,destination,*args,**kwargs)
c.shutil.copyfile=copyfile
argv=[''' + repr(str(probe)) + ''', *sys.argv[1:]]
sys.exit(c.spawn(argv,Path.cwd(),Path('calls'),expected_tool=c.artifact(argv[0])))
''')
        driver.chmod(0o755)
        makefile = ('MAIN_MAKEFILE=1\nV=1\nCONFIG_SHARED=yes\n'
                    'SRC_PATH:=' + str(directory / 'rules') + '\n'
                    'include ffbuild/config.mak\nLD:=' + str(driver) + '\nLD_O=-o $@\n'
                    'SHFLAGS=-shared\nSLIBNAME=$(NAME).dll\nSLIBNAME_WITH_MAJOR=$(NAME)-62.dll\n'
                    'include $(SRC_PATH)/ffbuild/common.mak\n')
        for name in ('avcodec', 'avformat'):
            lib = directory / ('lib' + name)
            lib.mkdir()
            for file in ('version.h', 'version_major.h', name + '.o', 'lib' + name + '.v',
                         'lib' + name + '.version', 'lib' + name + '.ver'):
                (lib / file).write_text('# fixture\n')
            makefile += ('NAME=' + name + '\nFULLNAME=' + name + '\nSUBDIR=lib' + name + '/\n'
                         'OBJS=' + name + '.o\ninclude $(SRC_PATH)/ffbuild/library.mak\n')
        (directory / 'Makefile').write_text(makefile)
        argv = ['make', '-f', 'Makefile', '-j4' if mode == 'overlap' else '-j1',
                'libavcodec/avcodec-62.dll', 'libavformat/avformat-62.dll']
        result = subprocess.run(argv, cwd=directory, capture_output=True)
        self.capture.write(directory / 'make-control.json', {'argv': argv, 'cwd': str(directory),
            'returncode': result.returncode, 'stdout': result.stdout.decode(), 'stderr': result.stderr.decode(),
            'tool': self.capture.artifact(shutil.which('make'))})
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        receipts = {}
        for path in calls.glob('*/finished.json'):
            data = json.loads(path.read_bytes())
            module_path = Path(data['outputs']['module']['path'])
            expected = ('map-owner=' + str(module_path.relative_to(directory)) + '\n').encode()
            self.assertEqual(Path(data['saved']['map']['path']).read_bytes(), expected,
                             'snapshot 混入另一 link 的 map')
            self.assertEqual(Path(data['outputs']['map']['path']).read_bytes(), expected,
                             '顺序后继 link 覆盖原 map')
            actual = json.loads((path.parent / 'stdout.bin').read_bytes())
            self.assertEqual(actual['argv'], data['argv'])
            self.assertEqual(actual['cwd'], data['cwd'])
            receipts[module_path.name] = path
        self.assertEqual(set(receipts), {'avcodec-62.dll', 'avformat-62.dll'})
        first, second = (self.capture.load_call(receipts[name]) for name in ('avcodec-62.dll', 'avformat-62.dll'))
        self.assertNotEqual(first['outputs']['map']['path'], second['outputs']['map']['path'])
        selected = self.bundle.link_map_from_receipt(directory / 'libavcodec/avcodec-62.dll', receipts['avcodec-62.dll'])
        self.assertEqual(selected, directory / 'libavcodec/avcodec-62.dll.map')
        self.assertEqual(selected.read_bytes(), b'map-owner=libavcodec/avcodec-62.dll\n')
        if mode == 'overlap':
            self.assertTrue((directory / 'codec-snapshot-ready').is_file())
            self.assertTrue((directory / 'format-link-complete').is_file())

    def test_sequential_links_keep_each_original_map_and_select_avcodec(self):
        self.exercise('sequential')

    def test_overlapping_completion_window_keeps_snapshot_owner(self):
        self.exercise('overlap')


if __name__ == '__main__':
    unittest.main()
