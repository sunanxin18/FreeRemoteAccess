"""独立小型 host-only 控制；不产生 Windows/native receipt。"""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import tempfile
import unittest
import zipfile
import warnings

MODULE_PATH = Path(__file__).resolve().parents[1] / 'admit-windows-private-artifact.py'
spec = importlib.util.spec_from_file_location('admission', MODULE_PATH)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def packed(files):
    stream = io.BytesIO()
    with warnings.catch_warnings(record=True) as observed:
        warnings.simplefilter('always')
        with zipfile.ZipFile(stream, 'w', zipfile.ZIP_DEFLATED) as z:
            for name, data in files:
                z.writestr(name, data)
        duplicates = len(files) - len({name for name, _ in files})
        assert len(observed) == duplicates
        assert all(isinstance(w.message, UserWarning) and str(w.message).startswith('Duplicate name:') for w in observed)
    return stream.getvalue()


def encoded(value):
    return json.dumps(value).encode()


class AdmissionControls(unittest.TestCase):
    def test_reviewed_checkout_pins_match_current_bytes(self):
        for name, pin in m.CHECKOUT_PINS.items():
            self.assertEqual(hashlib.sha256((MODULE_PATH.parents[1]/name).read_bytes()).hexdigest(), pin)

    def test_json_strict(self):
        self.assertEqual(m.strict_json(b'{"x":1}'), {'x': 1})
        for data in (b'{"x":1,"x":2}', b'{"x":NaN}', b'{"x":Infinity}', b'{"x":1.0}', b'{"x":1e0}', b'[]', b'{"x":9223372036854775808}'):
            with self.subTest(data=data), self.assertRaises(m.AdmissionError):
                m.strict_json(data)

    def test_windows_names(self):
        for name in ('../x', '/x', 'a//b', 'a/./b', 'C:/x', 'a\\b', 'a:b', 'NUL.txt', 'aux', 'COM1.obj', 'LPT9.x', 'x. ', 'x.', 'a/PRN/y', 'x\x00y', 'a\ny'):
            with self.subTest(name=name), self.assertRaises(m.AdmissionError):
                m.member_name(name)
        self.assertEqual(m.member_name('inputs/.github/workflows/test.yml'), 'inputs/.github/workflows/test.yml')

    def test_zip_exact_and_crc(self):
        self.assertEqual(m.read_zip(packed([('a', b'alpha')]), {'a'}), {'a': b'alpha'})
        for files in ([('a', b'a'), ('a', b'b')], [('A', b'a'), ('a', b'b')], [('a/b', b'a'), ('a', b'b')], [('a', b'a'), ('b', b'b')]):
            with self.subTest(files=files), self.assertRaises(m.AdmissionError):
                m.read_zip(packed(files), {'a'})
        raw = bytearray(packed([('a', b'alpha')]))
        raw[31] ^= 0xff
        with self.assertRaises(m.AdmissionError):
            m.read_zip(bytes(raw), {'a'})

    def test_zip_symlink_and_unsupported_extra(self):
        for mode, extra in ((stat.S_IFLNK | 0o777, b''), (stat.S_IFREG | 0o600, b'\x01\x00\x00\x00')):
            stream = io.BytesIO()
            with zipfile.ZipFile(stream, 'w') as z:
                info = zipfile.ZipInfo('a'); info.create_system = 3; info.external_attr = mode << 16; info.extra = extra
                z.writestr(info, b'x')
            with self.assertRaises(m.AdmissionError):
                m.read_zip(stream.getvalue(), {'a'})

    def test_bomb_declared_size_before_read(self):
        raw = bytearray(packed([('a', b'x')]))
        index = raw.index(b'PK\x01\x02')
        raw[index + 24:index + 28] = (2**30).to_bytes(4, 'little')
        with self.assertRaises(m.AdmissionError):
            m.read_zip(bytes(raw), {'a'})

    def test_manifest_is_not_authority(self):
        data = {'a': b'alpha'}
        manifest = {'schema': 'control.members', 'files': {'a': hashlib.sha256(b'alpha').hexdigest()}}
        raw = packed([('a', b'alpha'), ('members.json', encoded(manifest))])
        archive = {'schema': 'control.archive', 'sha256': hashlib.sha256(raw).hexdigest(), 'members_sha256': hashlib.sha256(encoded(manifest)).hexdigest(), 'strict_members': ['a', 'members.json']}
        self.assertEqual(m.bundle(raw, encoded(archive), {'a'}, 'control')['a'], b'alpha')
        for allowed in ({'a', 'hidden/workflow'}, set()):
            with self.assertRaises(m.AdmissionError):
                m.bundle(raw, encoded(archive), allowed, 'control')
        manifest['files']['a'] = '0' * 64
        broken = packed([('a', b'alpha'), ('members.json', encoded(manifest))])
        archive['sha256'] = hashlib.sha256(broken).hexdigest(); archive['members_sha256'] = hashlib.sha256(encoded(manifest)).hexdigest()
        with self.assertRaises(m.AdmissionError):
            m.bundle(broken, encoded(archive), {'a'}, 'control')

    def test_nested_integer_bool_confusion(self):
        self.assertFalse(m.same({'files': [{'size': False}]}, {'files': [{'size': 0}]}))
        self.assertFalse(m.same({'section': True}, {'section': 1}))
        self.assertTrue(m.same({'files': [{'size': 0}]}, {'files': [{'size': 0}]}))

    def test_exact_seven_file_transport_and_unsupported_flags(self):
        prefixes = ('task4c5c3b2c-install-2-1', 'task4c5c3b2c-compile-2-1', 'task4c5c3b2e-contract-2-1')
        names = {p+'/'+n for p in prefixes for n in ('evidence.zip', 'archive.json')} | {prefixes[2]+'/terminal-status.json'}
        raw = packed([(n, b'control') for n in sorted(names)])
        self.assertEqual(set(m.read_zip(raw, names)), names)
        with self.assertRaises(m.AdmissionError):
            m.read_zip(packed([(n, b'control') for n in sorted(names)[:-1]]), names)
        for attribute, value in (('flag', 1), ('reparse', 0x400), ('directory', 0x10)):
            data = bytearray(packed([('a', b'data')]))
            central = data.index(b'PK\x01\x02')
            if attribute == 'flag':
                data[central+8:central+10] = value.to_bytes(2, 'little')
            else:
                data[central+38:central+42] = value.to_bytes(4, 'little')
            with self.subTest(attribute=attribute), self.assertRaises(m.AdmissionError):
                m.read_zip(bytes(data), {'a'})

    def test_integer_types(self):
        for value in (True, False, 1.0, '1', 0, -1, 2**63):
            with self.subTest(value=value), self.assertRaises(m.AdmissionError):
                m.uint(value, 'control', positive=True)
        self.assertEqual(m.uint(1, 'control', positive=True), 1)

    def test_context_binding(self):
        context = {'repository': m.REPOSITORY, 'repository_id': m.REPOSITORY_ID, 'source_sha': '1' * 40, 'run_id': 12, 'run_attempt': 1, 'compile_job_result': 'success', 'artifact': {'id': 34, 'name': 'task4c5c3b2f2-input-12-1', 'size_in_bytes': 99, 'digest': 'sha256:' + '2' * 64}}
        metadata = {**context['artifact'], 'expired': False, 'workflow_run': {'id': 12, 'repository_id': m.REPOSITORY_ID, 'head_repository_id': m.REPOSITORY_ID, 'head_sha': '1' * 40}}
        m.validate_context(context, metadata)
        for key, value in (('id', 35), ('name', 'other'), ('size_in_bytes', 100), ('digest', 'sha256:' + '3' * 64), ('expired', True)):
            with self.subTest(key=key), self.assertRaises(m.AdmissionError):
                m.validate_context(context, {**metadata, key: value})
        for key, value in (('id', 13), ('repository_id', m.REPOSITORY_ID + 1), ('head_repository_id', m.REPOSITORY_ID + 1), ('head_sha', '3' * 40)):
            with self.subTest(key=key), self.assertRaises(m.AdmissionError):
                m.validate_context(context, {**metadata, 'workflow_run': {**metadata['workflow_run'], key: value}})
        for key, value in (('compile_job_result', True), ('compile_job_result', 'packaging-pending'), ('run_attempt', True), ('source_sha', 'A' * 40)):
            with self.subTest(key=key), self.assertRaises(m.AdmissionError):
                m.validate_context({**context, key: value}, metadata)

    def test_terminal_authority(self):
        summary = {'schema': m.CONTRACT_SCHEMA, 'status': 'packaging-pending', 'build_status': 'compiled-only', 'failures': [], 'terminal_authority': m.TERMINAL_AUTHORITY}
        raw = encoded(summary); archive = {'schema': m.CONTRACT_SCHEMA + '.archive'}
        terminal = {'schema': m.CONTRACT_SCHEMA + '.terminal', 'status': 'compiled-only', 'archive_verified': True, 'archive': archive, 'summary_sha256': hashlib.sha256(raw).hexdigest(), 'failures': [], 'first_failure': None, 'authority_filename': 'terminal-status.json', 'scope': summary.get('scope')}
        m.terminal_authority(encoded(terminal), raw, archive)
        for key, value in (('archive_verified', 1), ('status', 'packaging-pending'), ('first_failure', {}), ('authority_filename', 'terminal-precommit.json'), ('summary_sha256', '0' * 64)):
            with self.subTest(key=key), self.assertRaises(m.AdmissionError):
                m.terminal_authority(encoded({**terminal, key: value}), raw, archive)

    def test_fresh_and_alias(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            m.fresh_destination(root / 'new')
            for path in (root / 'new', root / 'new' / '..' / 'alias'):
                with self.assertRaises(m.AdmissionError):
                    m.fresh_destination(path)
            (root / 'link').symlink_to(root / 'new', target_is_directory=True)
            with self.assertRaises(m.AdmissionError):
                m.fresh_destination(root / 'link' / 'child')


class SealAndProducerControls(unittest.TestCase):
    def test_relative_input_set_and_after_mutation(self):
        snapshot = {'tools/x.py': {'path': '/current/tools/x.py', 'sha256': hashlib.sha256(b'x').hexdigest()}}
        old = {'tools/x.py': {'path': '/linux/tools/x.py', 'sha256': snapshot['tools/x.py']['sha256']}}
        files = {'source-inputs.json': encoded(old), 'source-inputs-after.json': encoded(old), 'inputs/tools/x.py': b'x'}
        m.source_copy(files, snapshot, '/linux')
        for key, data in (('inputs/tools/x.py', b'y'), ('source-inputs-after.json', b'{}'), ('source-inputs.json', encoded({**old, 'extra': old['tools/x.py']}))):
            with self.subTest(key=key), self.assertRaises(m.AdmissionError):
                m.source_copy({**files, key: data}, snapshot, '/linux')

    def test_same_run_attempt_is_independent(self):
        expected = {'source_sha': '1' * 40, 'run_id': 2, 'run_attempt': 3}
        summary = {'github': {'GITHUB_SHA': '1' * 40, 'GITHUB_RUN_ID': '2', 'GITHUB_RUN_ATTEMPT': '3'}, 'not_attempted': [], 'failures': []}
        m.same_run(summary, expected)
        for key, value in (('GITHUB_SHA', '2' * 40), ('GITHUB_RUN_ID', '3'), ('GITHUB_RUN_ATTEMPT', '4')):
            with self.subTest(key=key), self.assertRaises(m.AdmissionError):
                m.same_run({**summary, 'github': {**summary['github'], key: value}}, expected)

    def test_reviewed_module_pin_checked_before_import(self):
        from unittest import mock
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            for name in m.CHECKOUT_PINS:
                path = root / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(b'unreviewed')
            with mock.patch.object(m, '_pinned_source_module') as loader:
                with self.assertRaises(m.AdmissionError):
                    m.reviewed(root)
                loader.assert_not_called()

    def test_data_only_seal_detects_all_byte_and_extra_mutations(self):
        from unittest import mock
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(); output = root / 'output'; output.mkdir()
            source = root / 'source'; source.write_bytes(b'source')
            archive = root / 'outer.zip'; archive.write_bytes(b'zip')
            pe = output / 'control.exe'; pe.write_bytes(b'host-control-data')
            seal = {'schema': m.SCHEMA, 'status': 'admitted-static-only', 'checkout_root': str(root), 'destination': str(output), 'outer_zip': str(archive), 'outer_identity': m.identity(archive), 'checkout': {'source': m.identity(source)}, 'output_tree': m.snapshot_tree(output)}
            sealpath = output / m.SEAL_FILE; sealpath.write_bytes(m.json_bytes(seal))
            with mock.patch.object(m, 'reviewed'):
                self.assertTrue(m.revalidate(seal))
                for path in (source, archive, pe, sealpath):
                    original = path.read_bytes(); path.write_bytes(original + b'changed')
                    with self.subTest(path=path), self.assertRaises(m.AdmissionError):
                        m.revalidate(seal)
                    path.write_bytes(original)
                extra = output / 'extra.dll'; extra.write_bytes(b'extra')
                with self.assertRaises(m.AdmissionError):
                    m.revalidate(seal)
                extra.unlink()
                (output / 'alias').symlink_to(pe)
                with self.assertRaises(m.AdmissionError):
                    m.revalidate(seal)

    def test_record_bool_exit_pid_and_unbound_stream_rejected(self):
        files = {'cc.stdout': b'out', 'cc.stderr': b''}
        item = {'argv': ['fixed'], 'cwd': '/linux', 'environment': {'PATH': '/opt/frd-llvm-mingw-20250709/bin:/usr/bin:/bin', 'LC_ALL': 'C', 'LANG': 'C', 'HOME': '/tmp'}, 'pid': 3, 'elapsed_ns': 1, 'wait_terminal': 'exited', 'returncode': 0, 'failure': None, 'stdout': {'name': 'cc.stdout', 'sha256': hashlib.sha256(b'out').hexdigest()}, 'stderr': {'name': 'cc.stderr', 'sha256': hashlib.sha256(b'').hexdigest()}}
        files['cc.receipt.json'] = encoded(item)
        m.record(files, 'cc', ['fixed'], '/linux')
        for key, value in (('pid', True), ('returncode', False), ('returncode', 1), ('argv', ['injected']), ('wait_terminal', 'timeout-killed')):
            with self.subTest(key=key), self.assertRaises(m.AdmissionError):
                m.record({**files, 'cc.receipt.json': encoded({**item, key: value})}, 'cc', ['fixed'], '/linux')
        with self.assertRaises(m.AdmissionError):
            m.record({**files, 'cc.stdout': b'replaced'}, 'cc', ['fixed'], '/linux')

    def test_coff_bytes_machine_and_truncation(self):
        _, support, _ = m.reviewed(MODULE_PATH.parents[1])
        # 独立一条 ret 的 COFF 容器数据，不是可运行 ABI helper。
        raw = bytes.fromhex('64aa0100 00000000 00000000 00000000 0000 0000 '
                            '2e74657874000000 00000000 00000000 04000000 3c000000 '
                            '00000000 00000000 0000 0000 20000060 c0035fd6')
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve() / 'control.obj'; path.write_bytes(raw)
            self.assertEqual(support.coff_admit(path)['machine'], '0xAA64')
            for data in (b'MZ' + raw[2:], raw[:-1], b'\x64\x86' + raw[2:]):
                path.write_bytes(data)
                with self.assertRaises(support.GateError):
                    support.coff_admit(path)

    def test_reviewed_pe_variant1_import_gate_and_bad5(self):
        # 只加载已完整 pin 检查的静态 helper；不运行原19/36 suite。
        compiler, support, _ = m.reviewed(MODULE_PATH.parents[1])
        bad = {'wait_terminal': 'exited', 'returncode': 1, 'failure': None}
        diagnostic = '<unknown>:0: error: Incorrect size for frd_h264_dc_check_abi epilogue: 40 bytes of instructions in range, but .seh directives corresponding to 44 bytes'
        self.assertEqual(support.candidate_outcome(bad, diagnostic, 'bad-unwind-5'), 'rejected')
        self.assertEqual(support.candidate_outcome(bad, diagnostic, 'bad-unwind-4'), 'error')
        self.assertEqual(support.candidate_outcome(bad, diagnostic + '\nextra', 'bad-unwind-5'), 'error')
        # 手写最小 PE 数据表只证明 imports selector；没有可执行代码或 native 成功语义。
        import struct
        data = bytearray(4096); data[:2] = b'MZ'; struct.pack_into('<I', data, 60, 128); data[128:132] = b'PE\0\0'
        struct.pack_into('<HHIIIHH', data, 132, 0xAA64, 1, 0, 0, 0, 240, 2)
        opt = 152; struct.pack_into('<H', data, opt, 0x20b); struct.pack_into('<Q', data, opt+24, 0x140000000)
        struct.pack_into('<II', data, opt+56, 8192, 512); struct.pack_into('<H', data, opt+68, 3); struct.pack_into('<I', data, opt+108, 16)
        struct.pack_into('<II', data, opt+120, 0x1200, 80); struct.pack_into('<II', data, opt+136, 0x1000, 8)
        struct.pack_into('<8sIIIIIIHHI', data, 392, b'.text\0\0\0', 3584, 4096, 3584, 512, 0, 0, 0, 0, 0x60000020)
        struct.pack_into('<II', data, 512, 0x1100, 0x1120); struct.pack_into('<II', data, 800, 4 | (1 << 27), 0xe4)
        nextname = 0x1600
        for index, (dll, names) in enumerate((('KERNEL32.dll', ('RtlCaptureContext', 'RtlLookupFunctionEntry', 'OpenThread', 'GetThreadContext')), ('msvcrt.dll', ('exit',)), ('bcrypt.dll', ('BCryptOpenAlgorithmProvider', 'BCryptCreateHash', 'BCryptHashData', 'BCryptFinishHash', 'BCryptDestroyHash', 'BCryptCloseAlgorithmProvider')))):
            dllrva, thunk = 0x1280 + index*32, 0x1300 + index*64
            struct.pack_into('<IIIII', data, 1024+index*20, thunk, 0, 0, dllrva, thunk)
            raw = dll.encode()+b'\0'; data[dllrva-3584:dllrva-3584+len(raw)] = raw
            for n, symbol in enumerate(names):
                raw = b'\0\0'+symbol.encode()+b'\0'; data[nextname-3584:nextname-3584+len(raw)] = raw
                struct.pack_into('<Q', data, thunk-3584+n*8, nextname); nextname += len(raw)
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve()/'host-control.exe'; path.write_bytes(data)
            for variant in range(5):
                with self.assertRaisesRegex(compiler.GateError, 'KERNEL32 缺少精确命名 RtlVirtualUnwind'):
                    compiler.pe_admit(path, variant=variant)
            data[132:134] = (0x8664).to_bytes(2, 'little'); path.write_bytes(data)
            with self.assertRaises(compiler.GateError):
                compiler.pe_admit(path, variant=1)



class ProducerConsumerDiagnosticInventoryControls(unittest.TestCase):
    # 独立规格：不能从 producer ranges/member_sets 或可写 archive 学到期望名字。
    OLD_NAMES = ('main','native_admission','linked_admission','child_main','isolated_suite',
                 'frd_h264_dc_check_abi','frd_h264_dc_outer_probe','frd_h264_dc_wrong_return')
    NEW_NAMES = ('frd_h264_dc_abi_sp_failure_fragment','frd_h264_dc_preserve',
                 'frd_h264_dc_corrupt_fp','frd_h264_dc_corrupt_sp16',
                 'frd_h264_dc_corrupt_sp8','frd_h264_dc_no_return')
    SUFFIXES = ('.stdout','.stderr','.receipt.json','.argv.json')

    def independent_diagnostics(self, names):
        return {'variant-%d-disassemble-%s%s' % (variant,name,suffix)
                for variant in range(5) for name in names for suffix in self.SUFFIXES}

    def accepted_producer_packet(self):
        compiler,support,snapshot=m.reviewed(MODULE_PATH.parents[1])
        old={name:item for name,item in snapshot.items()
             if name not in {compiler.FIXTURE+'/'+n for n in compiler.PINS}|set(compiler.OWN)}
        allowed=m.member_sets(compiler,support,old,snapshot)[2]
        expected=self.independent_diagnostics(self.OLD_NAMES+self.NEW_NAMES)
        expected|={'variant-%d-disassemble-capture_callback%s'%(variant,suffix)
                   for variant in (0,4) for suffix in self.SUFFIXES}
        self.assertEqual(len(expected),288)
        actual={name for name in allowed if '-disassemble-' in name}
        # Removing any new fixed family would recreate the actual producer/consumer failure.
        self.assertEqual(actual,expected)
        producer_names=(allowed-actual)|expected
        files={name:b'controlled producer member' for name in producer_names}
        self.assertEqual(set(m.read_zip(packed(sorted(files.items())),allowed)),producer_names)
        return files,allowed

    def test_complete_fixed_six_by_five_by_four_producer_inventory_is_consumed(self):
        files,allowed=self.accepted_producer_packet()
        self.assertEqual(len(files),454)
        self.assertEqual(len(self.independent_diagnostics(self.NEW_NAMES)),120)
        self.assertFalse(any('isolated_child.' in name for name in allowed))
        self.assertNotIn('variant-1-disassemble-capture_callback.argv.json',allowed)
        self.assertNotIn('variant-2-disassemble-capture_callback.argv.json',allowed)
        self.assertNotIn('variant-3-disassemble-capture_callback.argv.json',allowed)

    def test_unknown_and_each_missing_new_diagnostic_remain_refused(self):
        files,allowed=self.accepted_producer_packet()
        for name in self.independent_diagnostics(self.NEW_NAMES):
            with self.subTest(missing=name),self.assertRaisesRegex(m.AdmissionError,'ZIP exact allowed members mismatch'):
                m.read_zip(packed([(key,value) for key,value in sorted(files.items()) if key!=name]),allowed)
        for unknown in ('variant-0-disassemble-unreviewed_callback.stdout',
                        'variant-2-disassemble-capture_callback.argv.json',
                        'variant-5-disassemble-frd_h264_dc_corrupt_fp.stdout',
                        'variant-0-disassemble-frd_h264_dc_corrupt_fp.extra.json'):
            with self.subTest(unknown=unknown),self.assertRaisesRegex(m.AdmissionError,'ZIP exact allowed members mismatch'):
                m.read_zip(packed(sorted(files.items())+[(unknown,b'not authorized')]),allowed)

    def test_new_diagnostic_raw_bytes_and_argv_have_no_acceptance_waiver(self):
        label='variant-3-disassemble-frd_h264_dc_corrupt_sp8'
        argv=['/opt/frd-llvm-mingw-20250709/bin/llvm-objdump','--disassemble','--reloc',
              '--start-address=0x140001000','--stop-address=0x140001008','/producer/abi-contract-3.exe']
        files={label+'.stdout':b'actual bounded disassembly bytes',label+'.stderr':b'',label+'.argv.json':encoded(argv)}
        receipt={'argv':argv,'cwd':'/producer','environment':{'PATH':'/opt/frd-llvm-mingw-20250709/bin:/usr/bin:/bin','LC_ALL':'C','LANG':'C','HOME':'/tmp'},
                 'pid':23,'elapsed_ns':123,'wait_terminal':'exited','returncode':0,'failure':None}
        receipt.update({stream:{'name':label+'.'+stream,'sha256':hashlib.sha256(files[label+'.'+stream]).hexdigest()} for stream in ('stdout','stderr')})
        files[label+'.receipt.json']=encoded(receipt)
        manifest={'schema':'control.members','files':{name:hashlib.sha256(raw).hexdigest() for name,raw in files.items()}}
        def envelope(content):
            raw=packed(sorted(content.items())+[('members.json',encoded(manifest))])
            archive={'schema':'control.archive','sha256':hashlib.sha256(raw).hexdigest(),'members_sha256':hashlib.sha256(encoded(manifest)).hexdigest(),'strict_members':sorted(files)+['members.json']}
            return raw,encoded(archive)
        raw,archive=envelope(files)
        admitted=m.bundle(raw,archive,set(files),'control')
        m.record(admitted,label,argv,'/producer')
        changed={**files,label+'.stdout':b'altered diagnostic bytes'}
        raw,archive=envelope(changed)  # outer digest refreshed; independent member hash remains fixed.
        with self.assertRaisesRegex(m.AdmissionError,'member full bytes hash'):
            m.bundle(raw,archive,set(files),'control')
        changed_receipt={**receipt,'argv':argv+['--unreviewed']}
        with self.assertRaisesRegex(m.AdmissionError,'stored compile command/cwd mismatch'):
            m.record({**files,label+'.receipt.json':encoded(changed_receipt)},label,argv,'/producer')

class PinnedSourceCacheControls(unittest.TestCase):
    """完整小 checkout 的真实 pyc 读取控制；不 mock loader 或源码期望。"""
    def cache_control(self, relative):
        import builtins
        import marshal
        import struct
        from unittest import mock
        fixture = 'third_party/ffmpeg/8.1.2/tests/'
        names = set(m.CHECKOUT_PINS) | {fixture + name for name in (
            'h264-luma-dc-platform-windows.c', 'h264-luma-dc-clock-convert.h',
            'h264-luma-dc-platform-windows-tests.c', 'README-windows-platform.md',
            'h264-luma-dc-clock-tests.c', 'h264-luma-dc-clock-oracle.py',
            'h264-luma-dc-windows-clock-record.h', 'h264-luma-dc-platform.h',
            'h264-luma-dc-abi-windows.S', 'h264-luma-dc-abi-windows-negative.S',
            'README-windows-abi.md', 'h264-luma-dc-windows-contract.c',
            'README-windows-contract.md', 'h264-luma-dc-windows-diagnostics.h')}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            expected = {}
            for name in sorted(names):
                path = root / name; path.parent.mkdir(parents=True, exist_ok=True)
                raw = (MODULE_PATH.parents[1] / name).read_bytes(); path.write_bytes(raw)
                expected[name] = {'path': str(path), 'sha256': hashlib.sha256(raw).hexdigest()}
            target = root / relative
            raw = target.read_bytes()
            # 合法 timestamp/size header，缓存代码先记内存 marker 再运行完整原源码。
            poisoned = b"import builtins\nbuiltins._frd_admission_cache_probe.append('unhashed-cache-executed')\n" + raw
            code = compile(poisoned, str(target), 'exec')
            header = struct.pack('<4sIII', importlib.util.MAGIC_NUMBER, 0,
                                 int(target.stat().st_mtime) & 0xffffffff, len(raw))
            cache = Path(importlib.util.cache_from_source(str(target)))
            cache.parent.mkdir()
            cache.write_bytes(header + marshal.dumps(code))
            before_cache = cache.read_bytes()
            with mock.patch.object(builtins, '_frd_admission_cache_probe', [], create=True):
                compiler, support, snapshot = m.reviewed(root)
                self.assertEqual(snapshot, expected)  # 正确源码真实 inspection 已走完。
                self.assertTrue(callable(compiler.pe_admit))
                self.assertTrue(callable(support.coff_admit))
                self.assertEqual(builtins._frd_admission_cache_probe, [], relative)
            self.assertEqual(cache.read_bytes(), before_cache)  # 不删除/重写缓存。
            for name, item in expected.items():
                self.assertEqual(hashlib.sha256((root / name).read_bytes()).hexdigest(), item['sha256'])

    def test_compiler_timestamp_cache_cannot_replace_pinned_source(self):
        self.cache_control('tools/compile-windows-contract.py')

    def test_nested_support_timestamp_cache_cannot_replace_pinned_source(self):
        self.cache_control('tools/compile-windows-private-abi.py')


class OptionalRealStaticFixture(unittest.TestCase):
    @unittest.skipUnless(__import__('os').environ.get('FRD_ADMISSION_STATIC_FIXTURE'), 'explicit old compiled-only fixture optional; CI uses independent small controls')
    def test_old_inner_bytes_in_host_control_envelope(self):
        import os
        old = Path(os.environ['FRD_ADMISSION_STATIC_FIXTURE'])
        output = Path(os.environ['FRD_ADMISSION_STATIC_OUTPUT'])
        output.mkdir()  # fresh only; no retries or cleanup of failed admissions.
        self.assertEqual(m.identity(old)['sha256'], 'ddf55493a5be7f5832fe368bdf190a6fc294428a53ac641ba87aed1d6a9c8d7f')
        prefixes = ('task4c5c3b2c-install-38548996801-1', 'task4c5c3b2c-compile-38548996801-1', 'task4c5c3b2e-contract-38548996801-1')
        outer = output / 'host-control-envelope.zip'
        with zipfile.ZipFile(old) as source, zipfile.ZipFile(outer, 'x', zipfile.ZIP_STORED) as target:
            for index, prefix in enumerate(prefixes):
                for name in ('evidence.zip', 'archive.json') + (('terminal-status.json',) if index == 2 else ()):
                    target.writestr(prefix + '/' + name, source.read(prefix + '/' + name))
        original = m.identity(outer)
        context = {'repository': m.REPOSITORY, 'repository_id': m.REPOSITORY_ID, 'source_sha': '30189bf67ee2e9743045275969cdd039a8cdba50', 'run_id': 38548996801, 'run_attempt': 1, 'compile_job_result': 'success', 'artifact': {'id': 1, 'name': 'task4c5c3b2f2-input-38548996801-1', 'size_in_bytes': original['size'], 'digest': 'sha256:' + original['sha256']}}
        metadata = {**context['artifact'], 'expired': False, 'workflow_run': {'id': 38548996801, 'repository_id': m.REPOSITORY_ID, 'head_repository_id': m.REPOSITORY_ID, 'head_sha': context['source_sha']}}
        (output / 'HOST-CONTROL-ONLY.json').write_bytes(encoded({'scope': 'Old compiled-only inner bytes; new envelope/context/id1/metadata are fabricated host controls, not same-run API observation or Windows execution.', 'original_fixture': str(old), 'original_fixture_sha256': m.identity(old)['sha256']}))
        seal = m.admit_artifact(outer, MODULE_PATH.parents[1], context, metadata, output / 'admitted-static')
        self.assertEqual(len(seal['pe_paths']), 5)
        self.assertTrue(m.revalidate(seal))
        pe = Path(seal['pe_paths'][0]); original_pe = pe.read_bytes()
        pe.write_bytes(original_pe + b'after-admission-control')
        with self.assertRaises(m.AdmissionError):
            m.revalidate(seal)
        pe.write_bytes(original_pe)
        self.assertTrue(m.revalidate(seal))
        # 失败目录/首错保存控制：只注入静态检查失败，不运行 PE/外部命令。
        from unittest import mock
        with mock.patch.object(m, 'static_files', side_effect=m.AdmissionError('host-control-first-static-failure')):
            with self.assertRaisesRegex(m.AdmissionError, 'host-control-first-static-failure'):
                m.admit_artifact(outer, MODULE_PATH.parents[1], context, metadata, output / 'partial-first-failure')
        self.assertFalse((output / 'partial-first-failure' / m.SEAL_FILE).exists())
        failure = m.strict_json((output / 'partial-first-failure' / 'admission-failure.json').read_bytes())
        self.assertEqual(failure['first_failure']['message'], 'host-control-first-static-failure')
        self.assertTrue((output / 'partial-first-failure' / 'transport' / prefixes[0] / 'evidence.zip').is_file())
        for key, value in (('digest', 'sha256:' + '0'*64), ('size_in_bytes', original['size']+1)):
            changed = {**context, 'artifact': {**context['artifact'], key: value}}
            observed = {**metadata, key: value}
            with self.assertRaises(m.AdmissionError):
                m.admit_artifact(outer, MODULE_PATH.parents[1], changed, observed, output / ('bad-' + key))
            self.assertFalse((output / ('bad-' + key)).exists())


if __name__ == '__main__':
    unittest.main()
