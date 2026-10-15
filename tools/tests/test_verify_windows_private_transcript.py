"""显式合成 host-only 字节控制；从未执行 Windows，也不是 native receipt。"""
import importlib.util
import copy
from pathlib import Path
import sys
import unittest

PATH = Path(__file__).resolve().parents[1] / 'verify-windows-private-transcript.py'
spec = importlib.util.spec_from_file_location('windows_private_transcript', PATH)
parser = None
if PATH.exists():
    parser = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = parser
    spec.loader.exec_module(parser)

PCS = [('entry', 0), ('after_alloc', 4), ('body', 0x30), ('call', 0x1b0),
       ('return', 0x1b4), ('epilogue', 0x420), ('after_d14', 0x424), ('ret', 0x44c)]
GROUPS = [('x19', 0), ('x20', 1), ('x21', 2), ('x22', 3), ('x23', 4),
          ('x24', 5), ('x25', 6), ('x26', 7), ('x27', 8), ('x28', 9),
          ('d8', 10), ('d9', 11), ('d10', 12), ('d11', 13), ('d12', 14),
          ('d13', 15), ('d14', 16), ('d15', 17)]
TRAP_SITES = {'image_size': 0x5000, 'functions': {
    'frd_h264_dc_check_abi': {'rva': 0x1000, 'length': 0x450},
    'frd_h264_dc_wrong_return': {'rva': 0x3000, 'length': 0x10},
    'frd_h264_dc_outer_probe': {'rva': 0x2000, 'length': 0x430},
    'frd_h264_dc_abi_sp_failure_fragment': {'rva': 0x1450, 'length': 32}}}

CASES = ('preserve', 'corrupt_fp', 'corrupt_sp16', 'corrupt_sp8', 'wrong_return', 'no_return')


def line(text, nl=b'\n'):
    return text.encode('ascii') + nl


def admission(v, nl=b'\n'):
    out = line(f'FRD1 admission native=aa64 process=0000 pe=aa64 pointer=64 sp_align=16 variant={v}', nl)
    for name, off in PCS:
        out += line(f'FRD1 binding id={name} rva={0x1000+off:x} offset={off:x} table={"absent" if v == 1 else "present"}', nl)
    out += line('FRD1 binding outer_rva=2000 return_offset=188 epilogue_offset=400 length=430', nl)
    out += line(f'FRD1 fragment owner=sp rva=1450 length=20 trap_offset=18 table={"absent" if v == 1 else "present"} authority=bound-callback-preserves-sp', nl)
    return out + line('FRD1 fragment owner=wrong_return rva=3000 length=10 trap_offset=8 table=present authority=bound-callback-preserves-sp', nl)


def mask(name, value):
    return line(f'FRD1 mask id={name} actual={value:016x} expected={value:016x}')


def final(suite, v):
    return line(f'FRD1 final suite={suite} variant={v} status=ABI-contract-only/nativehelper-control shipping_kernel=unimplemented qmul=unimplemented qualification=OPEN')


def raw(case, stream, payload):
    return (line(f'FRD1 raw case={case} stream={stream} bytes={len(payload)} begin', b'\r\n') + payload
            + line('', b'\r\n') + line(f'FRD1 raw case={case} stream={stream} end', b'\r\n'))


def cont(case, pid, tid, event, status='00010002'):
    return line(f'FRD1 debug_continue case={case} pid={pid} tid={tid} event={event} status={status} result=1 ok=1 error_valid=0 error=00000000')


def exception(case, pid, first, pc='80011468', ready=1, status='80010001'):
    return (line(f'FRD1 exception case={case} pid={pid} tid=22 first={first} code=80000003 flags=00000000 pc={pc} ready={ready}')
            + line(f'FRD1 exception_parameters case={case} count=0') + cont(case, pid, 22, 1, status))


def child(case, index, ordinary=0, loader=False):
    pid = 100 + index
    bad = case not in ('preserve', 'no_return')
    timeout = case == 'no_return'
    code = '80000003' if bad else ('e0f00101' if timeout else '00000000')
    out = line(f'FRD1 child case={case} pid={pid} watchdog_ms=10000 no_return_ms={1500 if timeout else 0}')
    base = 0x80000000 + index * 0x10000
    pc = f'{base + (0x3008 if case == 'wrong_return' else 0x1468 if case == 'corrupt_fp' else 0x1418):x}' if bad else '0'
    out += line(f'FRD1 child_image case={case} pid={pid} base={base:x}')
    out += cont(case, pid, 22, 3) + cont(case, pid, 22, 6)
    if loader:
        out += exception(case, pid, 1, '7fff00001000', 0, '00010002')
    for n in range(ordinary):
        event = (2, 4, 7, 8)[n % 4]
        out += line(f'FRD1 debug_event case={case} pid={pid} tid={30+n} event={event} classification=ordinary')
        out += cont(case, pid, 30+n, event)
    if bad:
        out += exception(case, pid, 1, pc) + exception(case, pid, 0, pc)
    out += line(f'FRD1 exit_event case={case} pid={pid} tid=22 event=5 code={code} tick=3000 terminal=0')
    out += cont(case, pid, 22, 5)
    payload = admission(0, b'\r\n') + line(f'FRD1 ready case={case} variant=0')
    if case == 'preserve':
        payload += line('FRD1 returned case=preserve mask=0000000000000000', b'\r\n')
        payload += line('FRD1 result case=preserve outcome=normal-control')
    out += raw(case, 'stdout', payload) + raw(case, 'stderr', b'')
    brk = 'd43e0000' if bad else '00000000'
    out += line(f'FRD1 child_result case={case} ready=1 go=1 terminal=1 first={int(bad)} second={int(bad)} code={"80000003" if bad else "00000000"} pc={pc} brk={brk} exit={code} timeout={int(timeout)} forced={int(timeout)} signal=not-applicable-win32')
    out += line(f'FRD1 child_terminal case={case} exit_event_seen=1 exit_event_code={code} exit_continue_attempted=1 exit_continue_result=1 exit_continue_ok=1 exit_continue_error_valid=0 exit_continue_error=00000000 wait_attempted=1 wait_result_valid=1 wait_result=00000000 wait_error_valid=0 wait_error=00000000 terminal=1')
    out += line(f'FRD1 child_time case={case} start_tick=1000 ready_tick=1100 terminate_tick={2600 if timeout else 0} exit_event_tick=3000 terminal_tick=3001 designated_timeout={int(timeout)}')
    if timeout:
        out += line('FRD1 isolated case=no_return outcome=expected-bounded-timeout termination=parent-forced')
    return out


def transcript(suite, v=0, ordinary=0, loader=False):
    out = admission(v) + mask('baseline_return', 0)
    if suite == 'register':
        out += mask('preserve', 0) + mask('simd_upper_only', 0)
        for name, bit in GROUPS:
            out += mask(name+'_lo', 1 << bit) + mask(name+'_bit40', 1 << bit)
            out += mask(name+'_fake_checker', 1 << (bit+32))
        out += mask('x18_snapshot', 0x40000) + mask('fp_snapshot', 0x80000) + mask('sp_snapshot', 0x100000)
    elif suite == 'unwind':
        for k, (name, _) in enumerate(PCS):
            if v == 1:
                out += line(f'FRD1 unwind id={name} outcome=expected-missing-nonleaf-table')
            else:
                diff = (0x100000 if v == 2 and 1 <= k <= 6 else
                        3 if v == 3 and 2 <= k <= 6 else 0x30000 if v == 4 and k == 5 else 0)
                out += line('FRD1 rtl establisher=70001000 handler=absent')
                out += line(f'FRD1 unwind id={name} diff={diff:016x} expected={diff:016x} caller_sp={0x70002010 if diff == 0x100000 else 0x70002000:016x} caller_pc=0000000080002188')
        for owner,length in (('sp',32),('wrong_return',16)):
            for offset in range(0,length,4):
                if v==1 and owner=='sp':
                    out += line(f'FRD1 fragment_unwind owner={owner} offset={offset:x} outcome=expected-missing-nonleaf-table')
                else:
                    diff=(0x100000 if v==2 else 3 if v==3 else 0) if owner=='sp' else 0
                    out += line('FRD1 rtl establisher=70001000 handler=absent')
                    out += line(f'FRD1 fragment_unwind owner={owner} offset={offset:x} diff={diff:016x} expected={diff:016x} caller_sp={0x70003010 if diff==0x100000 else 0x70003000:016x} caller_pc=0000000080002188')
        if v in (0, 4):
            out += line('FRD1 rtl establisher=70001000 handler=absent')
            for owner,length in (('sp',32),('wrong_return',16)):
                for offset in range(0,length,4):
                    out += line('FRD1 rtl establisher=70001000 handler=absent') * 2
                    out += line(f'FRD1 fragment_walk owner={owner} offset={offset:x} outcome=matched')
            out += line('FRD1 rtl establisher=70001000 handler=absent') * 2
            out += line('FRD1 walk callback-helper-outer-caller outcome=matched')
        else:
            out += line('FRD1 walk outcome=not-applicable-bad-body-metadata')
    else:
        for i, case in enumerate(CASES):
            out += child(case, i, ordinary, loader)
    return out + final(suite, v)


class TranscriptTests(unittest.TestCase):
    def test_all_fragment_records_are_mandatory_exact_and_ordered(self):
        original=transcript('unwind')
        records=original.splitlines(keepends=True)
        for index,record in enumerate(records):
            if record.startswith((b'FRD1 fragment_unwind ',b'FRD1 fragment_walk ')):
                with self.subTest(record=record),self.assertRaises(parser.TranscriptError):
                    self.parse(b''.join(records[:index]+records[index+1:]),'unwind')
                for changed in (record.replace(b'owner=sp',b'owner=unknown'),record.replace(b'offset=0 ',b'offset=4 '),record.rstrip(b'\n')+b' extra=1\n'):
                    if changed!=record:
                        with self.subTest(changed=changed),self.assertRaises(parser.TranscriptError):
                            self.parse(b''.join(records[:index]+[changed]+records[index+1:]),'unwind')
        got=self.parse(original,'unwind')
        self.assertEqual(len(got.unwind),20)
        self.assertEqual(len(got.rtl),47)

    def test_fragment_variant_differences_and_authority_cannot_be_self_reported(self):
        for variant,want in ((2,b'0000000000100000'),(3,b'0000000000000003')):
            data=transcript('unwind',variant)
            prefix=b'FRD1 fragment_unwind owner=sp offset=0 diff='+want+b' expected='+want
            self.assertIn(prefix,data)
            with self.assertRaises(parser.TranscriptError):
                self.parse(data.replace(prefix,prefix.replace(want,b'0000000000000000')),'unwind',variant)
        for changed in (b'authority=aligned-sp',b'authority=case-name',b'authority=unknown'):
            with self.assertRaises(parser.TranscriptError):
                self.parse(transcript('register').replace(b'authority=bound-callback-preserves-sp',changed,1),'register')

    def test_corrupt_fp_cannot_use_original_main_trap_or_wrong_return_owner(self):
        data=transcript('isolated')
        for other in (b'80011418',b'80013008',b'8001146c'):
            with self.subTest(other=other),self.assertRaises(parser.TranscriptError):
                self.parse(data.replace(b'pc=80011468',b'pc='+other))

    def test_isolated_without_independent_site_identity_is_rejected(self):
        with self.assertRaises(parser.TranscriptError):
            parser.parse_transcript(transcript('isolated'), b'', 'isolated', 0, 0)

    def test_independent_sites_and_different_child_bases(self):
        got = self.parse(transcript('isolated'))
        self.assertEqual([dict(c.events[0].fields)['base'] for c in got.children],
                         [0x80000000, 0x80010000, 0x80020000, 0x80030000, 0x80040000, 0x80050000])
        self.assertEqual(dict(got.children[1].result)['pc'], 0x80011468)
        self.assertEqual(dict(got.children[4].result)['pc'], 0x80043008)
        for case, old, alternatives in [
                ('corrupt_fp', b'80011468', (b'80013008', b'800123f8', b'8001146c')),
                ('wrong_return', b'80043008', (b'80041418', b'800423f8', b'8004300c'))]:
            for other in alternatives:
                data = transcript('isolated')
                self.assertEqual(data.count(b'pc='+old), 3)
                with self.subTest(case=case, other=other), self.assertRaises(parser.TranscriptError):
                    self.parse(data.replace(b'pc='+old, b'pc='+other))

    def test_child_image_missing_duplicate_foreign_and_order_are_rejected(self):
        data = transcript('isolated')
        record = line('FRD1 child_image case=corrupt_fp pid=101 base=80010000')
        for replacement in (b'', record*2, record.replace(b'pid=101', b'pid=102'),
                            record.replace(b'corrupt_fp', b'wrong_return'),
                            record.replace(b'base=80010000', b'base=80020000'),
                            record.replace(b'base=80010000', b'base=0'),
                            record.replace(b'base=80010000', b'base=fffffffffffffff0'),
                            record.replace(b'base=80010000', b'base=080010000'),
                            record.replace(b'base=80010000', b'base=8001000A')):
            with self.subTest(replacement=replacement), self.assertRaises(parser.TranscriptError):
                self.parse(data.replace(record, replacement, 1))
        create = cont('corrupt_fp', 101, 22, 3)
        with self.assertRaises(parser.TranscriptError):
            self.parse(data.replace(record+create, create+record, 1))

    def test_expected_site_schema_bounds_and_wrong_rva(self):
        invalid = [None, {}, {'image_size': True, 'functions': TRAP_SITES['functions']}]
        for function, key, value in [('frd_h264_dc_check_abi', 'rva', True),
                                     ('frd_h264_dc_check_abi', 'rva', 0x1004),
                                     ('frd_h264_dc_wrong_return', 'rva', 0x3004),
                                     ('frd_h264_dc_wrong_return', 'rva', 0x1000),
                                     ('frd_h264_dc_outer_probe', 'rva', 0x4f00),
                                     ('frd_h264_dc_outer_probe', 'length', 0x434)]:
            value_set = copy.deepcopy(TRAP_SITES)
            value_set['functions'][function][key] = value
            invalid.append(value_set)
        extra = copy.deepcopy(TRAP_SITES); extra['opcode'] = 0xd43e0000; invalid.append(extra)
        for sites in invalid:
            with self.subTest(sites=sites), self.assertRaises(parser.TranscriptError):
                parser.parse_transcript(transcript('isolated'), b'', 'isolated', 0, 0, expected_trap_sites=sites)
        # 其它六个 pair 的行为不依赖这个 isolated-only 参数。
        parser.parse_transcript(transcript('register'), b'', 'register', 0, 0)

    def test_illegal_instruction_and_extra_av_remain_rejected(self):
        data = transcript('isolated')
        self.reject(b'code=80000003 flags=', b'code=c000001d flags=', data=data)
        extra = exception('corrupt_fp', 101, 1, '7ffd18078150').replace(b'code=80000003', b'code=c0000005')
        second = exception('corrupt_fp', 101, 0, '80011468')
        with self.assertRaises(parser.TranscriptError):
            self.parse(data.replace(second, extra+second, 1))

    def setUp(self):
        self.assertIsNotNone(parser, '待实现 strict bytes transcript parser')

    def parse(self, data, suite='isolated', v=0, stderr=b'', returncode=0):
        self.assertIsNotNone(parser, '待实现 strict bytes transcript parser')
        return parser.parse_transcript(data, stderr, suite, v, returncode, expected_trap_sites=TRAP_SITES)

    def reject(self, old, new, suite='isolated', v=0, data=None):
        original = transcript(suite, v) if data is None else data
        self.assertIn(old, original)
        self.assertNotEqual(old, new)
        with self.assertRaises(parser.TranscriptError if parser else AssertionError):
            self.parse(original.replace(old, new, 1), suite, v)

    def test_seven_public_pairs(self):
        for suite, v in [('register', 0), ('isolated', 0)] + [('unwind', n) for n in range(5)]:
            with self.subTest(suite=suite, variant=v):
                got = self.parse(transcript(suite, v), suite, v)
                self.assertEqual((got.suite, got.variant), (suite, v))
                self.assertEqual(got.parent_returncode, 0)
        got = self.parse(transcript('isolated'))
        self.assertEqual(len(got.children), 6)
        self.assertIn(b'\r\n', got.children[0].stdout)
        self.assertEqual(dict(got.children[-1].time)['terminate_tick'], 2600)

    def test_lf_crlf_parent_and_variable_ordinary_events(self):
        self.parse(transcript('unwind').replace(b'\n', b'\r\n'), 'unwind')
        self.parse(transcript('isolated', ordinary=9, loader=True))

    def test_exact_high64_and_independent_oracle(self):
        for old, new in [(b'0000000100000000', b'0000000000000001'),
                         (b'actual=0000000000000001 expected=0000000000000001', b'actual=0000000000000002 expected=0000000000000002'),
                         (b'0002000000000000', b'0000000000000000')]:
            self.reject(old, new, 'register')

    def test_missing_duplicate_order_unknown_masks(self):
        item = mask('preserve', 0)
        for old, new in [(item, b''), (item, item*2), (b'id=preserve', b'id=unknown'),
                         (mask('x19_lo', 1)+mask('x19_bit40', 1), mask('x19_bit40', 1)+mask('x19_lo', 1))]:
            self.reject(old, new, 'register')

    def test_final_and_unknown_trailing_data(self):
        f = final('unwind', 0)
        for old, new in [(f, b''), (f, f*2), (b'status=ABI-contract-only/nativehelper-control', b'status=failed'),
                         (f, f+b'extra\n'), (b'qualification=OPEN', b'qualification=OPEN extra=1')]:
            self.reject(old, new, 'unwind')

    def test_admission_binding_schema_and_widths(self):
        for old, new in [(b'native=aa64', b'native=8664'), (b'process=0000', b'process=aa64'),
                         (b'pointer=64', b'pointer=32'), (b'rva=1004 offset=4', b'rva=1008 offset=4'),
                         (b'rva=1000', b'rva=01000'), (b'offset=30', b'offset=31'),
                         (b'table=present', b'table=absent'), (b'outer_rva=2000', b'outer_rva=1000'),
                         (b'variant=0', b'variant=00')]:
            self.reject(old, new, 'unwind')

    def test_unwind_variant_masks_ids_livewalk(self):
        for v in range(5):
            if v == 1:
                self.reject(b'expected-missing-nonleaf-table', b'matched', 'unwind', v)
                self.reject(b'table=absent', b'table=present', 'unwind', v)
            else:
                diff = {0:b'0000000000000000', 2:b'0000000000100000', 3:b'0000000000000003', 4:b'0000000000030000'}[v]
                self.reject(b'diff='+diff+b' expected='+diff, b'diff=0000000000000001 expected=0000000000000001', 'unwind', v)
            self.reject(b'id=entry', b'id=ret', 'unwind', v)
            self.reject(b'handler=absent', b'handler=present', 'unwind', v if v != 1 else 0)
        self.reject(b'callback-helper-outer-caller outcome=matched', b'outcome=not-applicable-bad-body-metadata', 'unwind')
        self.reject(b'outcome=not-applicable-bad-body-metadata', b'callback-helper-outer-caller outcome=matched', 'unwind', 2)
        self.reject(b'caller_sp=0000000070002010', b'caller_sp=0000000070002020', 'unwind', 2)

    def test_bytes_framing_and_namespace_fail_closed(self):
        original = transcript('isolated')
        start = original.index(b'FRD1 raw case=preserve stream=stdout bytes=')
        end = original.index(b' begin', start)
        count = original[start:end].rsplit(b'=', 1)[1]
        for replacement in (b'0'+count, b'32768', b'18446744073709551616', str(int(count)+1).encode(), str(int(count)-1).encode()):
            self.reject(b'bytes='+count+b' begin', b'bytes='+replacement+b' begin')
        for old, new in [(b'\r\nFRD1 raw case=preserve stream=stdout end', b'\nFRD1 raw case=wrong_return stream=stdout end'),
                         (b'FRD1 ready case=preserve variant=0\n', b'FRD1 ready case=preserve variant=0\r\n'),
                         (b'FRD1 raw case=preserve stream=stderr bytes=0 begin', b'FRD1 raw case=preserve stream=stdout bytes=0 begin')]:
            self.reject(old, new)
        # Reframe an injected parent final as child bytes; it must never satisfy parent grammar.
        p = admission(0)+line('FRD1 ready case=preserve variant=0')+final('isolated', 0)
        first = original.index(b'FRD1 raw case=preserve stream=stdout bytes=')
        last = original.index(b'FRD1 raw case=preserve stream=stderr bytes=')
        with self.assertRaises(parser.TranscriptError):
            self.parse(original[:first]+raw('preserve', 'stdout', p)+original[last:])
        for data in (original[:-1], original+b'\x00', original.replace(b'FRD1 child', b'FRD1 ch\xffld', 1)):
            with self.assertRaises(parser.TranscriptError): self.parse(data)

    def test_child_ownership_event_continue_wait_terminal(self):
        for old, new in [(b'pid=100 tid=22 event=3', b'pid=999 tid=22 event=3'),
                         (b'case=preserve pid=100 watchdog', b'case=corrupt_fp pid=100 watchdog'),
                         (b'event=3 status=00010002', b'event=9 status=00010002'),
                         (b'event=5 status=00010002', b'event=5 status=80010001'),
                         (b'result=1 ok=1', b'result=0 ok=0'), (b'wait_attempted=1', b'wait_attempted=0'),
                         (b'wait_result_valid=1', b'wait_result_valid=0'), (b'wait_result=00000000', b'wait_result=00000102'),
                         (b'exit_continue_attempted=1', b'exit_continue_attempted=0'),
                         (b'terminal=0\nFRD1 debug_continue', b'terminal=1\nFRD1 debug_continue'),
                         (b'terminal_tick=3001', b'terminal_tick=2999'),
                         (b'exit_event_code=00000000', b'exit_event_code=80000003')]:
            self.reject(old, new)
        data = transcript('isolated', ordinary=1)
        self.reject(b'tid=30 event=2 status', b'tid=31 event=2 status', data=data)
        self.reject(b'event=2 classification=ordinary', b'event=9 classification=ordinary', data=data)
        self.reject(cont('preserve',100,22,3), b'')

    def test_probe_exception_chances_opcode_exit_and_flags(self):
        for old, new in [(b'first=1 code=80000003', b'first=0 code=80000003'),
                         (b'first=0 code=80000003', b'first=1 code=80000003'),
                         (b'code=80000003 flags=', b'code=c0000005 flags='),
                         (b'pc=80011468 ready=1', b'pc=8001146c ready=1'),
                         (b'pc=80011468 ready=1', b'pc=80011468 ready=0'),
                         (b'brk=d43e0000', b'brk=d421e020'), (b'brk=d43e0000', b'brk=d421e040'),
                         (b'brk=d43e0000', b'brk=d421e060'),
                         (b'count=0', b'count=1'), (b'exit=80000003', b'exit=00000000'),
                         (b'flags=00000000', b'flags=000000000')]:
            self.reject(old, new)
        self.reject(b'event=1 status=80010001', b'event=1 status=00010002')

    def test_loader_classification_and_parameters_preserved(self):
        data = transcript('isolated', loader=True)
        self.parse(data)
        self.reject(b'pc=7fff00001000 ready=0', b'pc=7fff00001000 ready=1', data=data)
        changed = data.replace(b'count=0', b'count=2 p0=123 p1=ffffffffffffffff', 1)
        got = self.parse(changed)
        self.assertIn(('p1', 0xffffffffffffffff), got.children[0].events[3].parameters)
        self.reject(b'count=0', b'count=16', data=data)

    def test_no_return_exact_designated_ready_timing(self):
        for old, new in [(b'terminate_tick=2600', b'terminate_tick=2599'),
                         (b'designated_timeout=1', b'designated_timeout=0'),
                         (b'timeout=1 forced=1', b'timeout=0 forced=1'),
                         (b'timeout=1 forced=1', b'timeout=1 forced=0'),
                         (b'exit=e0f00101', b'exit=00000000'),
                         (b'no_return_ms=1500', b'no_return_ms=1000'),
                         (b'termination=parent-forced', b'termination=normal'),
                         (b'first=0 second=0 code=00000000 pc=0 brk=00000000 exit=e0f00101',
                          b'first=1 second=1 code=80000003 pc=1 brk=d421e020 exit=e0f00101')]:
            self.reject(old, new)

    def test_probe_main_identity_and_exit_continue_pairing(self):
        self.reject(b'pid=101 tid=22 event=3', b'pid=101 tid=23 event=3')
        self.reject(b'case=preserve pid=100 tid=22 event=5', b'case=preserve pid=100 tid=24 event=5')
        self.reject(b'case=corrupt_fp pid=101 tid=22 first=1', b'case=corrupt_fp pid=101 tid=23 first=1')

    def test_each_register_group_uses_independent_low_and_high_mask(self):
        data = transcript('register')
        got = self.parse(data, 'register')
        self.assertEqual(len(got.masks), 60)
        for name, bit in GROUPS:
            for suffix, value in [('lo', 1 << bit), ('bit40', 1 << bit), ('fake_checker', 1 << (bit+32))]:
                with self.subTest(register=name, suffix=suffix):
                    self.reject(mask(name+'_'+suffix, value), mask(name+'_'+suffix, 0), 'register', data=data)

    def test_child_raw_ready_result_and_stderr_cannot_be_parent_facts(self):
        data = transcript('isolated')
        begin = data.index(b'FRD1 raw case=preserve stream=stdout bytes=')
        end = data.index(b'FRD1 raw case=preserve stream=stderr bytes=')
        good = admission(0, b'\r\n') + line('FRD1 ready case=preserve variant=0')
        good += line('FRD1 returned case=preserve mask=0000000000000000', b'\r\n')
        good += line('FRD1 result case=preserve outcome=normal-control')
        ready = line('FRD1 ready case=preserve variant=0')
        for payload in [good.replace(ready, b''), good.replace(ready, ready*2),
                        good.replace(b'case=preserve variant=0', b'case=corrupt_fp variant=0'),
                        good.replace(b'variant=0', b'variant=1', 1),
                        good.replace(b'rva=1000', b'rva=3000', 1),
                        good.replace(b'mask=0000000000000000', b'mask=0000000000000001'),
                        good.replace(b'outcome=normal-control', b'outcome=failed'),
                        good+line('FRD1 raw case=preserve stream=stdout end'),
                        good+b'\x00', good+b'\xff']:
            with self.subTest(payload=payload[-60:]):
                with self.assertRaises(parser.TranscriptError):
                    self.parse(data[:begin]+raw('preserve', 'stdout', payload)+data[end:])
        self.reject(raw('preserve','stderr',b''), raw('preserve','stderr',b'warning\n'))

    def test_missing_ordinary_pair_and_exit_order(self):
        data = transcript('isolated', ordinary=1)
        self.reject(cont('preserve',100,30,2), b'', data=data)
        self.reject(line('FRD1 exit_event case=preserve pid=100 tid=22 event=5 code=00000000 tick=3000 terminal=0'), b'')
        self.reject(cont('preserve',100,22,5), b'')
        self.reject(b'FRD1 child_terminal case=preserve', b'FRD1 child_time case=preserve')
        for old,new in [(b'pid=100',b'pid=0100'), (b'pid=100',b'pid=4294967296'),
                        (b'start_tick=1000',b'start_tick=18446744073709551616'),
                        (b'wait_error_valid=0',b'wait_error_valid=1'),
                        (b'exit_continue_error=00000000',b'exit_continue_error=00000001'),
                        (b'error_valid=0 error=00000000',b'error_valid=1 error=00000001'),
                        (b'go=1',b'go=0'), (b'ready=1 go=1',b'ready=0 go=1'),
                        (b'watchdog_ms=10000',b'watchdog_ms=10001')]:
            self.reject(old,new)

    def test_raw_bool_nonzero_signed32_is_preserved_for_every_continue_kind(self):
        # 覆盖 implicit create/load、ordinary、exception、exit，以及 terminal raw repeat。
        base = transcript('isolated', ordinary=1, loader=True)
        for value in (2, -1, -2147483648, 2147483647):
            with self.subTest(raw_bool=value):
                data = base.replace(b'result=1 ', f'result={value} '.encode())
                got = self.parse(data)
                for c in got.children:
                    self.assertEqual(dict(c.terminal)['exit_continue_result'], value)
                    self.assertIs(type(dict(c.terminal)['exit_continue_result']), int)
                    for event in c.events:
                        if event.kind == 'debug_continue':
                            self.assertEqual(dict(event.fields)['result'], value)
                            self.assertIs(type(dict(event.fields)['result']), int)
        # 一个 create raw BOOL 改为2，不要求其他独立API结果也随之变化。
        old = cont('preserve', 100, 22, 3)
        data = transcript('isolated').replace(old, old.replace(b'result=1 ', b'result=2 '), 1)
        got = self.parse(data)
        self.assertEqual(dict(got.children[0].events[1].fields)['result'], 2)
        self.assertEqual(dict(got.children[0].terminal)['exit_continue_result'], 1)

    def test_raw_bool_zero_out_of_range_and_noncanonical_are_rejected(self):
        for bad in (b'0', b'2147483648', b'-2147483649', b'+1', b'01', b'-01',
                    b'-0', b'00', b'1.0', b'', b' 1', b'1\t'):
            for prefix, suffix in [(b'result=', b' ok='),
                                   (b'exit_continue_result=', b' exit_continue_ok=')]:
                with self.subTest(raw_bool=bad, field=prefix):
                    self.reject(prefix+b'1'+suffix, prefix+bad+suffix)

    def test_zero_raw_bool_with_success_flags_fails_each_explicit_continue(self):
        base = transcript('isolated', ordinary=1)
        for case, pid, tid, event, status in [('preserve',100,30,2,'00010002'),
                                              ('corrupt_fp',101,22,1,'80010001'),
                                              ('preserve',100,22,5,'00010002')]:
            old = cont(case,pid,tid,event,status)
            data = base.replace(old, old.replace(b'result=1 ', b'result=0 '), 1)
            if event == 5:
                # 终态也重复0，避免重复值检查掩盖缺失的nonzero gate。
                data = data.replace(b'exit_continue_result=1 ', b'exit_continue_result=0 ', 1)
            with self.subTest(event=event):
                with self.assertRaises(parser.TranscriptError):
                    self.parse(data)

    def test_exit_raw_bool_must_repeat_exact_nonzero_value(self):
        for actual, repeat in ((2, -1), (-1, 2), (1, 2), (-2147483648, 2147483647)):
            old = cont('preserve', 100, 22, 5)
            data = transcript('isolated').replace(old, old.replace(b'result=1 ', f'result={actual} '.encode()), 1)
            data = data.replace(b'exit_continue_result=1 ', f'exit_continue_result={repeat} '.encode(), 1)
            with self.subTest(actual=actual, repeat=repeat):
                with self.assertRaises(parser.TranscriptError):
                    self.parse(data)

    def test_process_exit_can_be_a_different_last_thread_for_all_cases(self):
        # 显式 synthetic host-only：CREATE_PROCESS22、CREATE_THREAD30、EXIT_THREAD22、EXIT_PROCESS30。
        # probe仍在22；这些字节不宣称任何真实Windows调度。
        for index, case in enumerate(CASES):
            pid = 100 + index
            data = transcript('isolated')
            load = cont(case, pid, 22, 6)
            created = line(f'FRD1 debug_event case={case} pid={pid} tid=30 event=2 classification=ordinary')
            created += cont(case, pid, 30, 2)
            data = data.replace(load, load+created, 1)
            code = '00000000' if case == 'preserve' else ('e0f00101' if case == 'no_return' else '80000003')
            exit_event = line(f'FRD1 exit_event case={case} pid={pid} tid=22 event=5 code={code} tick=3000 terminal=0')
            exited = line(f'FRD1 debug_event case={case} pid={pid} tid=22 event=4 classification=ordinary')
            exited += cont(case, pid, 22, 4)
            data = data.replace(exit_event, exited+exit_event.replace(b'tid=22', b'tid=30'), 1)
            data = data.replace(cont(case,pid,22,5), cont(case,pid,30,5), 1)
            with self.subTest(case=case):
                got = self.parse(data)
                events = got.children[index].events
                self.assertEqual(dict(events[1].fields)['tid'], 22)
                self.assertEqual(dict(events[-2].fields)['tid'], 30)
                self.assertEqual(dict(events[-1].fields)['tid'], 30)
                # 改单个事件使相邻Continue不匹配，仍必须失败。
                self.reject(b'tid=30 event=5 code=', b'tid=31 event=5 code=', data=data)
                self.reject(f'case={case} pid={pid} tid=30 event=5 status='.encode(),
                            f'case={case} pid={pid} tid=31 event=5 status='.encode(), data=data)

    def test_parent_exit_stderr_pair_types_and_size_bounds(self):
        for kwargs in ({'returncode':1}, {'returncode':False}, {'stderr':b'warning\n'}, {'stderr':'must be bytes'}):
            with self.assertRaises(parser.TranscriptError): self.parse(transcript('isolated'), **kwargs)
        for suite,v in [('all',0), ('child',0), ('register',1), ('isolated',4), ('unwind',5), ('unwind',True)]:
            with self.assertRaises(parser.TranscriptError): self.parse(b'',suite,v)
        for data in ('not bytes', b'A'*(1024*1024+1), b'\x00\n', b'\xff\n'):
            with self.assertRaises(parser.TranscriptError): self.parse(data)


if __name__ == '__main__':
    unittest.main()
