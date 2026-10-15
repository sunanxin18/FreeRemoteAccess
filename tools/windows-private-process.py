#!/usr/bin/env python3
"""Windows ARM64 私有进程 owner；host 模型只检查转换，不授予 native 资格。

公开 JOB_LIST 在 CreateProcessW 内原子关联 kill-on-close job；无 Assign/breakaway
后备路径。所有 Win32 原始返回值保存在 api_calls，失败输出不包含 argv 以外异常文本。
结构以固定 Windows LLP64 定义，Mac 上仅可检查布局。
"""
import ctypes as C
import os
from pathlib import Path
import struct
import subprocess
import sys
import time

DWORD = C.c_uint32
BOOL = C.c_int32
WORD = C.c_uint16
HANDLE = C.c_void_p
SIZE_T = C.c_uint64
LARGE_INTEGER = C.c_int64
MAX_STREAM = 1024 * 1024
WAIT_OBJECT_0, WAIT_TIMEOUT, WAIT_FAILED = 0, 258, 0xffffffff
AA64 = 0xaa64


class SECURITY_ATTRIBUTES(C.Structure):
    _fields_ = [('nLength', DWORD), ('lpSecurityDescriptor', HANDLE), ('bInheritHandle', BOOL)]


class STARTUPINFOW(C.Structure):
    _fields_ = [('cb', DWORD), ('lpReserved', HANDLE), ('lpDesktop', HANDLE), ('lpTitle', HANDLE),
                ('dwX', DWORD), ('dwY', DWORD), ('dwXSize', DWORD), ('dwYSize', DWORD),
                ('dwXCountChars', DWORD), ('dwYCountChars', DWORD), ('dwFillAttribute', DWORD),
                ('dwFlags', DWORD), ('wShowWindow', WORD), ('cbReserved2', WORD),
                ('lpReserved2', HANDLE), ('hStdInput', HANDLE), ('hStdOutput', HANDLE), ('hStdError', HANDLE)]


class STARTUPINFOEXW(C.Structure):
    _fields_ = [('StartupInfo', STARTUPINFOW), ('lpAttributeList', HANDLE)]


class PROCESS_INFORMATION(C.Structure):
    _fields_ = [('hProcess', HANDLE), ('hThread', HANDLE), ('dwProcessId', DWORD), ('dwThreadId', DWORD)]


class JOB_LIMIT(C.Structure):
    _fields_ = [('PerProcessUserTimeLimit', LARGE_INTEGER), ('PerJobUserTimeLimit', LARGE_INTEGER),
                ('LimitFlags', DWORD), ('MinimumWorkingSetSize', SIZE_T), ('MaximumWorkingSetSize', SIZE_T),
                ('ActiveProcessLimit', DWORD), ('Affinity', SIZE_T), ('PriorityClass', DWORD), ('SchedulingClass', DWORD)]


class IO_COUNTERS(C.Structure):
    _fields_ = [(name, C.c_uint64) for name in ('ReadOperationCount', 'WriteOperationCount', 'OtherOperationCount', 'ReadTransferCount', 'WriteTransferCount', 'OtherTransferCount')]


class JOB_EXTENDED(C.Structure):
    _fields_ = [('BasicLimitInformation', JOB_LIMIT), ('IoInfo', IO_COUNTERS),
                ('ProcessMemoryLimit', SIZE_T), ('JobMemoryLimit', SIZE_T), ('PeakProcessMemoryUsed', SIZE_T), ('PeakJobMemoryUsed', SIZE_T)]


class JOB_ACCOUNTING(C.Structure):
    _fields_ = [(n, LARGE_INTEGER) for n in ('TotalUserTime', 'TotalKernelTime', 'ThisPeriodTotalUserTime', 'ThisPeriodTotalKernelTime')] + [(n, DWORD) for n in ('TotalPageFaultCount', 'TotalProcesses', 'ActiveProcesses', 'TotalTerminatedProcesses')]


class PROCESS_MACHINE(C.Structure):
    _fields_ = [('ProcessMachine', WORD), ('Res0', WORD), ('MachineAttributes', DWORD)]


class SYSTEM_INFO(C.Structure):
    _fields_ = [('wProcessorArchitecture', WORD), ('wReserved', WORD), ('dwPageSize', DWORD),
                ('lpMinimumApplicationAddress', HANDLE), ('lpMaximumApplicationAddress', HANDLE),
                ('dwActiveProcessorMask', SIZE_T), ('dwNumberOfProcessors', DWORD),
                ('dwProcessorType', DWORD), ('dwAllocationGranularity', DWORD),
                ('wProcessorLevel', WORD), ('wProcessorRevision', WORD)]


assert C.sizeof(DWORD) == 4 and C.sizeof(BOOL) == 4 and C.sizeof(WORD) == 2
assert C.sizeof(HANDLE) == 8, '仅支持 64 位 Python 的静态布局'
assert C.sizeof(STARTUPINFOW) == 104 and STARTUPINFOW.hStdInput.offset == 80
assert C.sizeof(STARTUPINFOEXW) == 112 and STARTUPINFOEXW.lpAttributeList.offset == 104
assert C.sizeof(PROCESS_INFORMATION) == 24 and C.sizeof(JOB_EXTENDED) == 144
assert C.sizeof(JOB_ACCOUNTING) == 48 and JOB_ACCOUNTING.ActiveProcesses.offset == 40
assert C.sizeof(PROCESS_MACHINE) == 8 and PROCESS_MACHINE.MachineAttributes.offset == 4
assert C.sizeof(SYSTEM_INFO) == 48


class ProcessError(Exception):
    pass


def require(condition, stage):
    if not condition:
        raise ProcessError(stage)


def python_pe(path):
    """launcher 原始 PE 读取；不对系统 Python 应用私有 imports 白名单。"""
    import hashlib
    data = Path(path).read_bytes()
    def u16(o):
        require(0 <= o <= len(data) - 2, 'launcher-pe-bounds')
        return struct.unpack_from('<H', data, o)[0]
    def u32(o):
        require(0 <= o <= len(data) - 4, 'launcher-pe-bounds')
        return struct.unpack_from('<I', data, o)[0]
    def u64(o):
        require(0 <= o <= len(data) - 8, 'launcher-pe-bounds')
        return struct.unpack_from('<Q', data, o)[0]
    require(data[:2] == b'MZ', 'launcher-dos')
    pe = u32(0x3c)
    require(data[pe:pe + 4] == b'PE\0\0' and u16(pe + 4) == AA64, 'launcher-machine')
    opt, nsect, osize = pe + 24, u16(pe + 6), u16(pe + 20)
    require(u16(opt) == 0x20b and osize >= 240 and nsect > 0, 'launcher-pe32plus')
    require(u32(opt + 108) >= 15, 'launcher-directories')
    clr = (u32(opt + 112 + 14 * 8), u32(opt + 116 + 14 * 8))
    require(clr == (0, 0), 'launcher-clr')
    rva, size = u32(opt + 192), u32(opt + 196)
    if rva or size:
        require(rva > 0 and size >= 4, 'launcher-loadconfig')
        offsets = []
        for i in range(nsect):
            sec = opt + osize + i * 40
            va, rawsize, raw = u32(sec + 12), u32(sec + 16), u32(sec + 20)
            if va <= rva and rva + size <= va + rawsize:
                offsets.append(raw + rva - va)
        require(len(offsets) == 1, 'launcher-loadconfig-rva')
        offset = offsets[0]
        declared = u32(offset)
        require(4 <= declared <= size and offset + size <= len(data), 'launcher-loadconfig-size')
        if declared > 200:
            require(declared >= 208 and u64(offset + 200) == 0, 'launcher-chpe')
    return {'path': str(path), 'size': len(data), 'sha256': hashlib.sha256(data).hexdigest(), 'machine': AA64, 'magic': 0x20b, 'clr': False, 'chpe': False}


class Win32:
    """真实 native 后端；只能在 Windows 构造，不接受替代 DLL/backend。"""
    def __init__(self):
        require(os.name == 'nt' and C.sizeof(C.c_void_p) == 8, 'windows-native-unavailable')
        self.k = C.WinDLL('kernel32', use_last_error=True)
        self.calls, self.handles, self.process = [], [], None
        self.machine_queries = []
        self.stdout_read = self.stderr_read = self.thread = self.job = None
        P = HANDLE
        prototypes = {
            'CloseHandle': ([HANDLE], BOOL), 'CreateJobObjectW': ([P, C.c_wchar_p], HANDLE),
            'SetInformationJobObject': ([HANDLE, C.c_int32, P, DWORD], BOOL),
            'QueryInformationJobObject': ([HANDLE, C.c_int32, P, DWORD, P], BOOL),
            'CreatePipe': ([P, P, P, DWORD], BOOL), 'SetHandleInformation': ([HANDLE, DWORD, DWORD], BOOL),
            'CreateFileW': ([C.c_wchar_p, DWORD, DWORD, P, DWORD, DWORD, HANDLE], HANDLE),
            'InitializeProcThreadAttributeList': ([P, DWORD, DWORD, P], BOOL),
            'UpdateProcThreadAttribute': ([P, DWORD, SIZE_T, P, SIZE_T, P, P], BOOL),
            'DeleteProcThreadAttributeList': ([P], None),
            'CreateProcessW': ([C.c_wchar_p, C.c_wchar_p, P, P, BOOL, DWORD, P, C.c_wchar_p, P, P], BOOL),
            'IsWow64Process2': ([HANDLE, P, P], BOOL), 'GetProcessInformation': ([HANDLE, C.c_int32, P, DWORD], BOOL),
            'GetNativeSystemInfo': ([P], None), 'GetCurrentProcess': ([], HANDLE),
            'ResumeThread': ([HANDLE], DWORD), 'WaitForSingleObject': ([HANDLE, DWORD], DWORD),
            'GetExitCodeProcess': ([HANDLE, P], BOOL), 'TerminateJobObject': ([HANDLE, DWORD], BOOL),
            'PeekNamedPipe': ([HANDLE, P, DWORD, P, P, P], BOOL), 'ReadFile': ([HANDLE, P, DWORD, P, P], BOOL),
            'QueryPerformanceCounter': ([P], BOOL), 'QueryPerformanceFrequency': ([P], BOOL),
        }
        for name, (args, result) in prototypes.items():
            function = getattr(self.k, name)
            function.argtypes, function.restype = args, result

    def call(self, name, *args, valid=None):
        C.set_last_error(0)
        result = getattr(self.k, name)(*args)
        error = C.get_last_error()
        # VOID APIs have no return/error success contract; record as void, not BOOL.
        is_void = name in ('DeleteProcThreadAttributeList', 'GetNativeSystemInfo')
        ok = True if is_void else bool(result) if valid is None else valid(result)
        if is_void or name == 'GetCurrentProcess':
            error_valid = False
        elif name in ('WaitForSingleObject', 'ResumeThread'):
            error_valid = result == 0xffffffff
        elif name == 'CreateFileW':
            error_valid = result in (None, 0, C.c_void_p(-1).value)
        else:
            error_valid = not bool(result)
        self.calls.append({'api': name, 'attempted': True, 'return': result, 'valid': bool(ok),
                           'error_valid': error_valid, 'error': error if error_valid else None, 'last_error': error})
        require(ok, name)
        return result

    def own(self, handle):
        require(handle not in (None, 0, C.c_void_p(-1).value), 'invalid-owned-handle')
        self.handles.append(handle)
        return handle

    def close(self, handle):
        if handle in self.handles:
            self.handles.remove(handle)  # 即使 CloseHandle 失败也不重用/重复关闭该值。
            self.call('CloseHandle', handle)

    def close_all(self):
        failed = False
        for handle in list(reversed(self.handles)):
            try: self.close(handle)
            except ProcessError: failed = True
        require(not failed, 'close-handles')

    def create(self, exe, argv, cwd, env):
        self.job = self.own(self.call('CreateJobObjectW', None, None))
        limits = JOB_EXTENDED()
        limits.BasicLimitInformation.LimitFlags = 0x2000
        self.call('SetInformationJobObject', self.job, 9, C.byref(limits), C.sizeof(limits))
        sa = SECURITY_ATTRIBUTES(C.sizeof(SECURITY_ATTRIBUTES), None, 1)
        writes = []
        for attr in ('stdout_read', 'stderr_read'):
            read, write = HANDLE(), HANDLE()
            self.call('CreatePipe', C.byref(read), C.byref(write), C.byref(sa), 0)
            setattr(self, attr, self.own(read.value))
            writes.append(self.own(write.value))
            self.call('SetHandleInformation', read, 1, 0)
        stdin = self.own(self.call('CreateFileW', 'NUL', 0x80000000, 3, C.byref(sa), 3, 0, None,
                                  valid=lambda v: v not in (None, 0, C.c_void_p(-1).value)))
        size = SIZE_T()
        # 第一次探测必须是 FALSE/ERROR_INSUFFICIENT_BUFFER；保留原始返回。
        self.call('InitializeProcThreadAttributeList', None, 2, 0, C.byref(size),
                  valid=lambda value: value == 0 and C.get_last_error() == 122 and 0 < size.value <= 65536)
        storage = C.create_string_buffer(size.value)
        initialized = False
        try:
            self.call('InitializeProcThreadAttributeList', storage, 2, 0, C.byref(size))
            initialized = True
            job_list = (HANDLE * 1)(self.job)
            handle_list = (HANDLE * 3)(stdin, *writes)
            self.call('UpdateProcThreadAttribute', storage, 0, 0x2000d, job_list, C.sizeof(job_list), None, None)
            self.call('UpdateProcThreadAttribute', storage, 0, 0x20002, handle_list, C.sizeof(handle_list), None, None)
            startup, info = STARTUPINFOEXW(), PROCESS_INFORMATION()
            startup.StartupInfo.cb = C.sizeof(startup)
            startup.StartupInfo.dwFlags = 0x100
            startup.StartupInfo.hStdInput = stdin
            startup.StartupInfo.hStdOutput, startup.StartupInfo.hStdError = writes
            startup.lpAttributeList = C.addressof(storage)
            command = C.create_unicode_buffer(subprocess.list2cmdline(argv))
            environment = C.create_unicode_buffer('\0'.join(k + '=' + v for k, v in sorted(env.items(), key=lambda item: item[0].upper())) + '\0\0')
            flags = 0x4 | 0x400 | 0x80000 | 0x8000000
            self.call('CreateProcessW', exe, command, None, None, 1, flags, environment, cwd, C.byref(startup), C.byref(info))
            self.process = self.own(info.hProcess)
            self.thread = self.own(info.hThread)
            result = {'pid': info.dwProcessId, 'tid': info.dwThreadId, 'creation_flags': flags, 'job_at_creation': True,
                      'handle_list_count': 3, 'job_inherited': False}
        finally:
            if initialized:
                self.call('DeleteProcThreadAttributeList', storage)
        for handle in (stdin, *writes): self.close(handle)
        return result

    def machine(self, process):
        wow, native, machine = WORD(), WORD(), PROCESS_MACHINE()
        observed = {'process_handle': process, 'wow_valid': False, 'machine_info_valid': False,
                    'process_machine': None, 'native_machine': None, 'machine_type': None, 'machine_attributes': None}
        self.machine_queries.append(observed)
        self.call('IsWow64Process2', process, C.byref(wow), C.byref(native))
        observed.update(wow_valid=True, process_machine=wow.value, native_machine=native.value)
        self.call('GetProcessInformation', process, 9, C.byref(machine), C.sizeof(machine))
        observed.update(machine_info_valid=True, machine_type=machine.ProcessMachine, machine_attributes=machine.MachineAttributes)
        return {'accepted': wow.value == 0 and native.value == AA64 and machine.ProcessMachine == AA64,
                'process_machine': wow.value, 'native_machine': native.value,
                'machine_type': machine.ProcessMachine, 'machine_attributes': machine.MachineAttributes, 'reserved': machine.Res0}

    def admission(self):
        pe = python_pe(sys.executable)
        current = self.call('GetCurrentProcess')
        machine = self.machine(current)
        system = SYSTEM_INFO()
        self.call('GetNativeSystemInfo', C.byref(system))
        version = sys.getwindowsversion()
        result = {'python': sys.version, 'executable': sys.executable, 'pe': pe, 'machine': machine,
                  'native_architecture': system.wProcessorArchitecture, 'os_version': list(version),
                  'runner_image': {k: os.environ.get(k) for k in ('ImageOS', 'ImageVersion', 'RUNNER_OS', 'RUNNER_ARCH')},
                  'api_calls': list(self.calls)}
        self.admission_observation = result
        require(machine['accepted'] and system.wProcessorArchitecture == 12 and version.build >= 22000, 'launcher-native-admission')
        return result

    def resume(self):
        value = self.call('ResumeThread', self.thread, valid=lambda v: v != 0xffffffff)
        require(value == 1, 'unexpected-suspend-count')

    def read(self, stream, remaining):
        handle = self.stdout_read if stream == 'stdout' else self.stderr_read
        available = DWORD()
        before = len(self.calls)
        try:
            self.call('PeekNamedPipe', handle, None, 0, None, C.byref(available), None)
        except ProcessError:
            if self.calls[before]['error'] == 109:
                self.calls[before]['classification'] = 'pipe-eof'
                return b''
            raise
        if not available.value: return b''
        length = min(4096, available.value, remaining + 1)
        buffer, received = C.create_string_buffer(length), DWORD()
        self.call('ReadFile', handle, buffer, length, C.byref(received), None)
        require(0 < received.value <= length, 'read-length')
        return buffer.raw[:received.value]

    def wait(self):
        return self.call('WaitForSingleObject', self.process, 0, valid=lambda v: v in (WAIT_OBJECT_0, WAIT_TIMEOUT))

    def exit_code(self):
        code = DWORD()
        self.call('GetExitCodeProcess', self.process, C.byref(code))
        return code.value

    def active(self):
        accounting, returned = JOB_ACCOUNTING(), DWORD()
        self.call('QueryInformationJobObject', self.job, 1, C.byref(accounting), C.sizeof(accounting), C.byref(returned))
        require(returned.value == C.sizeof(accounting), 'job-accounting-size')
        return accounting.ActiveProcesses

    def terminate(self): self.call('TerminateJobObject', self.job, 0xe0f00301)
    def now(self): return time.monotonic()
    def pause(self): time.sleep(0.01)
    def _clock(self, name):
        value = LARGE_INTEGER()
        self.call(name, C.byref(value))
        return value.value
    def counter(self): return self._clock('QueryPerformanceCounter')
    def frequency(self):
        value = self._clock('QueryPerformanceFrequency')
        require(value > 0, 'qpf-positive')
        return value


def collect_owned(api, exe, argv, cwd, env, timeout):
    """中立生命周期核心。返回观察；单凭此返回值不能创建 native 成功终态。"""
    require(type(timeout) is int and timeout in (30, 120), 'fixed-timeout')
    result = {'argv': argv, 'cwd': cwd, 'environment': env, 'first_failure': None,
              'stdout': b'', 'stderr': b'', 'wait': None, 'exit_code': None,
              'wall_start_unix_ns': time.time_ns(), 'cleanup_confirmed': False, 'cleanup': [], 'qpc_start': None, 'qpc_end': None, 'qpf': None}
    def fail(stage, error=None):
        if result['first_failure'] is None:
            result['first_failure'] = {'stage': stage, 'type': type(error).__name__ if error else 'ProcessError'}
            if type(error) is ProcessError: result['first_failure']['code'] = str(error)
    def capture(stream):
        # 主循环与退出后 drain 共用：逐流立即保存 cap 内 bytes，再判断 overflow。
        nonlocal stage
        stage = 'read-' + stream
        remaining = MAX_STREAM - len(result[stream])
        chunk = api.read(stream, remaining)
        require(type(chunk) is bytes, 'raw-stream-type')
        result[stream] += chunk[:remaining]
        if len(chunk) > remaining:
            stage = 'stream-overflow'
            raise ProcessError(stage)
        return bool(chunk)
    stage = 'clock'
    try:
        result['qpf'] = api.frequency()
        result['qpc_start'] = api.counter()
        stage = 'create'
        result['creation'] = api.create(exe, argv, cwd, env)
        stage = 'machine'
        result['machine'] = api.machine(api.process)
        require(result['machine']['accepted'], 'parent-machine-mismatch')
        stage = 'resume'
        api.resume()
        deadline = api.now() + timeout
        while True:
            for stream in ('stdout', 'stderr'):
                capture(stream)
            stage = 'wait'
            result['wait'] = api.wait()
            require(result['wait'] in (WAIT_OBJECT_0, WAIT_TIMEOUT), 'unexpected-wait')
            if result['wait'] == WAIT_OBJECT_0:
                stage = 'active'
                active = api.active()
                result['normal_active_processes'] = active
                require(active == 0, 'descendants-still-active')
                # 结束后继续有限 drain，直到两流本轮均无更多 bytes。
                drained = False
                for stream in ('stdout', 'stderr'):
                    if capture(stream): drained = True
                if drained:
                    stage = 'timeout'
                    require(api.now() < deadline, 'drain-timeout')
                    continue
                stage = 'exit'
                result['exit_code'] = api.exit_code()
                require(result['exit_code'] == 0, 'parent-nonzero')
                break
            stage = 'timeout'
            require(api.now() < deadline, 'parent-timeout')
            api.pause()
    except Exception as error:
        fail(stage, error)
    finally:
        if api.process is not None:
            try:
                deadline = api.now() + 10
                if result['first_failure'] is not None:
                    try: api.terminate()
                    except Exception as error:
                        result['cleanup'].append({'stage': 'terminate', 'type': type(error).__name__})
                while True:
                    wait, active = api.wait(), api.active()
                    # 只保存状态变化；所有真实 API 返回仍在 api_calls，不扩大 pipe buffers。
                    observation = {'wait': wait, 'active_processes': active}
                    if not result['cleanup'] or result['cleanup'][-1] != observation:
                        result['cleanup'].append(observation)
                    if wait == WAIT_OBJECT_0 and active == 0:
                        result['cleanup_confirmed'] = True
                        break
                    if api.now() >= deadline:
                        fail('cleanup-unconfirmed')
                        break
                    api.pause()
            except Exception as error: fail('cleanup', error)
        else:
            result['cleanup_confirmed'] = True  # 未 Create，无进程需要 wait；creation 字段仍缺席。
        try: result['qpc_end'] = api.counter()
        except Exception as error: fail('qpc-end', error)
        try: api.close_all()
        except Exception as error: fail('close', error)
        result['api_calls'] = list(getattr(api, 'calls', []))
        result['machine_queries'] = list(getattr(api, 'machine_queries', []))
        result['wall_end_unix_ns'] = time.time_ns()
    return result
