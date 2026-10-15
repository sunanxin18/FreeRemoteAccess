"""Host-only 生命周期控制；无 Win32/PE 执行或 native 收据。"""
import ctypes as C
import hashlib
import importlib.util
import struct
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch

PATH = Path(__file__).resolve().parents[1] / 'windows-private-process.py'
m = None
if PATH.exists():
    spec = importlib.util.spec_from_file_location('owner_controls', PATH)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)


class FakeAPI:
    def __init__(self, fail=None, *, waiting=0, active=0, stream=b'abc'):
        self.fail, self.waiting, self.jobs = fail, waiting, active
        self.stream = stream
        self.log, self.handles, self.process = [], [], None
        self.tick = 0
    def action(self, name):
        self.log.append(name)
        if self.fail == name:
            raise OSError(name)
    def counter(self):
        self.tick += 1
        return self.tick
    def frequency(self): return 1000
    def now(self):
        self.tick += 1
        return self.tick * 1.0
    def pause(self): pass
    def create(self, exe, argv, cwd, env):
        self.handles.append('job')
        self.action('job')
        self.handles.extend(['readout', 'readerr'])
        self.action('pipes')
        self.action('attributes')
        self.action('create')
        self.handles.extend(['process', 'thread'])
        self.process = 'process'
        return {'pid': 42, 'creation_flags': 0x80404, 'job_at_creation': True}
    def machine(self, handle):
        self.action('machine')
        return {'accepted': self.fail != 'mismatch', 'process_machine': 0, 'native_machine': 0xaa64, 'machine_type': 0xaa64}
    def resume(self): self.action('resume')
    def read(self, stream, remaining):
        self.action('read-' + stream)
        if stream == 'stdout':
            result, self.stream = self.stream, b''
            return result
        return b''
    def wait(self):
        self.action('wait')
        return self.waiting
    def exit_code(self):
        self.action('exit')
        return 7 if self.fail == 'nonzero' else 0
    def active(self):
        self.action('active')
        return self.jobs
    def terminate(self):
        self.action('terminate')
        if self.fail != 'cleanup': self.waiting, self.jobs = 0, 0
    def close_all(self):
        self.log.extend('close-' + n for n in reversed(self.handles))
        self.handles.clear()


class BoundedPostExitAPI(FakeAPI):
    """遵守 Win32 read 的 remaining+1/4096 上限，专门经过 post-exit drain。"""
    def __init__(self, failure):
        super().__init__(stream=b'')
        self.failure, self.stdout_reads, self.stderr_reads, self.waits = failure, 0, 0, 0
        self.read_observations = []
        self.pool = b'x' * (1024 * 1024) + b'!'
    def now(self):
        self.tick += 1
        return self.tick / 1000
    def read(self, stream, remaining):
        self.action('read-' + stream)
        if stream == 'stdout':
            self.stdout_reads += 1
            if self.failure == 'overflow':
                length = min(1 if self.stdout_reads == 1 else 4096, remaining + 1, len(self.pool))
                value, self.pool = self.pool[:length], self.pool[length:]
            else:
                value = b'initial' if self.stdout_reads == 1 else b'drain-prefix' if self.stdout_reads == 2 else b''
        else:
            self.stderr_reads += 1
            if self.failure == 'stderr' and self.stderr_reads == 2: raise OSError('host-only stderr read error')
            value = b''
        assert len(value) <= min(4096, remaining + 1)
        self.read_observations.append({'stream': stream, 'remaining': remaining, 'size': len(value)})
        return value
    def wait(self):
        self.action('wait'); self.waits += 1
        return 258 if self.failure == 'overflow' and self.waits == 1 else 0


class OwnerControls(unittest.TestCase):
    def setUp(self): self.assertIsNotNone(m, 'missing process owner implementation')
    def run_case(self, api, timeout=30):
        return m.collect_owned(api, 'fixed.exe', ['fixed.exe', '--suite', 'register', '--expect-variant', '0'], 'cwd', {'SystemRoot': 'C:\\Windows'}, timeout)
    def test_fixed_width_layout(self):
        self.assertEqual([C.sizeof(x) for x in (m.DWORD, m.BOOL, m.WORD, m.HANDLE)], [4, 4, 2, 8])
        self.assertEqual(C.sizeof(m.STARTUPINFOW), 104)
        self.assertEqual(C.sizeof(m.STARTUPINFOEXW), 112)
        self.assertEqual(C.sizeof(m.JOB_EXTENDED), 144)
        self.assertEqual(m.JOB_ACCOUNTING.ActiveProcesses.offset, 40)
        self.assertEqual(m.PROCESS_MACHINE.MachineAttributes.offset, 4)
    def test_exit_wait_and_empty_job_required(self):
        api = FakeAPI()
        result = self.run_case(api)
        self.assertIsNone(result['first_failure'])
        self.assertTrue(result['cleanup_confirmed'])
        self.assertEqual(result['stdout'], b'abc')
        self.assertEqual(result['exit_code'], 0)
        self.assertLess(api.log.index('machine'), api.log.index('resume'))
        self.assertFalse(api.handles)
    def test_each_failure_closes_partial_handles_and_never_resumes_early(self):
        for failure in ('job', 'pipes', 'attributes', 'create', 'machine', 'mismatch', 'resume', 'read-stdout', 'wait', 'active', 'exit', 'nonzero'):
            with self.subTest(failure=failure):
                api = FakeAPI(failure)
                result = self.run_case(api)
                self.assertIsNotNone(result['first_failure'])
                self.assertFalse(api.handles)
                self.assertEqual(len([v for v in api.log if v.startswith('close-')]), len(set(v for v in api.log if v.startswith('close-'))))
                if failure in ('job', 'pipes', 'attributes', 'create', 'machine', 'mismatch'):
                    self.assertNotIn('resume', api.log)
    def test_overflow_is_failure_and_bounded(self):
        api = FakeAPI(stream=b'x' * (1024 * 1024 + 1))
        result = self.run_case(api)
        self.assertEqual(result['first_failure']['stage'], 'stream-overflow')
        self.assertLessEqual(len(result['stdout']), 1024 * 1024)
        self.assertIn('terminate', api.log)
    def test_post_exit_overflow_preserves_every_byte_inside_cap(self):
        api = BoundedPostExitAPI('overflow')
        result = self.run_case(api)
        self.assertEqual(result['first_failure'], {'stage': 'stream-overflow', 'type': 'ProcessError', 'code': 'stream-overflow'})
        self.assertEqual(len(result['stdout']), 1024 * 1024)
        self.assertEqual(result['stdout'], b'x' * (1024 * 1024))
        self.assertEqual(hashlib.sha256(result['stdout']).hexdigest(), hashlib.sha256(b'x' * (1024 * 1024)).hexdigest())
        self.assertEqual(api.read_observations[-1], {'stream': 'stdout', 'remaining': 4095, 'size': 4096})
        self.assertTrue(result['cleanup_confirmed'])
        self.assertIn('terminate', api.log)
        self.assertFalse(api.handles)

    def test_post_exit_stderr_failure_keeps_already_read_stdout_block(self):
        api = BoundedPostExitAPI('stderr')
        result = self.run_case(api)
        self.assertEqual(result['stdout'], b'initialdrain-prefix')
        self.assertEqual(hashlib.sha256(result['stdout']).hexdigest(), hashlib.sha256(b'initialdrain-prefix').hexdigest())
        self.assertEqual(result['first_failure'], {'stage': 'read-stderr', 'type': 'OSError'})
        self.assertTrue(result['cleanup_confirmed'])
        self.assertIn('terminate', api.log)
        self.assertFalse(api.handles)

    def test_timeout_is_not_terminal(self):
        api = FakeAPI(waiting=258, active=1)
        result = self.run_case(api, timeout=30)
        self.assertEqual(result['first_failure']['stage'], 'timeout')
        self.assertTrue(result['cleanup_confirmed'])
        self.assertIn('terminate', api.log)
    def test_job_active_after_parent_exit_is_terminated_and_rejected(self):
        api = FakeAPI(active=1)
        result = self.run_case(api)
        self.assertIsNotNone(result['first_failure'])
        self.assertTrue(result['cleanup_confirmed'])
    def test_cleanup_failure_does_not_replace_first_failure(self):
        for failure in ('terminate', 'cleanup'):
            api = FakeAPI(failure, waiting=258, active=1)
            result = self.run_case(api, timeout=30)
            self.assertEqual(result['first_failure']['stage'], 'timeout')
            self.assertFalse(result['cleanup_confirmed'])
    def test_only_fixed_production_timeouts_accepted(self):
        with self.assertRaises(m.ProcessError): self.run_case(FakeAPI(), timeout=3)

    def test_launcher_raw_pe_rejects_ec_clr_chpe_and_wrong_magic(self):
        raw = bytearray(2048)
        raw[:2] = b'MZ'; struct.pack_into('<I', raw, 0x3c, 128)
        raw[128:132] = b'PE\0\0'
        struct.pack_into('<HH', raw, 132, 0xaa64, 1)
        struct.pack_into('<H', raw, 148, 240)
        struct.pack_into('<H', raw, 152, 0x20b)
        struct.pack_into('<I', raw, 260, 16)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'python.exe'; path.write_bytes(raw)
            self.assertEqual(m.python_pe(path)['machine'], 0xaa64)
            for offset, form, value in ((132, '<H', 0xa641), (132, '<H', 0x8664), (152, '<H', 0x10b), (376, '<I', 1)):
                changed = bytearray(raw); struct.pack_into(form, changed, offset, value); path.write_bytes(changed)
                with self.assertRaises(m.ProcessError): m.python_pe(path)
            config = bytearray(raw)
            struct.pack_into('<II', config, 344, 0x1000, 208)
            struct.pack_into('<III', config, 404, 0x1000, 512, 512)
            struct.pack_into('<I', config, 512, 208)
            struct.pack_into('<Q', config, 712, 123)
            path.write_bytes(config)
            with self.assertRaises(m.ProcessError): m.python_pe(path)

    def test_native_entry_unavailable_on_host(self):
        import os
        if os.name != 'nt':
            with self.assertRaises(m.ProcessError): m.Win32()


class KernelDouble:
    """ctypes 边界替身，只在 host 检查参数/失败清理，不执行 Win32。"""
    def __init__(self, fail=None):
        self.fail, self.error, self.next = fail, 0, 100
        self.calls, self.closed, self.attributes = [], [], {}
        self.initializations = 0
        self.created = None
    def handle(self): self.next += 1; return self.next
    def __getattr__(self, name):
        def api(*args):
            self.calls.append(name)
            if name == 'InitializeProcThreadAttributeList':
                self.initializations += 1
                if args[0] is None:
                    args[3]._obj.value = 256; self.error = 122; return 0
            if self.fail == name:
                self.error = 5; return 0
            if name == 'CreateJobObjectW':
                self.job = self.handle(); return self.job
            if name == 'CreatePipe':
                args[0]._obj.value, args[1]._obj.value = self.handle(), self.handle()
            if name == 'CreateFileW': return self.handle()
            if name == 'SetInformationJobObject':
                self.limits = args[2]._obj.BasicLimitInformation.LimitFlags
            if name == 'UpdateProcThreadAttribute':
                self.attributes[args[2]] = list(args[3])
            if name == 'CreateProcessW':
                self.created = {'application': args[0], 'command': args[1].value, 'inherit': args[4],
                                'flags': args[5], 'environment': args[6][:], 'cwd': args[7],
                                'startup': args[8]._obj.StartupInfo}
                info = args[9]._obj
                info.hProcess, info.hThread, info.dwProcessId, info.dwThreadId = self.handle(), self.handle(), 42, 43
            if name == 'CloseHandle': self.closed.append(args[0])
            if name in ('DeleteProcThreadAttributeList',): return None
            return 1
        return api


class AttributeBoundaryControls(unittest.TestCase):
    def setUp(self): self.assertIsNotNone(m, 'missing process owner implementation')
    def backend(self, kernel):
        backend = m.Win32.__new__(m.Win32)
        backend.k, backend.calls, backend.handles, backend.process = kernel, [], [], None
        backend.stdout_read = backend.stderr_read = backend.thread = backend.job = None
        return backend
    def execute_create(self, kernel):
        backend = self.backend(kernel)
        with patch.object(C, 'set_last_error', lambda value: setattr(kernel, 'error', value), create=True), patch.object(C, 'get_last_error', lambda: kernel.error, create=True):
            try:
                result = backend.create(r'C:\\fixed path\\fixed.exe', [r'C:\\fixed path\\fixed.exe', '--suite', 'register', '--expect-variant', '0'], 'cwd', {'SystemRoot': r'C:\\Windows'})
            finally:
                backend.close_all()
        return backend, result
    def test_atomic_job_and_explicit_only_inherited_handles(self):
        kernel = KernelDouble(); backend, result = self.execute_create(kernel)
        self.assertEqual(kernel.attributes[0x2000d], [kernel.job])
        inherited = kernel.attributes[0x20002]
        self.assertEqual(len(inherited), 3)
        self.assertNotIn(kernel.job, inherited)
        self.assertEqual(kernel.limits, 0x2000)
        self.assertTrue(kernel.created['flags'] & 4)
        self.assertTrue(kernel.created['flags'] & 0x80000)
        self.assertFalse(kernel.created['flags'] & 0x1000000)
        self.assertEqual(kernel.created['inherit'], 1)
        self.assertLess(kernel.calls.index('UpdateProcThreadAttribute'), kernel.calls.index('CreateProcessW'))
        self.assertLess(kernel.calls.index('CreateProcessW'), kernel.calls.index('DeleteProcThreadAttributeList'))
        self.assertFalse(backend.handles)
        self.assertEqual(len(kernel.closed), len(set(kernel.closed)))
        self.assertEqual(kernel.created['startup'].hStdOutput, inherited[1])
        self.assertEqual(kernel.created['startup'].hStdError, inherited[2])
    def test_partial_initialization_closes_handles_and_deletes_initialized_attributes(self):
        for failing in ('SetInformationJobObject', 'CreatePipe', 'SetHandleInformation', 'UpdateProcThreadAttribute', 'CreateProcessW'):
            with self.subTest(failing=failing):
                kernel = KernelDouble(failing); backend = self.backend(kernel)
                with patch.object(C, 'set_last_error', lambda value: setattr(kernel, 'error', value), create=True), patch.object(C, 'get_last_error', lambda: kernel.error, create=True):
                    with self.assertRaises(m.ProcessError): backend.create('fixed.exe', ['fixed.exe'], 'cwd', {})
                    backend.close_all()
                self.assertFalse(backend.handles)
                self.assertEqual(len(kernel.closed), len(set(kernel.closed)))
                if failing in ('UpdateProcThreadAttribute', 'CreateProcessW'):
                    self.assertIn('DeleteProcThreadAttributeList', kernel.calls)
                else:
                    self.assertNotIn('DeleteProcThreadAttributeList', kernel.calls)
                self.assertTrue(any(call['error'] == 5 and not call['valid'] for call in backend.calls))
    def test_attribute_sizing_preserves_expected_false_and_error122(self):
        kernel = KernelDouble(); backend, result = self.execute_create(kernel)
        sizing = [call for call in backend.calls if call['api'] == 'InitializeProcThreadAttributeList'][0]
        self.assertEqual(sizing['return'], 0)
        self.assertEqual(sizing['last_error'], 122)
        self.assertTrue(sizing['error_valid'])
        self.assertEqual(sizing['error'], 122)
    def test_null_handle_is_failure_and_not_a_void_api_return(self):
        kernel = KernelDouble()
        def null_job(*args): kernel.error = 5; return None
        kernel.CreateJobObjectW = null_job
        backend = self.backend(kernel)
        with patch.object(C, 'set_last_error', lambda value: setattr(kernel, 'error', value), create=True), patch.object(C, 'get_last_error', lambda: kernel.error, create=True):
            with self.assertRaises(m.ProcessError): backend.call('CreateJobObjectW', None, None)
        self.assertFalse(backend.calls[-1]['valid'])
        self.assertTrue(backend.calls[-1]['error_valid'])
        self.assertEqual(backend.calls[-1]['error'], 5)

    def test_close_failure_never_reuses_or_recloses_handle(self):
        kernel = KernelDouble('CloseHandle'); backend = self.backend(kernel); backend.own(100)
        with patch.object(C, 'set_last_error', lambda value: setattr(kernel, 'error', value), create=True), patch.object(C, 'get_last_error', lambda: kernel.error, create=True):
            with self.assertRaises(m.ProcessError): backend.close_all()
            backend.close_all()
        self.assertEqual(kernel.calls.count('CloseHandle'), 1)
        self.assertFalse(backend.handles)

if __name__ == '__main__': unittest.main()
