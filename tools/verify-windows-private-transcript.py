#!/usr/bin/env python3
"""冻结 Windows ARM64 私有 C 合同的纯 bytes 解析器；不执行任何程序。

parse_transcript(stdout, stderr, suite, variant, parent_returncode) 只返回观察。
传入的退出码/bytes 都是数据；本库不能证明进程、host、PE、artifact 身份或
native 执行，不能将合成测试提升为资格。C 内部 saved-slot/context oracle
接受为有限控制，日志未提供 full contexts、child 模块文件或 loader 身份；
matched 不表示外部重建这些值。no_return 时间是 ready→terminate，非 GO 时钟。
"""
from dataclasses import dataclass
import re

MAX_STDOUT = 1024 * 1024
MAX_CHILD_STREAM = 32767
MAX_LINE = 4096
_PC = (('entry', 0), ('after_alloc', 4), ('body', 0x30), ('call', 0x1a0),
       ('return', 0x1a4), ('epilogue', 0x410), ('after_d14', 0x414), ('ret', 0x43c))
_CASES = ('preserve', 'corrupt_fp', 'corrupt_sp16', 'corrupt_sp8', 'wrong_return', 'no_return')
_PAIRS = frozenset([('register', 0), ('isolated', 0)] + [('unwind', v) for v in range(5)])
_LIMITATIONS = (
    '输入字节及 parent returncode 是调用方数据，并非进程/host/artifact admission',
    'full actual/expected contexts 仅由 C 内部 oracle 检查，外部未观察',
    'child base 为 C 记录的 OS create-event 字段；外部模块文件/provider 身份未关闭',
    'pre-ready external-PE breakpoint 仅为 C 分类，未独立证明 loader identity',
    'no_return >=1500ms 是 ready→terminate，不是 GO 后完整1500ms',
    'shipping kernel/qmul/native/runtime/package/benchmark 资格均未关闭',
)


class TranscriptError(ValueError):
    """输入不符合冻结合同；错误文本确定且不回显原始输入。"""


def _require(condition, message):
    if not condition:
        raise TranscriptError(message)


@dataclass(frozen=True)
class Record:
    kind: str
    fields: tuple
    parameters: tuple = ()


@dataclass(frozen=True)
class ChildObservation:
    case: str
    pid: int
    stdout: bytes
    stderr: bytes
    admission: tuple
    bindings: tuple
    events: tuple
    result: tuple
    terminal: tuple
    time: tuple


@dataclass(frozen=True)
class TranscriptObservation:
    suite: str
    variant: int
    parent_returncode: int
    stdout_size: int
    admission: tuple
    bindings: tuple
    masks: tuple
    unwind: tuple
    rtl: tuple
    walk: str | None
    children: tuple
    limitations: tuple = _LIMITATIONS


class _Bytes:
    """仅按 cursor 消费；raw 区不经过全局 splitlines/换行归一化。"""
    def __init__(self, data):
        self.data = data
        self.pos = 0

    def line(self, exact_lf=False):
        end = self.data.find(b'\n', self.pos, self.pos + MAX_LINE + 1)
        _require(end >= 0, 'missing/bounded line terminator')
        raw = self.data[self.pos:end]
        self.pos = end + 1
        if raw.endswith(b'\r') and not exact_lf:
            raw = raw[:-1]
        _require(all(32 <= byte <= 126 for byte in raw), 'illegal line encoding/control byte')
        return raw.decode('ascii')

    def peek(self):
        pos = self.pos
        result = self.line()
        self.pos = pos
        return result

    def literal(self, text, exact_lf=False):
        _require(self.line(exact_lf) == text, 'unexpected literal/order')

    def fields(self, kind, schema):
        prefix = 'FRD1 ' + kind + ' '
        text = self.line()
        _require(text.startswith(prefix), 'unexpected record/order: ' + kind)
        tokens = text[len(prefix):].split(' ')
        _require(len(tokens) == len(schema), 'field count: ' + kind)
        values = {}
        for token, (key, form) in zip(tokens, schema):
            _require(token.startswith(key + '='), 'field name/order: ' + kind)
            value = token[len(key) + 1:]
            values[key] = _value(value, form)
        return values

    def raw(self, case, stream):
        text = self.line()
        prefix = f'FRD1 raw case={case} stream={stream} bytes='
        _require(text.startswith(prefix) and text.endswith(' begin'), 'raw begin case/stream/framing')
        n = _value(text[len(prefix):-6], 'u64')
        _require(n <= MAX_CHILD_STREAM, 'child stream size bound')
        _require(n <= len(self.data) - self.pos, 'truncated raw payload')
        payload = self.data[self.pos:self.pos + n]
        self.pos += n
        self.literal('')
        self.literal(f'FRD1 raw case={case} stream={stream} end')
        return payload

    def done(self):
        _require(self.pos == len(self.data), 'unclassified trailing bytes')


def _value(value, form):
    if form == 's32':
        _require(re.fullmatch(r'0|-?[1-9][0-9]{0,9}', value) is not None, 'noncanonical signed decimal')
        number = int(value)
        _require(-(1 << 31) <= number <= (1 << 31) - 1, 'signed decimal width/range')
        return number
    if form in ('u32', 'u64', 'pid'):
        _require(re.fullmatch(r'0|[1-9][0-9]{0,19}', value) is not None, 'noncanonical decimal')
        number = int(value)
        limit = (1 << (64 if form == 'u64' else 32)) - 1
        _require(number <= limit and (form != 'pid' or number > 0), 'decimal width/range')
        return number
    if form in ('h32', 'h64', 'hex'):
        pattern = {'h32': r'[0-9a-f]{8}', 'h64': r'[0-9a-f]{16}',
                   'hex': r'0|[1-9a-f][0-9a-f]{0,15}'}[form]
        _require(re.fullmatch(pattern, value) is not None, 'noncanonical hex width')
        return int(value, 16)
    _require(value == form, 'unexpected fixed field')
    return value


def _fields(reader, kind, **schema):
    return reader.fields(kind, list(schema.items()))


def _admission(reader, variant):
    d = _fields(reader, 'admission', native='aa64', process='0000', pe='aa64', pointer='64', sp_align='16', variant=str(variant))
    bindings = []
    helper = None
    for name, offset in _PC:
        b = _fields(reader, 'binding', id=name, rva='hex', offset='hex', table='absent' if variant == 1 else 'present')
        if helper is None:
            helper = b['rva']
        _require(0 < helper <= 0xffffffff - 0x440 and helper % 4 == 0, 'helper RVA range/alignment')
        _require(b['rva'] == helper + offset and b['offset'] == offset, 'binding PC offset/RVA')
        bindings.append(tuple(b.items()))
    b = _fields(reader, 'binding', outer_rva='hex', return_offset='188', epilogue_offset='400', length='430')
    outer = b['outer_rva']
    _require(0 < outer <= 0xffffffff - 0x430 and outer % 4 == 0, 'outer RVA range/alignment')
    _require(outer + 0x430 <= helper or helper + 0x440 <= outer, 'overlapping helper/outer')
    bindings.append(tuple(b.items()))
    return tuple(d.items()), tuple(bindings)


def _mask(reader, name, want):
    d = _fields(reader, 'mask', id=name, actual='h64', expected='h64')
    _require(d['actual'] == want and d['expected'] == want, 'independent register mask mismatch')
    return tuple(d.items())


def _register(reader):
    rows = [_mask(reader, 'preserve', 0), _mask(reader, 'simd_upper_only', 0)]
    # 独立于 transcript expected=，来自 ABI GPR/SIMD 位号。
    for kind, start, stop in (('x', 19, 29), ('d', 8, 16)):
        for n in range(start, stop):
            bit = n - 19 if kind == 'x' else n + 2
            for suffix, value in (('lo', 1 << bit), ('bit40', 1 << bit), ('fake_checker', 1 << (bit + 32))):
                rows.append(_mask(reader, f'{kind}{n}_{suffix}', value))
    for name, bit in (('x18_snapshot', 18), ('fp_snapshot', 19), ('sp_snapshot', 20)):
        rows.append(_mask(reader, name, 1 << bit))
    return rows


def _rtl(reader):
    return tuple(_fields(reader, 'rtl', establisher='hex', handler='absent').items())


def _unwind(reader, variant):
    rows, rtl = [], []
    sps, pcs = [], []
    for index, (name, _) in enumerate(_PC):
        if variant == 1:
            d = _fields(reader, 'unwind', id=name, outcome='expected-missing-nonleaf-table')
        else:
            rtl.append(_rtl(reader))
            d = _fields(reader, 'unwind', id=name, diff='h64', expected='h64', caller_sp='h64', caller_pc='h64')
            want = 0
            if variant == 2 and 1 <= index <= 6:
                want = 1 << 20
            elif variant == 3 and 2 <= index <= 6:
                want = 3
            elif variant == 4 and index == 5:
                want = 3 << 16
            _require(d['diff'] == want and d['expected'] == want, 'independent unwind mask mismatch')
            _require(d['caller_sp'] > 0 and d['caller_sp'] % 16 == 0 and d['caller_pc'] > 0 and d['caller_pc'] % 4 == 0, 'caller SP/PC alignment')
            sps.append(d['caller_sp'] - (16 if want == 1 << 20 else 0))
            pcs.append(d['caller_pc'])
        rows.append(tuple(d.items()))
    if sps:
        _require(len(set(sps)) == 1 and len(set(pcs)) == 1, 'synthetic caller consistency')
    if variant in (0, 4):
        rtl.extend(_rtl(reader) for _ in range(3))
        walk = 'callback-helper-outer-caller outcome=matched'
    else:
        walk = 'outcome=not-applicable-bad-body-metadata'
    reader.literal('FRD1 walk ' + walk)
    return tuple(rows), tuple(rtl), walk


def _continue(reader, case, pid, tid, event, status):
    d = _fields(reader, 'debug_continue', case=case, pid='pid', tid='pid', event='u32', status='h32', result='s32', ok='1', error_valid='0', error='00000000')
    _require(d['pid'] == pid and (tid is None or d['tid'] == tid) and d['event'] == event and d['status'] == status and d['result'] != 0, 'debug continue pairing/status/nonzero result')
    return Record('debug_continue', tuple(d.items()))


def _exception(reader, case, pid):
    d = _fields(reader, 'exception', case=case, pid='pid', tid='pid', first='u32', code='h32', flags='h32', pc='hex', ready='u32')
    _require(d['pid'] == pid and d['first'] in (0, 1) and d['ready'] in (0, 1) and d['pc'] > 0, 'exception ownership/boolean/address')
    text = reader.line()
    prefix = f'FRD1 exception_parameters case={case} count='
    _require(text.startswith(prefix), 'missing exception parameters')
    tokens = text[len(prefix):].split(' ')
    count = _value(tokens[0], 'u32')
    _require(count <= 15 and len(tokens) == count + 1, 'exception parameters count')
    parameters = []
    for i, token in enumerate(tokens[1:]):
        _require(token.startswith(f'p{i}='), 'exception parameters order')
        parameters.append((f'p{i}', _value(token[len(f'p{i}='):], 'hex')))
    return d, Record('exception', tuple(d.items()), tuple(parameters))


def _trap_sites(value):
    """调用方持有的 F2 数据；固定 offset/opcode 不接受外部覆盖。"""
    lengths = {'frd_h264_dc_check_abi': 0x440, 'frd_h264_dc_wrong_return': 0x10,
               'frd_h264_dc_outer_probe': 0x430}
    _require(type(value) is dict and set(value) == {'image_size', 'functions'}, 'expected trap sites schema')
    size, functions = value['image_size'], value['functions']
    _require(type(size) is int and 0 < size <= 0xffffffff, 'expected image size')
    _require(type(functions) is dict and set(functions) == set(lengths), 'expected trap functions')
    copied, ranges = {}, []
    for name, length in lengths.items():
        item = functions[name]
        _require(type(item) is dict and set(item) == {'rva', 'length'}, 'expected trap function schema')
        rva = item['rva']
        _require(type(rva) is int and type(item['length']) is int and item['length'] == length and
                 0 < rva and rva % 4 == 0 and rva + length <= size, 'expected trap function range/length')
        _require(all(rva + length <= begin or end <= rva for begin, end in ranges), 'expected trap owners overlap')
        ranges.append((rva, rva + length))
        copied[name] = {'rva': rva, 'length': length}
    return {'image_size': size, 'functions': copied}


def _child(reader, case, parent_bindings, sites):
    header = _fields(reader, 'child', case=case, pid='pid', watchdog_ms='10000', no_return_ms='1500' if case == 'no_return' else '0')
    pid = header['pid']
    bad = case not in ('preserve', 'no_return')
    timeout = case == 'no_return'
    expected_exit = 0x80000003 if bad else (0xe0f00101 if timeout else 0)
    image = _fields(reader, 'child_image', case=case, pid='pid', base='hex')
    _require(image['pid'] == pid and image['base'] > 0 and image['base'] % 4 == 0 and
             image['base'] + sites['image_size'] <= (1 << 64), 'child image ownership/base/range')
    name, offset = ('frd_h264_dc_wrong_return', 8) if case == 'wrong_return' else ('frd_h264_dc_check_abi', 0x408)
    expected_pc = image['base'] + sites['functions'][name]['rva'] + offset
    events, probes = [Record('child_image', tuple(image.items()))], []
    created, loader, exited = False, False, False
    exit_info = None
    main_tid = None
    while not exited:
        text = reader.peek()
        if text.startswith('FRD1 debug_continue '):
            # CREATE_PROCESS/LOAD_DLL 不输出自己的 event 行；仍须成功 continue。
            d = _fields(reader, 'debug_continue', case=case, pid='pid', tid='pid', event='u32', status='h32', result='s32', ok='1', error_valid='0', error='00000000')
            _require(d['pid'] == pid and d['status'] == 0x10002 and d['result'] != 0, 'implicit event ownership/status/nonzero result')
            if not created:
                _require(d['event'] == 3, 'first event must create process')
                created = True
                main_tid = d['tid']
            else:
                _require(d['event'] == 6, 'unpaired/unexpected debug continue')
            events.append(Record('debug_continue', tuple(d.items())))
        elif text.startswith('FRD1 debug_event '):
            _require(created, 'ordinary event before create')
            d = _fields(reader, 'debug_event', case=case, pid='pid', tid='pid', event='u32', classification='ordinary')
            _require(d['pid'] == pid and d['event'] in (2, 4, 7, 8), 'ordinary event ownership/type')
            events.append(Record('debug_event', tuple(d.items())))
            events.append(_continue(reader, case, pid, d['tid'], d['event'], 0x10002))
        elif text.startswith('FRD1 exception '):
            _require(created, 'exception before create')
            d, record = _exception(reader, case, pid)
            if d['ready'] == 0:
                _require(not loader and not probes and d['first'] == 1 and d['code'] == 0x80000003, 'C-classified pre-ready breakpoint')
                loader = True
                status = 0x10002
            else:
                _require(d['tid'] == main_tid, 'probe main-thread identity')
                _require(bad and d['code'] == 0x80000003 and d['first'] == (1 if not probes else 0) and len(probes) < 2, 'probe exception code/chance/order')
                _require(d['pc'] == expected_pc, 'probe independent case/site PC')
                if probes:
                    _require(d['pc'] == probes[0]['pc'] and d['tid'] == probes[0]['tid'], 'probe exception PC/TID consistency')
                probes.append(d)
                status = 0x80010001
            events.append(record)
            events.append(_continue(reader, case, pid, d['tid'], 1, status))
        elif text.startswith('FRD1 exit_event '):
            _require(created, 'exit before create')
            d = _fields(reader, 'exit_event', case=case, pid='pid', tid='pid', event='5', code='h32', tick='u64', terminal='0')
            _require(d['pid'] == pid and d['code'] == expected_exit, 'exit event ownership/code')
            events.append(Record('exit_event', tuple(d.items())))
            exit_continue = _continue(reader, case, pid, d['tid'], 5, 0x10002)
            events.append(exit_continue)
            exit_info, exited = d, True
        else:
            raise TranscriptError('missing/unknown child debug event')
    _require(len(probes) == (2 if bad else 0), 'missing probe exception pair')
    out = reader.raw(case, 'stdout')
    err = reader.raw(case, 'stderr')
    _require(not err, 'unclassified child stderr')
    child = _Bytes(out)
    admission, bindings = _admission(child, 0)
    _require(bindings == parent_bindings, 'child/parent linked RVA mismatch')
    child.literal(f'FRD1 ready case={case} variant=0', exact_lf=True)
    if case == 'preserve':
        d = _fields(child, 'returned', case=case, mask='h64')
        _require(d['mask'] == 0, 'preserve returned mask')
        child.literal('FRD1 result case=preserve outcome=normal-control', exact_lf=True)
    child.done()
    result = _fields(reader, 'child_result', case=case, ready='1', go='1', terminal='1', first=str(int(bad)), second=str(int(bad)), code='h32', pc='hex', brk='h32', exit='h32', timeout=str(int(timeout)), forced=str(int(timeout)), signal='not-applicable-win32')
    _require(result['exit'] == expected_exit, 'child result exit consistency')
    if bad:
        _require(result['code'] == 0x80000003 and result['pc'] == expected_pc == probes[0]['pc'] and result['brk'] == 0xd43e0000, 'C internal exact trap outcome/opcode consistency')
        _require(result['pc'] % 4 == 0, 'trap PC alignment')
    else:
        _require(result['code'] == result['pc'] == result['brk'] == 0, 'unexpected non-probe exception result')
    terminal = _fields(reader, 'child_terminal', case=case, exit_event_seen='1', exit_event_code='h32', exit_continue_attempted='1', exit_continue_result='s32', exit_continue_ok='1', exit_continue_error_valid='0', exit_continue_error='00000000', wait_attempted='1', wait_result_valid='1', wait_result='00000000', wait_error_valid='0', wait_error='00000000', terminal='1')
    _require(terminal['exit_event_code'] == expected_exit, 'terminal exit code consistency')
    _require(terminal['exit_continue_result'] == dict(exit_continue.fields)['result'], 'terminal raw BOOL repeat mismatch')
    timing = _fields(reader, 'child_time', case=case, start_tick='u64', ready_tick='u64', terminate_tick='u64', exit_event_tick='u64', terminal_tick='u64', designated_timeout=str(int(timeout)))
    _require(timing['start_tick'] <= timing['ready_tick'] <= timing['exit_event_tick'] <= timing['terminal_tick'] and timing['exit_event_tick'] == exit_info['tick'], 'child clock order/exit tick consistency')
    if timeout:
        _require(timing['ready_tick'] <= timing['terminate_tick'] <= timing['exit_event_tick'] and timing['terminate_tick'] - timing['ready_tick'] >= 1500, 'designated ready-to-terminate timeout')
        reader.literal('FRD1 isolated case=no_return outcome=expected-bounded-timeout termination=parent-forced')
    else:
        _require(timing['terminate_tick'] == 0, 'unexpected terminate tick')
    return ChildObservation(case, pid, out, err, admission, bindings, tuple(events), tuple(result.items()), tuple(terminal.items()), tuple(timing.items()))


def parse_transcript(stdout: bytes, stderr: bytes, suite: str, variant: int,
                     parent_returncode: int, *, expected_trap_sites=None) -> TranscriptObservation:
    """验证七个固定公开 pair 的日志；返回观察，不创建 native success receipt。"""
    _require(type(stdout) is bytes and type(stderr) is bytes, 'stdout/stderr must be bytes')
    _require(type(suite) is str and type(variant) is int and (suite, variant) in _PAIRS, 'unsupported public suite/variant')
    _require(type(parent_returncode) is int and parent_returncode == 0, 'parent returncode must be actual exit0 data')
    _require(len(stdout) <= MAX_STDOUT, 'stdout size bound')
    _require(not stderr, 'unclassified parent stderr')
    reader = _Bytes(stdout)
    admission, bindings = _admission(reader, variant)
    sites = None
    if suite == 'isolated':
        sites = _trap_sites(expected_trap_sites)
        _require(dict(bindings[0])['rva'] == sites['functions']['frd_h264_dc_check_abi']['rva'] and
                 dict(bindings[-1])['outer_rva'] == sites['functions']['frd_h264_dc_outer_probe']['rva'],
                 'transcript/admitted helper and outer RVA')
    masks = [_mask(reader, 'baseline_return', 0)]
    unwind, rtl, children, walk = (), (), (), None
    if suite == 'register':
        masks.extend(_register(reader))
    elif suite == 'unwind':
        unwind, rtl, walk = _unwind(reader, variant)
    else:
        children = tuple(_child(reader, case, bindings, sites) for case in _CASES)
    reader.literal(f'FRD1 final suite={suite} variant={variant} status=ABI-contract-only/nativehelper-control shipping_kernel=unimplemented qmul=unimplemented qualification=OPEN')
    reader.done()
    return TranscriptObservation(suite, variant, parent_returncode, len(stdout), admission, bindings,
                                 tuple(masks), unwind, rtl, walk, children)
