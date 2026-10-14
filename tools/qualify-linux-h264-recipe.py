#!/usr/bin/env python3
"""单次实际构建后复用 shipping 对象；本机契约测试不是 Linux 运行证明。"""
import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import re
import stat
import struct
import subprocess
import sys
import time

REPO = Path(__file__).resolve().parent.parent
RECIPE = 'frd-h264dc-aarch64-v1'
OBJECTS = ('libavcodec/h264dsp.o', 'libavcodec/h264idct.o',
           'libavcodec/aarch64/h264dsp_init_aarch64.o',
           'libavcodec/aarch64/h264dsp_neon.o',
           'libavcodec/aarch64/h264idct_neon.o', 'libavcodec/startcode.o')
# 固定 FFmpeg 8.1.2 安装名；不从调用者或 provenance 推导链接信任。
INSTALLED_LIBRARIES = {'libavutil.so.60': 'libavutil.so.60.26.102',
                       'libavcodec.so.62': 'libavcodec.so.62.28.102'}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def module(name):
    spec = importlib.util.spec_from_file_location(name, REPO / 'tools' / (name + '.py'))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def safe_path(path, root=None, kind='file', absent=False):
    """先检查 lexical ancestors；resolve 不能先消除 symlink 证据。"""
    path = Path(os.path.abspath(path))
    require(all(not p.is_symlink() for p in (path, *path.parents)), f'路径拒绝 symlink：{path}')
    if root is not None:
        root = Path(os.path.abspath(root))
        require(path != root and root in path.parents, f'路径超出隔离根：{path}')
    if absent:
        require(not path.exists(), f'只允许不存在的 fresh 路径：{path}')
    else:
        mode = path.stat().st_mode
        require(stat.S_ISREG(mode) if kind == 'file' else stat.S_ISDIR(mode), f'路径类型不匹配：{path}')
    return path


def digest(path):
    path = safe_path(path)
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()


def stat_identity(value):
    return {name: getattr(value, 'st_' + name) for name in
            ('dev', 'ino', 'mode', 'size', 'mtime_ns', 'ctime_ns', 'uid', 'gid', 'nlink')}


def directory_identity(value):
    # 祖先中的无关目录项创建不会改变目录本体；仍检测替换、类型和权限变化。
    return {name: getattr(value, 'st_' + name) for name in ('dev', 'ino', 'mode', 'uid', 'gid')}


def installed_library(alias, attempt, root):
    """唯一链接例外：本次安装目录的两个固定单层 SONAME 叶子别名。"""
    root = Path(os.path.abspath(root))
    attempt = safe_path(attempt, root, 'dir')
    require(attempt.parent == root and re.fullmatch(r'build\.[A-Za-z0-9]+', attempt.name),
            '安装库 attempt 不属于本次隔离根')
    directory = safe_path(attempt / 'dist/usr/lib', root, 'dir')
    alias = Path(os.path.abspath(alias))
    require(alias.parent == directory and alias.name in INSTALLED_LIBRARIES,
            '安装库别名不属于精确 prefix')
    directories = {str(p): directory_identity(p.stat()) for p in (directory, *directory.parents)}
    alias_stat = stat_identity(alias.lstat())
    require(stat.S_ISLNK(alias_stat['mode']), '安装 SONAME 必须为 symlink 叶子')
    link = os.readlink(alias)
    require(link == INSTALLED_LIBRARIES[alias.name], '安装 SONAME readlink 不匹配固定 basename')
    target = safe_path(directory / link, root)
    target_stat = stat_identity(target.stat())
    require(stat.S_ISREG(target_stat['mode']), '安装目标必须为普通文件')
    # 目录 fd 固定读取位置；O_NOFOLLOW 防止最后一刻换成链接，fstat 检测同字节替换。
    directory_fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        require(directory_identity(os.fstat(directory_fd)) == directories[str(directory)], '安装目录身份改变')
        require(stat_identity(os.stat(alias.name, dir_fd=directory_fd, follow_symlinks=False)) == alias_stat
                and os.readlink(alias.name, dir_fd=directory_fd) == link, '安装别名读取前身份改变')
        fd = os.open(link, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory_fd)
        with os.fdopen(fd, 'rb') as stream:
            require(stat_identity(os.fstat(stream.fileno())) == target_stat, '安装目标读取前身份改变')
            result = hashlib.sha256()
            for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                result.update(chunk)
            require(stat_identity(os.fstat(stream.fileno())) == target_stat, '安装目标读取期间身份改变')
        require(stat_identity(os.stat(alias.name, dir_fd=directory_fd, follow_symlinks=False)) == alias_stat
                and os.readlink(alias.name, dir_fd=directory_fd) == link, '安装别名读取期间身份改变')
        require(directory_identity(os.fstat(directory_fd)) == directories[str(directory)], '安装目录读取期间身份改变')
    finally:
        os.close(directory_fd)
    safe_path(directory, root, 'dir')
    safe_path(target, root)
    require({str(p): directory_identity(p.stat()) for p in (directory, *directory.parents)} == directories,
            '安装路径祖先身份改变')
    require(stat_identity(alias.lstat()) == alias_stat and os.readlink(alias) == link,
            '安装别名读取后身份改变')
    require(stat_identity(target.stat()) == target_stat, '安装目标读取后身份改变')
    return {'alias': {'path': str(alias), 'readlink': link, 'stat': alias_stat},
            'target': {'path': str(target), 'stat': target_stat, 'sha256': result.hexdigest()},
            'directories': directories}


def assert_installed_unchanged(before, attempt, root):
    after = {name: installed_library(Path(value['alias']['path']), attempt, root)
             for name, value in before.items()}
    require(after == before, '资格检查期间安装库 alias/target 身份发生改变')
    return after


def check_inputs(evidence, label, before, installed, attempt, root):
    item = {'label': label, 'status': 'failed'}
    evidence.record.setdefault('identity_checks', []).append(item)
    try:
        aliases = assert_installed_unchanged(installed, attempt, root)
        regular = assert_unchanged(before)
        item['status'] = 'passed'
        return regular, aliases
    except (ValueError, OSError) as error:
        item['error'] = str(error)
        raise
    finally:
        evidence.save()


def numeric_input_paths(objects, installed):
    return objects + [Path(installed['libavutil.so.60']['target']['path'])]


def validate_numeric_inputs(result, paths, before):
    require(all(result['inputs'].get(str(path)) == before[str(path)] for path in paths),
            '原数值 runner 实际输入不是本次 shipping 对象/库')


def _configured_fixture_compiler(path, expected_sha256):
    """只识别已绑定配置中的固定赋值，不解释 make 或配置中的命令。"""
    path = safe_path(path)
    data = path.read_bytes()
    require(hashlib.sha256(data).hexdigest() == expected_sha256, '实际配置 hash 不匹配')
    text = data.decode('utf-8')
    # configure 的固定原生输出不需要续行；拒绝把下一行赋值当作独立有效 CC。
    require('\r' not in text and '\\\n' not in text, '配置要求唯一精确 CC=gcc，无续行')
    active = [line for line in text.split('\n') if not line.lstrip().startswith('#')]
    require(not any(re.match(r'\s*(?:define|endef)(?:\s|$)', line) for line in active),
            '配置要求独立 CC=gcc，拒绝 make define 块')
    lines = [line for line in active if re.search(r'(?<![A-Za-z0-9_])CC(?![A-Za-z0-9_])', line)]
    require(lines == ['CC=gcc'], '配置要求唯一精确 CC=gcc，拒绝替代赋值/命令/flags')
    require(digest(path) == expected_sha256, '实际配置读取期间 hash 改变')
    return {'command': ['gcc'], 'configuration_path': str(path),
            'configuration_sha256': expected_sha256}


def _validate_numeric_compiler(result, src, original, numeric, paths, prefix, configured_version):
    """核对原 runner 的版本、两次 C 编译、ABI 编译、链接和实际执行记录。"""
    compiler = result.get('compiler')
    require(isinstance(compiler, str) and bool(compiler.strip()) and
            compiler.splitlines()[0] == configured_version and configured_version.startswith('gcc '),
            '原数值 runner compiler 版本与实际配置不匹配')
    common = ['gcc', '-O3', '-std=c11', '-D_GNU_SOURCE', '-I', str(src)]
    fixtures = REPO / 'third_party/ffmpeg/8.1.2/tests'
    exe = numeric / 'h264-luma-dc-parity'
    expected = [
        ['gcc', '--version'],
        common + ['-Dff_h264dsp_init_aarch64=ff_h264dsp_init_aarch64_baseline', '-c',
                  str(original / 'libavcodec/aarch64/h264dsp_init_aarch64.c'), '-o', str(numeric / 'baseline-init.o')],
        common + ['-Wall', '-Wextra', '-Werror', '-c', str(fixtures / 'h264-luma-dc-parity.c'),
                  '-o', str(numeric / 'parity.o')],
        ['gcc', '-I', str(src), '-c', str(fixtures / 'h264-luma-dc-abi.S'), '-o', str(numeric / 'abi.o')],
        ['gcc', str(numeric / 'parity.o'), str(numeric / 'abi.o'), str(numeric / 'baseline-init.o')]
        + [str(path) for path in paths] + ['-lm', '-pthread', '-Wl,-rpath,' + str(prefix / 'lib'), '-o', str(exe)],
        [str(exe)],
    ]
    commands = result.get('commands')
    require(isinstance(commands, list) and len(commands) == len(expected), '原数值 runner gcc 命令记录不完整')
    for item, argv in zip(commands, expected):
        require(isinstance(item, dict) and item.get('argv') == argv and item.get('cwd') == str(src)
                and type(item.get('returncode')) is int and item['returncode'] == 0
                and isinstance(item.get('output'), str), '原数值 runner gcc 编译/链接/执行记录不匹配')
    require(commands[0]['output'] == compiler, '原数值 runner compiler 输出与版本命令不匹配')
    return compiler


def ldd_binding(output, installed, attempt, root):
    lines = re.findall(r'^\s*libavutil\.so\.60(?:\s|$).*$', output, re.M)
    entries = re.findall(r'^\s*libavutil\.so\.60 => (\S+) \([^\n]*\)\s*$', output, re.M)
    value = installed['libavutil.so.60']
    require(len(lines) == 1 and entries == [value['alias']['path']] and 'not found' not in output,
            '运行 avutil 解析不匹配同 prefix SONAME')
    bound = installed_library(Path(entries[0]), attempt, root)
    require(bound == value, '运行 avutil alias/target 身份改变')
    return {'soname': 'libavutil.so.60', 'lexical_path': entries[0],
            'canonical_path': bound['target']['path'], 'sha256': bound['target']['sha256']}


def installed_failure_snapshot(before, attempt, root):
    """只记录安全重新验证结果；失败路径另存 lstat/readlink，不读取不可信目标。"""
    after = {}
    for name, value in before.items():
        try:
            current = installed_library(Path(value['alias']['path']), attempt, root)
            after[name] = {'identity': current, 'changed': current != value}
        except (ValueError, OSError) as error:
            item = {'error': str(error), 'changed': True}
            for kind in ('alias', 'target'):
                path = Path(value[kind]['path'])
                try:
                    info = path.lstat()
                    item[kind] = {'path': str(path), 'stat': stat_identity(info)}
                    if stat.S_ISLNK(info.st_mode):
                        item[kind]['readlink'] = os.readlink(path)
                except OSError as stat_error:
                    item[kind] = {'path': str(path), 'error': str(stat_error)}
            after[name] = item
    return after


def preflight(env):
    # 不使用 PATH 中可替代的 uname；测试不得将注入结果标成 native pass。
    require(platform.system() == 'Linux' and platform.machine().lower() in ('aarch64', 'arm64'),
            '运行资格门必须为真实原生 Linux ARM64')
    require(env.get('FRD_FFMPEG_SOURCE_RECIPE') == RECIPE, '资格门要求精确显式修改配方')
    require(env.get('FRD_FFMPEG_ARCH', platform.machine()).lower() in ('aarch64', 'arm64'),
            '修改配方资格门只允许 ARM64 目标')
    require(env.get('FRD_CARGO_TARGET', 'aarch64-unknown-linux-gnu') == 'aarch64-unknown-linux-gnu',
            '资格门拒绝 cross/未知 Cargo target')
    require(not env.get('FRD_FFMPEG_CROSS_PREFIX') and not env.get('FRD_FFMPEG_CC'),
            '资格门不接受交叉编译器或覆盖生产编译器')
    source = module('desktop-ffmpeg-source')
    source.metadata(RECIPE, 'linux-aarch64')
    return source


def retained_paths(text, root):
    result = []
    for label in ('configured attempt', 'shipping source'):
        found = re.findall(r'^Linux FFmpeg retained ' + label + r': (.+)$', text, re.M)
        require(len(found) == 1, f'缺少或重复 retained {label}')
        result.append(safe_path(found[0], root, 'dir'))
    attempt, source = result
    require(attempt.parent == root and re.fullmatch(r'build\.[A-Za-z0-9]+', attempt.name),
            'retained attempt 不是本次隔离根直接子目录')
    require(source == attempt / 'source/ffmpeg-8.1.2', 'retained source 路径不匹配')
    attempts = list(root.glob('build.*'))
    require(attempts == [attempt], 'retained attempt 缺失或存在多个构建')
    return attempt, source


def object_identity(path, kernel=None):
    """仅检查实际 ELF REL 容器/固定核字节，不实现计算。"""
    data = safe_path(path).read_bytes()
    require(len(data) >= 64 and data[:7] == b'\x7fELF\x02\x01\x01', '对象要求 LE ELF64')
    h = struct.unpack_from('<HHIQQQIHHHHHH', data, 16)
    require(h[:3] == (1, 183, 1), '对象要求 AArch64 ET_REL')
    if kernel is None:
        return
    require(h[7] == 64 and h[10] == 64 and h[11] > 0 and h[5] + h[11] * 64 <= len(data),
            '对象 section table 无效')
    sections = [struct.unpack_from('<IIQQQQIIQQ', data, h[5] + i * 64) for i in range(h[11])]
    matches = []
    for section in sections:
        if section[1] != 2:
            continue
        require(section[9] == 24 and section[5] % 24 == 0 and
                0 < section[6] < len(sections), '对象 symtab 无效')
        strings = sections[section[6]]
        require(strings[1] == 3 and strings[4] + strings[5] <= len(data) and
                section[4] + section[5] <= len(data), '对象 symbol 范围无效')
        names = data[strings[4]:strings[4] + strings[5]]
        for offset in range(section[4], section[4] + section[5], 24):
            name, info, _, index, value, size = struct.unpack_from('<IBBHQQ', data, offset)
            require(name < len(names) and names.find(b'\0', name) >= 0, '对象 symbol name 无效')
            if names[name:names.find(b'\0', name)] != kernel['symbol'].encode():
                continue
            require(info & 15 == 2 and 0 < index < len(sections), '核必须为已定义 FUNC')
            target = sections[index]
            require(target[1] == 1 and target[2] & 6 == 6 and size == kernel['size'] and
                    value <= target[5] and size <= target[5] - value and
                    target[4] + target[5] <= len(data), '对象核范围或 size 无效')
            code = data[target[4] + value:target[4] + value + size]
            require(hashlib.sha256(code).hexdigest() == kernel['sha256'], '实际对象核 bytes 不匹配')
            matches.append((value, size))
    require(len(matches) == 1, '对象核 symbol 缺失/重复')


def benchmark_result(output):
    pattern = (r'^benchmark (C|NEON): warmup=(\d+) measured_calls=(\d+) batches=(\d+) '
               r'ns/call median=([\d.]+) p95=([\d.]+) max=([\d.]+) checksum=([0-9a-f]{16})$')
    rows = re.findall(pattern, output, re.M)
    require(len(rows) == 2 and {r[0] for r in rows} == {'C', 'NEON'}, '基准输出缺失/重复')
    result = {}
    for row in rows:
        require(tuple(map(int, row[1:4])) == (65536, 8126464, 31), '基准固定工作负载不匹配')
        values = tuple(map(float, row[4:7]))
        require(all(math.isfinite(v) and v > 0 for v in values) and values[0] <= values[1] <= values[2],
                '基准时间无效')
        result[row[0]] = dict(warmup=int(row[1]), measured_calls=int(row[2]), batches=int(row[3]),
                              median_ns=values[0], p95_ns=values[1], max_ns=values[2], checksum=row[7])
    require(result['C']['checksum'] == result['NEON']['checksum'], '基准 C/NEON checksum 不匹配')
    return result


class Evidence:
    def __init__(self, path, env):
        self.path, self.env = path, env
        self.record = {'schema': 'freeremotedesk.linux.h264-runtime.v1', 'status': 'running',
                       'host': {'system': platform.system(), 'machine': platform.machine(),
                                'platform': platform.platform()}, 'commands': [], 'benchmarks': []}
        self.save()

    def save(self):
        (self.path / 'qualification.json').write_text(json.dumps(self.record, indent=2) + '\n')

    def run(self, label, argv, timeout=120, env=None):
        item = {'label': label, 'argv': [str(v) for v in argv], 'cwd': str(REPO),
                'log': label + '.log', 'started_ns': time.time_ns(), 'timeout_seconds': timeout}
        self.record['commands'].append(item)
        self.save()
        log = self.path / item['log']
        try:
            with log.open('wb') as stream:
                process = subprocess.run(item['argv'], cwd=REPO, env=env or self.env,
                                         stdout=stream, stderr=subprocess.STDOUT, timeout=timeout)
            item['returncode'] = process.returncode
            require(process.returncode == 0, f'{label} 失败（{process.returncode}）；日志：{log}')
        except subprocess.TimeoutExpired:
            item['timed_out'] = True
            raise
        except (OSError, ValueError) as error:
            item['error'] = str(error)
            raise
        finally:
            item['finished_ns'] = time.time_ns()
            if log.exists():
                item['log_sha256'] = digest(log)
            self.save()
        return log.read_text(errors='replace')

    def quiet(self, label):
        output = self.run(label, ['/bin/ps', '-eo', 'pid=,comm='])
        busy = [line.strip() for line in output.splitlines() if line.strip() and
                re.fullmatch(r'(clang(?:-\d+)?|clang\+\+|cc1|cc1plus|gcc|g\+\+|rustc|cargo|make|ninja)',
                             Path(line.split(maxsplit=1)[1]).name)]
        require(not busy, '基准窗口仍有编译进程：' + ', '.join(busy))


def snapshot(paths):
    return {str(p): digest(p) for p in paths}


def assert_unchanged(before):
    after = snapshot([Path(p) for p in before])
    require(after == before, '资格检查期间输入身份发生改变')
    return after


def qualify(root, archive, source_api):
    root = safe_path(root, absent=True)
    require(not any(c in str(root) for c in (':', ',', '\n', '\r')),
            '隔离根不能包含动态链接路径分隔符或换行')
    # 所有宿主/recipe/fresh 根检查完成，才允许创建输出。
    if archive is not None:
        archive = safe_path(archive)
        require(digest(archive) == source_api.recipes.ARCHIVE_SHA, '固定归档身份不匹配')
    root.mkdir(parents=True)
    evidence_path = root / 'runtime-evidence'
    evidence_path.mkdir()
    env = dict(os.environ, FRD_FFMPEG_BUILD_ROOT=str(root), CARGO_TARGET_DIR=str(root / 'cargo'),
               FRD_FFMPEG_RUN_TARGET_TESTS='1', FRD_FFMPEG_KEEP_BUILD='1',
               FRD_FFMPEG_ARCH='aarch64', FRD_CARGO_TARGET='aarch64-unknown-linux-gnu',
               FRD_FFMPEG_SOURCE_RECIPE=RECIPE, LD_LIBRARY_PATH='')
    for name in ('LD_PRELOAD', 'LD_AUDIT', 'LD_DEBUG', 'LIBRARY_PATH', 'CPATH', 'C_INCLUDE_PATH',
                 'CPLUS_INCLUDE_PATH', 'FFMPEG_DIR', 'CFLAGS', 'CPPFLAGS', 'LDFLAGS'):
        env.pop(name, None)
    archive = archive or root / 'ffmpeg-8.1.2.tar.xz'
    env['FRD_FFMPEG_ARCHIVE'] = str(archive)
    evidence = Evidence(evidence_path, env)
    evidence.record['build_environment'] = {k: env[k] for k in (
        'FRD_FFMPEG_BUILD_ROOT', 'CARGO_TARGET_DIR', 'FRD_FFMPEG_RUN_TARGET_TESTS',
        'FRD_FFMPEG_KEEP_BUILD', 'FRD_FFMPEG_ARCHIVE', 'FRD_FFMPEG_SOURCE_RECIPE',
        'FRD_FFMPEG_ARCH', 'FRD_CARGO_TARGET', 'LD_LIBRARY_PATH')}
    before = None
    installed = {}
    attempt = None
    try:
        # builder 内插件/native main444 夹具只执行一次；失败不重试/回退。
        output = evidence.run('builder', ['bash', '-x', REPO / 'tools/build-ffmpeg-linux.sh'], timeout=4200)
        attempt, src = retained_paths(output, root)
        bundle = safe_path(root / 'bundle/linux-aarch64', root, 'dir')
        prefix = safe_path(attempt / 'dist/usr', root, 'dir')
        for name in INSTALLED_LIBRARIES:
            installed[name] = installed_library(prefix / 'lib' / name, attempt, root)
        evidence.record['installed_libraries_before'] = installed
        adapter = module('linux-ffmpeg-bundle')
        adapter.verify_bundle(bundle, 'linux-aarch64', RECIPE)
        record = json.loads(safe_path(bundle / 'FFmpeg-PROVENANCE.json', root).read_text())
        require(record['configuration']['args'] == adapter.FIXED_ARGS, 'runtime 拒绝 cross configure')
        require(json.loads(safe_path(attempt / 'configuration.json', root).read_text()) == record['configuration'],
                '配置记录与 bundle 不匹配')
        require(json.loads(safe_path(attempt / 'kernel.json', root).read_text()) == record['kernel'],
                '核记录与 bundle 不匹配')
        require(adapter.kernel_identity(safe_path(src / 'libavcodec/libavcodec.so.62', root)) == record['kernel'],
                '实际 source kernel 与 shipping 不匹配')
        for name in ('config.h', 'ffbuild/config.mak'):
            require(digest(safe_path(src / name, root)) == record['inputs'][name], '实际配置 hash 不匹配')
        configuration_before = {str(src / name): record['inputs'][name]
                                for name in ('config.h', 'ffbuild/config.mak')}
        before = dict(configuration_before)
        evidence.record['inputs_before'] = before
        evidence.record['fixture_compiler'] = _configured_fixture_compiler(
            src / 'ffbuild/config.mak', record['inputs']['ffbuild/config.mak'])
        require(digest(src / 'libavcodec/libavcodec.so.62') == record['inputs']['unstripped_libavcodec'],
                '未 strip 库身份不匹配')
        for name in ('libavcodec.so.62', 'libavutil.so.60'):
            require(installed[name]['target']['sha256'] == record['outputs'][name],
                    '实际 prefix 与 bundle 库身份不匹配')
        objects = [safe_path(src / name, root) for name in OBJECTS]
        pin = source_api.recipes.trusted()
        for obj in objects:
            object_identity(obj, pin['kernel'] if obj.name == 'h264idct_neon.o' else None)
        for name, hashes in pin['files'].items():
            require(digest(safe_path(src / name, root)) == hashes['post'], '实际源码 postimage 不匹配')
        for name, expected in pin['unchanged_oracle'].items():
            require(digest(safe_path(src / name, root)) == expected, '实际 C oracle 不匹配')
        require(digest(safe_path(archive)) == source_api.recipes.ARCHIVE_SHA, '实际 archive 不匹配')
        # 独立固定 archive 的原 C/initializer，不把生产源码重新 hash 当 preimage。
        original_text = evidence.run('prepare-upstream', [sys.executable, '-B', REPO / 'tools/desktop-ffmpeg-source.py',
            'prepare', '--profile', 'linux-aarch64', '--source-recipe', 'upstream',
            '--archive', archive, '--destination', root / 'upstream'])
        original = safe_path(original_text.strip(), root, 'dir')
        require(original == root / 'upstream/ffmpeg-8.1.2', '独立原版源码路径不匹配')
        for name, hashes in pin['files'].items():
            require(digest(safe_path(original / name, root)) == hashes['pre'], '独立源码 preimage 不匹配')
        for name, expected in pin['unchanged_oracle'].items():
            require(digest(safe_path(original / name, root)) == expected, '独立 C oracle 不匹配')
        inputs = objects + [Path(v['target']['path']) for v in installed.values()]
        inputs += [archive, src / 'libavcodec/libavcodec.so.62',
                           src / 'config.h', src / 'ffbuild/config.mak',
                           attempt / 'configuration.json', attempt / 'kernel.json']
        inputs += [src / name for name in set(pin['files']) | set(pin['unchanged_oracle'])]
        inputs += [original / name for name in set(pin['files']) | set(pin['unchanged_oracle'])]
        inputs += list(bundle.iterdir())
        inputs += [REPO / name for name in adapter.HELPERS]
        inputs += [REPO / 'tools/verify-ffmpeg-h264-dc.py', Path(__file__).resolve(),
                   REPO / 'tools/qualify-linux-h264-recipe.sh',
                   REPO / 'third_party/ffmpeg/8.1.2/tests/h264-luma-dc-parity.c',
                   REPO / 'third_party/ffmpeg/8.1.2/tests/h264-luma-dc-abi.S',
                   REPO / 'third_party/ffmpeg/8.1.2/tests/h264-luma-dc-platform.h']
        before = snapshot(inputs)
        # 上游 prepare 后仍绑定最初验证的配置，不允许重新 snapshot 掩盖配置变化。
        before.update(configuration_before)
        evidence.record.update(inputs_before=before, attempt=str(attempt), source=str(src),
                               upstream_source=str(original), prefix=str(prefix), bundle=str(bundle),
                               source_recipe=record['source'], kernel=record['kernel'],
                               configuration=record['configuration'], source_asset=record['source_asset'])
        runtime_env = dict(env, LD_LIBRARY_PATH=str(prefix / 'lib'))
        numeric = evidence_path / 'numeric'
        argv = [sys.executable, '-B', REPO / 'tools/verify-ffmpeg-h264-dc.py',
                '--target', 'aarch64-linux-gnu', '--execute', '--source', src,
                '--upstream-source', original, '--output', numeric, '--cc', 'gcc']
        numeric_paths = numeric_input_paths(objects, installed)
        for obj in numeric_paths:
            argv += ['--object', obj]
        # runner 的 --object 为直接链接输入；加入此同 prefix DSO 使原 results.json 也记录其 hash。
        argv += ['--ldflag=-Wl,-rpath,' + str(prefix / 'lib')]
        evidence.record['runtime_environment'] = {'LD_LIBRARY_PATH': runtime_env['LD_LIBRARY_PATH'],
                                                 'compiler': 'gcc', 'default_cflags': ['-O3', '-std=c11', '-D_GNU_SOURCE']}
        check_inputs(evidence, 'before-numeric', before, installed, attempt, root)
        numeric_output = evidence.run('numeric', argv, timeout=180, env=runtime_env)
        result = json.loads(safe_path(numeric / 'results.json', root).read_text())
        require(result.get('status') == 'passed' and result.get('mode') == 'parity' and
                result.get('target') == 'aarch64-linux-gnu', '原数值 runner 没有原生成功证据')
        evidence.record['runtime_environment']['compiler_version'] = _validate_numeric_compiler(
            result, src, original, numeric, numeric_paths, prefix, record['configuration']['compiler'])
        validate_numeric_inputs(result, numeric_paths, before)
        require('PASS parity cases=5974518' in numeric_output and 'dispatch: 30 contexts' in numeric_output and
                'guard pages; AArch64 ABI sentinels' in numeric_output, '数值/分派/内存/ABI 输出不完整')
        exe = safe_path(numeric / 'h264-luma-dc-parity', root)
        # 动态加载解析必须定位到本次 prefix，不能使用另一缓存或系统 avutil。
        check_inputs(evidence, 'before-ldd', before, installed, attempt, root)
        dependencies = evidence.run('runtime-dependencies', ['/usr/bin/ldd', exe], env=runtime_env)
        evidence.record['runtime_avutil_binding'] = ldd_binding(dependencies, installed, attempt, root)
        check_inputs(evidence, 'after-ldd', before, installed, attempt, root)
        evidence.record['numeric_results_sha256'] = digest(numeric / 'results.json')
        private_inputs = [exe, numeric / 'baseline-init.o', numeric / 'parity.o', numeric / 'abi.o']
        before.update(snapshot(private_inputs))
        evidence.record['inputs_before'] = before
        benchmark_errors = []
        for window in range(1, 4):
            label = f'benchmark-{window}'
            item = {'window': window, 'status': 'failed', 'executable_sha256': digest(exe)}
            evidence.record['benchmarks'].append(item)
            try:
                evidence.quiet(f'quiet-{window}')
                check_inputs(evidence, f'before-benchmark-{window}', before, installed, attempt, root)
                output = evidence.run(label, [exe, '--benchmark'], timeout=60, env=runtime_env)
                item.update(status='passed', results=benchmark_result(output))
            except (ValueError, OSError, subprocess.TimeoutExpired) as error:
                item['error'] = str(error)
                benchmark_errors.append(str(error))
            finally:
                (evidence_path / (label + '.json')).write_text(json.dumps(item, indent=2) + '\n')
                evidence.save()
        require(not benchmark_errors, '基准窗口失败：' + '; '.join(benchmark_errors))
        checksums = {r['results'][k]['checksum'] for r in evidence.record['benchmarks'] for k in ('C', 'NEON')}
        require(len(checksums) == 1, '三个窗口 checksum 不一致')
        regular_after, installed_after = check_inputs(evidence, 'final', before, installed, attempt, root)
        evidence.record['inputs_after'] = regular_after
        evidence.record['installed_libraries_after'] = installed_after
        evidence.record['status'] = 'passed'
        evidence.record['scope'] = 'native Linux ARM64 single H264 luma DC kernel; bounded CI-host measurements'
        return 0
    except (ValueError, OSError, subprocess.TimeoutExpired) as error:
        evidence.record.update(status='failed', error=str(error))
        print(str(error), file=sys.stderr)
        return 1
    finally:
        if installed and 'installed_libraries_after' not in evidence.record:
            evidence.record['installed_libraries_before'] = installed
            evidence.record['installed_libraries_after_failure'] = installed_failure_snapshot(installed, attempt, root)
        if before is not None and 'inputs_after' not in evidence.record:
            # 输入消失/变为 symlink 也必须保留失败状态，不能在 finally 再抛错遮住日志。
            after_failure = {}
            for path in before:
                try:
                    after_failure[path] = {'sha256': digest(Path(path))}
                except (ValueError, OSError) as error:
                    after_failure[path] = {'error': str(error)}
            evidence.record['inputs_after_failure'] = after_failure
        evidence.save()
        print(f'Linux H264 runtime evidence: {evidence_path}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build-root', type=Path)
    parser.add_argument('--archive', type=Path)
    args = parser.parse_args()
    try:
        source = preflight(os.environ)
        root = args.build_root or Path(os.environ.get('FRD_FFMPEG_BUILD_ROOT', REPO / 'target/ffmpeg-linux'))
        root = REPO / root if not root.is_absolute() else root
        archive = args.archive or (Path(os.environ['FRD_FFMPEG_ARCHIVE']) if os.environ.get('FRD_FFMPEG_ARCHIVE') else None)
        archive = REPO / archive if archive is not None and not archive.is_absolute() else archive
        return qualify(root, archive, source)
    except (ValueError, OSError) as error:
        print(str(error), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
