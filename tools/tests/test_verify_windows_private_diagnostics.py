#!/usr/bin/env python3
"""手写失败 bytes；没有 Win32/native 成功语义。"""
import copy
import importlib.util
import json
from pathlib import Path
import unittest

PATH = Path(__file__).resolve().parents[1] / 'verify-windows-private-diagnostics.py'
SPEC = importlib.util.spec_from_file_location('diagnostics', PATH)
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)


def api(result=1, got=0):
    return dict(attempted=1, result=result, error_valid=int(result == 0), error=5 if result == 0 else 0, got=got)


def unattempted():
    return dict(attempted=0, result=0, error_valid=0, error=0, got=0)


def without_header_evidence(rows):
    rows[-1]['diagnostic_failed'] = 1
    a = rows[4]
    a.update(hash_attempted=0, hash_status=0, hash_bytes=0, hash_stages_attempted=0,
             hash_stages=[0]*6, hash_failure='not-attempted', sha256=None,
             code_size=0, code_start='0'*16, mapped=0, matched=0, remote='', disk='',
             file_machine=0, file_image_size=0, file_offset=0, section_rva=0,
             section_raw=0, section_size=0, section_flags=0)
    for field in ('header_seek', 'header_read', 'code_seek', 'code_read', 'seek', 'read', 'rpm'):
        a[field] = unattempted()
    return a


def hash_failure(rows, size, done, reason):
    rows[-1]['diagnostic_failed'] = 1
    a = rows[4]
    a['before']['size'] = a['after']['size'] = size
    a.update(sha256=None, hash_bytes=done, hash_failure=reason,
             hash_stages_attempted=55 if done else 51)
    a['seek'] = api() if done else unattempted()
    a['read'] = api(got=65536) if done else unattempted()
    return a


def context(pc=0x1100):
    return dict(flags=0x400007, pc=f'{pc:016x}', sp='0000000000200000', fp='0000000000000000',
                lr='0000000000001104', pstate=0, x=['0000000000000000'] * 29,
                d8_15=['0000000000000000'] * 8)


def fixture():
    start = dict(type='begin', version=1, case='corrupt_fp', pid=123, events=3, modules=1,
                 overflow=0, reference_rva=0x4000)
    module = dict(type='module', index=0, pid=123, tid=456, load_seq=1, unload_seq=0,
                  base='0000000000001000', size=4096, machine=0xaa64, header=api(got=4096),
                  duplicate=api(), event_close=api(), file_close=api())
    events = []
    for seq, code, first in ((2, 0x80000003, 1), (3, 0xc0000005, 1), (4, 0x80000003, 0)):
        events.append(dict(type='event', seq=seq, pid=123, tid=456, code=code, first=first,
                           pc='0000000000001100', open=api(), owner=api(), thread_id=api(),
                           actual_pid=123, actual_tid=456, context_api=api(7), close=api(),
                           context=context(), reference_api=api(got=944), reference_valid=1,
                           baseline=context(0x1500), reference_identity=dict(magic='4652444449414731', pid=123, tid=456, case_id=1, complete=1), frames='unknown-callsite-SP-proof'))
    identity = dict(volume=1, index='0000000000000001', size=4096, write='0000000000000002')
    av = dict(type='av', seq=3, pid=123, tid=456, module=0, base='0000000000001000', rva=256,
              path_api=api(26), path_valid=1, path_utf16='\\\\?\\C:\\Windows\\fixture.dll'.encode('utf-16-le').hex(),
              path_length=26, file_machine=0xaa64, file_image_size=4096, file_offset=192,
              section_rva=0, section_raw=0, section_size=4096, section_flags=0x60000020,
              before_api=api(), before=identity, after_api=api(), after=copy.deepcopy(identity),
              stable=1, hash_attempted=1, hash_status=0, sha256='ab'*32, hash_failure='none',
              hash_bytes=4096, hash_stages_attempted=63, hash_stages=[0]*6,
              header_seek=api(), header_read=api(got=4096), code_seek=api(), code_read=api(got=256),
              code_start='00000000000010c0', code_size=256, mapped=1, matched=1,
              seek=api(), read=api(got=4096), rpm=api(got=256), remote='aa'*256, disk='aa'*256)
    end = dict(type='end', pid=123, failed=1, diagnostic_failed=0, native='unresolved')
    return [start, module, events[0], events[1], av, events[2], end]


def encoded(records):
    return b''.join(b'FRDD1 ' + json.dumps(x, separators=(',', ':')).encode() + b'\n' for x in records)


class DiagnosticTests(unittest.TestCase):
    def test_failed_file_prerequisites_cannot_support_digest_or_matched_code(self):
        for field in ('seek', 'read', 'header_seek', 'code_seek'):
            for failed in (0, 1):
                rows=fixture(); rows[4][field]=api(0); rows[-1]['diagnostic_failed']=failed
                with self.subTest(field=field, failed=failed), self.assertRaises(M.DiagnosticError):
                    M.parse(encoded(rows))

    def test_partial_final_hash_reads_cannot_support_full_digest(self):
        for size, got in ((4096, 4095), (65536, 0), (65536, 65535),
                          (65537, 0), (65537, 65536), (131073, 0)):
            rows=fixture(); a=rows[4]
            a['before']['size']=a['after']['size']=a['hash_bytes']=size
            a['read']=api(got=got)
            with self.subTest(size=size, got=got), self.assertRaises(M.DiagnosticError):
                M.parse(encoded(rows))

    def test_exact_final_hash_chunk_lengths(self):
        for size in (4096, 65536, 65537, 131072, 131073, 32*1024*1024):
            rows=fixture(); a=rows[4]
            a['before']['size']=a['after']['size']=a['hash_bytes']=size
            a['read']=api(7, got=(size-1)%65536+1)
            with self.subTest(size=size): M.parse(encoded(rows))

    def test_failed_or_partial_header_has_no_downstream_evidence(self):
        for seek, read in ((api(0), unattempted()), (api(), api(0, got=17)),
                           (api(), api(got=4095))):
            rows=fixture(); a=without_header_evidence(rows)
            a.update(header_seek=seek, header_read=read)
            with self.subTest(seek=seek, read=read): M.parse(encoded(rows))
            rows[-1]['diagnostic_failed']=0
            with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_failed_code_read_preserves_independent_full_hash(self):
        for seek, read in ((api(0), unattempted()), (api(), api(0, got=64)),
                           (api(), api(got=255))):
            rows=fixture(); rows[-1]['diagnostic_failed']=1
            rows[4].update(code_seek=seek, code_read=read, rpm=unattempted(), remote='', disk='', matched=0)
            with self.subTest(seek=seek, read=read): M.parse(encoded(rows))

    def test_unmapped_code_and_unusable_header_preserve_failed_observations(self):
        rows=fixture(); rows[-1]['diagnostic_failed']=1
        rows[4].update(mapped=0, matched=0, code_start='0'*16, code_seek=unattempted(),
                       code_read=unattempted(), rpm=unattempted(), disk='', remote='')
        M.parse(encoded(rows))  # 无唯一代码映射不妨碍独立完整文件 hash。
        rows=fixture(); a=without_header_evidence(rows)
        a.update(header_seek=api(), header_read=api(got=4096), file_machine=0x8664, file_image_size=4096)
        M.parse(encoded(rows))  # 完整 header 字节不等于匹配的 PE header。
        for size in (4095, 32*1024*1024+1):
            rows=fixture(); a=without_header_evidence(rows)
            a['before']['size']=a['after']['size']=size
            with self.subTest(size=size): M.parse(encoded(rows))

    def test_failed_hash_seek_retains_previous_read_only_after_covered_chunk(self):
        for size, done in ((4096, 0), (65537, 65536), (131073, 65536), (131073, 131072)):
            rows=fixture(); a=hash_failure(rows, size, done, 'file-read'); a['seek']=api(0)
            with self.subTest(size=size, done=done): M.parse(encoded(rows))
            a['read']=api(got=65535) if done else api(got=1)
            with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_failed_or_partial_hash_read_remains_observable(self):
        for size, done in ((4096, 0), (65537, 65536), (131073, 65536), (131073, 131072)):
            requested=min(size-done, 65536)
            for result, got in ((0, 0), (0, requested), (1, requested-1)):
                rows=fixture(); a=hash_failure(rows, size, done, 'file-read')
                a.update(seek=api(), read=api(result, got=got))
                with self.subTest(size=size, done=done, result=result, got=got): M.parse(encoded(rows))
                a['read']=api(got=requested)
                with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_budget_keeps_previous_full_chunk_and_requires_failure_latch(self):
        for size, done in ((4096, 0), (65537, 65536), (131073, 131072)):
            rows=fixture(); hash_failure(rows, size, done, 'budget')
            with self.subTest(size=size, done=done): M.parse(encoded(rows))
            rows[-1]['diagnostic_failed']=0
            with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_hash_data_failure_has_full_uncredited_current_read(self):
        for size, done in ((4096, 0), (65537, 65536), (131073, 65536)):
            rows=fixture(); a=hash_failure(rows, size, done, 'bcrypt')
            a.update(seek=api(), read=api(got=min(size-done, 65536)), hash_stages_attempted=55, hash_status=0xc0000001)
            a['hash_stages'][2]=0xc0000001
            with self.subTest(size=size, done=done): M.parse(encoded(rows))
            a['read']['got']-=1
            with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_hash_setup_finish_and_cleanup_failures_remain_observable(self):
        for stage in (0, 1, 3, 4, 5):
            rows=fixture(); rows[-1]['diagnostic_failed']=1; a=rows[4]
            a.update(sha256=None, hash_status=0xc0000001)
            a['hash_stages'][stage]=0xc0000001
            if stage < 2:
                a.update(hash_bytes=0, hash_failure='bcrypt', hash_stages_attempted=1 if stage==0 else 35,
                         seek=unattempted(), read=unattempted())
            elif stage==3: a['hash_failure']='bcrypt'
            with self.subTest(stage=stage): M.parse(encoded(rows))

    def test_absent_output_does_not_excuse_impossible_downstream_attempts(self):
        rows=fixture(); a=without_header_evidence(rows); a['header_seek']=api(0)
        a['header_read']=api(0)
        with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))
        rows=fixture(); rows[-1]['diagnostic_failed']=1; a=rows[4]
        a.update(code_seek=api(0), code_read=api(0), rpm=unattempted(), remote='', disk='', matched=0)
        with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))
        rows=fixture(); a=hash_failure(rows, 65537, 65536, 'file-read'); a['seek']=api(0)
        a['sha256']='ab'*32
        with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_actual_nonzero_bool_and_individual_contexts_have_no_native_authority(self):
        got = M.parse(encoded(fixture()))
        self.assertEqual(got['native'], 'unresolved')
        self.assertEqual(got['blocks'][0]['contexts'], 3)
        self.assertEqual(got['blocks'][0]['access_violations'], 1)
        self.assertNotIn('accepted', got)

    def test_exception_address_and_actual_context_pc_remain_independent(self):
        rows=fixture(); rows[2]['context']['pc']='0000000000001104'
        result=M.parse(encoded(rows))
        self.assertEqual(result['blocks'][0]['context_pc_mismatches'], 1)
        self.assertEqual(result['blocks'][0]['records'][2]['context']['pc'], '0000000000001104')

    def test_unknown_module_stays_explicit(self):
        rows = fixture()
        rows[4] = dict(type='av', seq=3, pid=123, tid=456, module=None, reason='module-or-file-unknown')
        rows[-1]['diagnostic_failed'] = 1
        self.assertEqual(M.parse(encoded(rows))['blocks'][0]['unknown_modules'], 1)

    def test_failed_context_cannot_manufacture_zero_registers(self):
        rows = fixture(); rows[2]['context_api'] = api(0); rows[2]['context'] = None
        rows[-1]['diagnostic_failed'] = 1
        M.parse(encoded(rows))
        rows[2]['context'] = context()
        with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_partial_reads_are_visible_refusals_not_zero_contexts(self):
        rows=fixture(); rows[-1]['diagnostic_failed']=1
        rows[2]['reference_api']=api(0, got=100)
        rows[2]['reference_valid']=0; rows[2]['baseline']=None; rows[2]['reference_identity']=None
        rows[4]['rpm']=api(0, got=64); rows[4]['remote']=''; rows[4]['matched']=0
        result=M.parse(encoded(rows))
        self.assertEqual(result['blocks'][0]['records'][2]['reference_api']['got'], 100)
        self.assertEqual(result['blocks'][0]['records'][4]['rpm']['got'], 64)

    def test_path_required_size_is_not_a_fabricated_win32_error(self):
        rows=fixture(); a=without_header_evidence(rows)
        a.update(path_api=api(2048), path_valid=0, path_length=2048, path_utf16='', sha256=None)
        M.parse(encoded(rows))
        a['path_api']['error_valid']=1; a['path_api']['error']=122
        with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_hash_budget_refusal_cannot_claim_file_digest(self):
        rows=fixture(); hash_failure(rows, 4096, 0, 'budget')
        M.parse(encoded(rows))
        rows[4]['sha256']='ab'*32
        with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_ownership_flags_order_and_lengths_are_not_advisory(self):
        changes = [(0, 'pid', True), (0, 'events', 2), (0, 'modules', 33),
                   (1, 'pid', 124), (1, 'size', 1<<64), (1, 'base', 'fffffffffffffff0'),
                   (2, 'seq', 0), (3, 'seq', 2), (2, 'pid', 124), (2, 'tid', 0),
                   (2, 'actual_tid', 457), (2, 'actual_pid', 124),
                   (2, 'context', {**context(), 'flags': 0}),
                   (2, 'context', {**context(), 'x': ['0']*29}),
                   (2, 'reference_api', api(got=943)), (2, 'reference_valid', 0),
                   (4, 'tid', 457), (4, 'module', 1), (4, 'rva', 255),
                   (4, 'remote', 'aa'*257), (4, 'disk', 'bb'*256),
                   (4, 'rpm', api(0)), (4, 'rpm', api(got=255)),
                   (4, 'sha256', 'a'*63), (4, 'after', dict(volume=2, index='0'*16, size=4096, write='0'*16)),
                   (4, 'hash_bytes', 4095), (4, 'hash_stages_attempted', 31),
                   (4, 'file_offset', 193), (4, 'section_flags', 0), (4, 'file_machine', 0x8664), (4, 'file_image_size', 8192),
                   (4, 'code_start', '0000000000002000'), (4, 'code_size', 257),
                   (4, 'path_utf16', '00'), (6, 'failed', 0), (6, 'native', 'passed')]
        for index, key, value in changes:
            rows = fixture(); rows[index][key] = value
            with self.subTest(index=index, key=key, value=value):
                with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_duplicate_module_and_changed_baseline_cannot_be_authenticated(self):
        rows=fixture(); extra=copy.deepcopy(rows[1]); extra['index']=1
        rows.insert(2,extra); rows[0]['modules']=2
        with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))
        rows=fixture(); rows[3]['baseline']['sp']='0000000000200010'
        with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_stale_module_and_context_without_owned_thread_reject(self):
        for index, key, value in [(1,'unload_seq',3),(2,'open',api(0)),(2,'owner',api(0)),(2,'thread_id',api(0))]:
            rows=fixture(); rows[index][key]=value
            with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_framing_duplicates_unknown_fields_and_boundaries(self):
        raw = encoded(fixture())
        for bad in [b'', raw[:-1], raw.replace(b'"version":1', b'"version":1,"version":1'),
                    encoded(fixture()[1:]), encoded(fixture()[:-1]), encoded(fixture()+fixture()[1:]),
                    b'FRDD1 '+b'x'*(1024*1024), raw.replace(b'"version":1', b'"version":NaN')]:
            with self.assertRaises(M.DiagnosticError): M.parse(bad)
        rows=fixture(); rows[2]['trusted_native_success']=True
        with self.assertRaises(M.DiagnosticError): M.parse(encoded(rows))

    def test_ordinary_fail_lines_allowed_only_outside_blocks(self):
        M.parse(b'FRD1 failed id=old win32=109\n'+encoded(fixture()))
        rows=encoded(fixture()).splitlines(keepends=True)
        with self.assertRaises(M.DiagnosticError): M.parse(rows[0]+b'garbage\n'+b''.join(rows[1:]))


if __name__ == '__main__': unittest.main()
