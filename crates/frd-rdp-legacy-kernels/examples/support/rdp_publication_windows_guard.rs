//! Windows 私有三页 owner 与固定 self-child；不改变产品或 Linux guard。
#[cfg(windows)]
use super::native::{expected, input, kernels, literal, COUNTS};
use std::{ffi::c_void, ptr::NonNull};
#[cfg(windows)]
use std::{
    io::{self, Write},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

const PAGE_NOACCESS: u32 = 1;
const PAGE_READONLY: u32 = 2;
const PAGE_READWRITE: u32 = 4;
const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_RELEASE: u32 = 0x8000;

// 私有注入缝只给 owner 错误控制使用，不模拟 native 保护页或 ISA。
trait MemoryApi {
    fn page_size(&self) -> usize;
    fn alloc(
        &self,
        address: *mut c_void,
        bytes: usize,
        kind: u32,
        protection: u32,
    ) -> Result<NonNull<c_void>, u32>;
    fn protect(&self, address: *mut c_void, bytes: usize, protection: u32) -> Result<u32, u32>;
    fn release(&self, address: *mut c_void) -> Result<(), u32>;
}

#[cfg(windows)]
mod win32 {
    use super::*;
    #[repr(C)]
    struct SystemInfo {
        architecture_and_reserved: u32,
        page_size: u32,
        minimum_address: *mut c_void,
        maximum_address: *mut c_void,
        active_processor_mask: usize,
        number_of_processors: u32,
        processor_type: u32,
        allocation_granularity: u32,
        processor_level: u16,
        processor_revision: u16,
    }
    // x86 和 x64 的指针/掩码字段按各自目标布局；不是复制 ARM64 假设。
    #[cfg(target_arch = "x86")]
    const _: () = {
        assert!(size_of::<SystemInfo>() == 36);
        assert!(align_of::<SystemInfo>() == 4);
        assert!(std::mem::offset_of!(SystemInfo, active_processor_mask) == 16);
        assert!(std::mem::offset_of!(SystemInfo, processor_level) == 32);
    };
    #[cfg(target_arch = "x86_64")]
    const _: () = {
        assert!(size_of::<SystemInfo>() == 48);
        assert!(align_of::<SystemInfo>() == 8);
        assert!(std::mem::offset_of!(SystemInfo, active_processor_mask) == 24);
        assert!(std::mem::offset_of!(SystemInfo, processor_level) == 44);
    };
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemInfo(info: *mut SystemInfo);
        fn GetLastError() -> u32;
        fn VirtualAlloc(
            address: *mut c_void,
            bytes: usize,
            kind: u32,
            protection: u32,
        ) -> *mut c_void;
        fn VirtualProtect(
            address: *mut c_void,
            bytes: usize,
            protection: u32,
            old: *mut u32,
        ) -> i32;
        fn VirtualFree(address: *mut c_void, bytes: usize, kind: u32) -> i32;
        fn SetErrorMode(mode: u32) -> u32;
    }
    pub(super) struct Api;
    impl MemoryApi for Api {
        fn page_size(&self) -> usize {
            let mut info = std::mem::MaybeUninit::<SystemInfo>::zeroed();
            // SAFETY: SYSTEM_INFO 按目标 Win32 布局；API 填满结构，无失败返回。
            unsafe {
                GetSystemInfo(info.as_mut_ptr());
                info.assume_init().page_size as usize
            }
        }
        fn alloc(
            &self,
            address: *mut c_void,
            bytes: usize,
            kind: u32,
            protection: u32,
        ) -> Result<NonNull<c_void>, u32> {
            // SAFETY: 仅本模块 owner 提供已检查的 reserve/commit 参数。
            NonNull::new(unsafe { VirtualAlloc(address, bytes, kind, protection) })
                .ok_or_else(|| unsafe { GetLastError() })
        }
        fn protect(&self, address: *mut c_void, bytes: usize, protection: u32) -> Result<u32, u32> {
            let mut old = 0;
            // SAFETY: owner 的中页，长度和地址由 reserve/commit 建立。
            if unsafe { VirtualProtect(address, bytes, protection, &mut old) } == 0 {
                Err(unsafe { GetLastError() })
            } else {
                Ok(old)
            }
        }
        fn release(&self, address: *mut c_void) -> Result<(), u32> {
            // SAFETY: 精确原始 reservation base；MEM_RELEASE 的长度必须为零。
            if unsafe { VirtualFree(address, 0, MEM_RELEASE) } == 0 {
                Err(unsafe { GetLastError() })
            } else {
                Ok(())
            }
        }
    }
    pub(super) fn suppress_error_dialogs() {
        // 仅本私有进程。保留已有 error-mode 位；不改系统 WER/注册表。
        const FLAGS: u32 = 0x0001 | 0x0002 | 0x8000;
        unsafe {
            let old = SetErrorMode(FLAGS);
            SetErrorMode(old | FLAGS);
        }
    }
}
#[cfg(windows)]
pub fn suppress_error_dialogs() {
    win32::suppress_error_dialogs();
}

struct Guard<A: MemoryApi> {
    api: A,
    base: Option<NonNull<c_void>>,
    page: usize,
    protection: Option<u32>,
}
impl<A: MemoryApi> Guard<A> {
    fn reserve(api: A) -> Result<Self, String> {
        let page = api.page_size();
        let bytes = page
            .checked_mul(3)
            .filter(|_| page >= 516)
            .ok_or_else(|| "无效 page size 或三页长度溢出".to_owned())?;
        let base = api
            .alloc(std::ptr::null_mut(), bytes, MEM_RESERVE, PAGE_NOACCESS)
            .map_err(|e| format!("VirtualAlloc reserve GetLastError={e}"))?;
        let owner = Self {
            api,
            base: Some(base),
            page,
            protection: None,
        };
        if (base.as_ptr() as usize).checked_add(bytes).is_none() {
            return Err("reservation 地址范围溢出；owner 尝试清理".into());
        }
        Ok(owner)
    }
    fn middle(&self) -> *mut c_void {
        let base = self.base.expect("仍持有 reservation");
        // reserve 已检查三页地址范围；这里只计算地址，不解引用。
        // wrapping_add 也允许纯 owner 控制使用无映射的假地址，实际访问仅在 Win32 commit 后。
        base.as_ptr().cast::<u8>().wrapping_add(self.page).cast()
    }
    fn commit(&mut self) -> Result<(), String> {
        if self.protection.is_some() {
            return Err("不允许重复 commit".into());
        }
        let address = self.middle();
        let committed = self
            .api
            .alloc(address, self.page, MEM_COMMIT, PAGE_READWRITE)
            .map_err(|e| format!("VirtualAlloc commit GetLastError={e}"))?;
        if committed.as_ptr() != address {
            return Err("VirtualAlloc commit 返回非中页地址".into());
        }
        self.protection = Some(PAGE_READWRITE);
        Ok(())
    }
    fn protect(&mut self, protection: u32) -> Result<(), String> {
        if !matches!(protection, PAGE_READONLY | PAGE_READWRITE) {
            return Err("不支持的 protection".into());
        }
        let previous = self.protection.ok_or("中页尚未 commit")?;
        let old = self
            .api
            .protect(self.middle(), self.page, protection)
            .map_err(|e| format!("VirtualProtect GetLastError={e}"))?;
        self.protection = Some(protection);
        if old != previous {
            return Err(format!("VirtualProtect old={old} expected={previous}"));
        }
        Ok(())
    }
    #[cfg(windows)]
    fn edge(&self, bytes: usize, end: bool) -> *mut u8 {
        assert!(self.protection.is_some() && bytes <= self.page);
        let offset = if end {
            self.page * 2 - bytes
        } else {
            self.page
        };
        // len0 的尾地址合法非空，但不可解引用；全三页仍在本 reservation 内。
        unsafe { self.base.unwrap().as_ptr().cast::<u8>().add(offset) }
    }
    fn try_release(&mut self) -> Result<(), String> {
        if let Some(base) = self.base {
            self.api
                .release(base.as_ptr())
                .map_err(|e| format!("VirtualFree GetLastError={e}"))?;
            self.base = None;
            self.protection = None;
        }
        Ok(())
    }
}
impl<A: MemoryApi> Drop for Guard<A> {
    fn drop(&mut self) {
        if let Err(error) = self.try_release() {
            // 首错误由调用方保留；cleanup 单独报告未释放地址，不声称成功释放。
            eprintln!(
                "FAIL cleanup={error} reservation_unreleased={:?}",
                self.base
            );
        }
    }
}

#[cfg(windows)]
pub const CASES: [&str; 6] = [
    "valid",
    "below-read",
    "above-read",
    "below-write",
    "above-write",
    "readonly-write",
];
#[cfg(windows)]
fn allocate() -> Result<Guard<win32::Api>, String> {
    let mut guard = Guard::reserve(win32::Api)?;
    guard.commit()?;
    Ok(guard)
}
#[cfg(windows)]
fn valid() -> Result<(), String> {
    for kernel in kernels() {
        literal(kernel);
        let mut cases = 0;
        for pixels in COUNTS {
            let source = input(pixels);
            let want = expected(&source);
            for source_end in [false, true] {
                for destination_end in [false, true] {
                    let mut source_guard = allocate()?;
                    let mut destination_guard = allocate()?;
                    let source_ptr = source_guard.edge(source.len(), source_end);
                    let destination_ptr = destination_guard.edge(source.len(), destination_end);
                    // SAFETY: 独立 reservation 中已 commit 的 RW 中页，长度已检查。
                    unsafe {
                        std::ptr::copy_nonoverlapping(source.as_ptr(), source_ptr, source.len());
                        std::ptr::write_bytes(destination_ptr, 0xa7, source.len());
                    }
                    source_guard.protect(PAGE_READONLY)?;
                    {
                        // SAFETY: source 已只读，不再构造可写 source；两个 owner 活到检查结束。
                        let guarded_source =
                            unsafe { std::slice::from_raw_parts(source_ptr, source.len()) };
                        let guarded_destination = unsafe {
                            std::slice::from_raw_parts_mut(destination_ptr, source.len())
                        };
                        kernel.convert(guarded_source, guarded_destination);
                        assert_eq!(guarded_destination, want, "kernel={} pixels={pixels} source_end={source_end} destination_end={destination_end}", kernel.name());
                        assert_eq!(guarded_source, source);
                    }
                    // 正常路径必须确认两个 reservation 均释放，不能由 Drop 静默兜底授 GREEN。
                    source_guard.try_release()?;
                    destination_guard.try_release()?;
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 160);
        println!(
            "GUARD_VALID kernel={} cases={cases} counts=40 placements=4 source_readonly=true",
            kernel.name()
        );
    }
    Ok(())
}
#[cfg(windows)]
pub fn child(case: &str) -> Result<(), String> {
    if case == "valid" {
        valid()?;
        println!("GUARD_CHILD_OK case=valid");
        io::stdout().flush().map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !CASES.contains(&case) {
        return Err("未知固定 child case".into());
    }
    let mut guard = allocate()?;
    let address = if case == "readonly-write" {
        guard.protect(PAGE_READONLY)?;
        guard.edge(4, false)
    } else {
        let offset = if case.starts_with("below-") {
            guard.page - 1
        } else {
            guard.page * 2
        };
        // SAFETY: reservation 内不可访问的保护页；下面的 volatile 故意触发真实 AV。
        unsafe { guard.base.unwrap().as_ptr().cast::<u8>().add(offset) }
    };
    println!("GUARD_READY case={case} expected=0xC0000005");
    io::stdout().flush().map_err(|e| e.to_string())?;
    unsafe {
        if case.ends_with("-read") {
            std::ptr::read_volatile(address);
        } else {
            std::ptr::write_volatile(address, 0x31);
        }
    }
    Err(format!("保护页未产生预期 fault case={case}"))
}

#[cfg(windows)]
struct ChildFailure {
    original: String,
    reaped: bool,
}
#[cfg(windows)]
fn terminate_and_reap(child: &mut Child, original: String) -> ChildFailure {
    let kill = match child.kill() {
        Ok(()) => "kill=ok".to_owned(),
        Err(error) => format!("kill=failed error={error}"),
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return ChildFailure {
                    original: format!(
                        "{original}; cleanup=reaped pid={} {kill} status={status}",
                        child.id()
                    ),
                    reaped: true,
                }
            }
            Err(error) => {
                return ChildFailure {
                    original: format!(
                        "{original}; cleanup=unconfirmed pid={} {kill} reap_error={error}",
                        child.id()
                    ),
                    reaped: false,
                }
            }
            Ok(None) if Instant::now() >= deadline => {
                return ChildFailure {
                    original: format!(
                        "{original}; cleanup=unconfirmed pid={} {kill} reap_deadline=1s",
                        child.id()
                    ),
                    reaped: false,
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}
#[cfg(windows)]
fn observe_child(child: &mut Child, case: &str) -> Result<ExitStatus, ChildFailure> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let original = loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Err(error) => break format!("保护页 child 观察失败 case={case} error={error}"),
            Ok(None) if Instant::now() >= deadline => {
                break format!("保护页 child 超时 case={case}")
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    Err(terminate_and_reap(child, original))
}
#[cfg(windows)]
pub fn parent() -> Result<(), String> {
    for case in CASES {
        let mut child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
            .args(["guard-child", case])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let observed = observe_child(&mut child, case);
        if let Err(failure) = &observed {
            if !failure.reaped {
                // 不对仍可能运行的进程做阻塞 pipe read；明确未证明停止/完整捕获。
                return Err(format!(
                    "{}; child_raw_capture=unconfirmed",
                    failure.original
                ));
            }
        }
        // 固定 child 不产生后代，输出最多几行；terminal status 已确认才 drain 原 streams。
        let output = child.wait_with_output().map_err(|error| match &observed {
            Ok(_) => format!("child raw capture failed error={error}"),
            Err(failure) => format!(
                "{}; child raw capture failed error={error}",
                failure.original
            ),
        })?;
        io::stdout().write_all(&output.stdout).map_err(|e| {
            format!(
                "child stdout 写回失败 error={e}; original={}",
                observed.as_ref().err().map_or("none", |f| &f.original)
            )
        })?;
        io::stderr().write_all(&output.stderr).map_err(|e| {
            format!(
                "child stderr 写回失败 error={e}; original={}",
                observed.as_ref().err().map_or("none", |f| &f.original)
            )
        })?;
        if let Err(failure) = observed {
            return Err(failure.original);
        }
        let stdout = std::str::from_utf8(&output.stdout).map_err(|e| e.to_string())?;
        let expected = if case == "valid" {
            "GUARD_VALID kernel=sse2 cases=160 counts=40 placements=4 source_readonly=true\nGUARD_VALID kernel=ssse3 cases=160 counts=40 placements=4 source_readonly=true\nGUARD_CHILD_OK case=valid\n".to_owned()
        } else {
            format!("GUARD_READY case={case} expected=0xC0000005\n")
        };
        let status_ok = if case == "valid" {
            output.status.code() == Some(0)
        } else {
            output.status.code().map(|code| code as u32) == Some(0xc0000005)
        };
        if !status_ok || stdout != expected || !output.stderr.is_empty() {
            return Err(format!(
                "child 拒绝 case={case} status={} stdout_bytes={} stderr_bytes={} exact_markers={}",
                output.status,
                output.stdout.len(),
                output.stderr.len(),
                stdout == expected
            ));
        }
        println!(
            "GUARD_CHILD_CONFIRMED case={case} exit_u32=0x{:08X}",
            output.status.code().unwrap() as u32
        );
    }
    println!("GUARDS_OK valid_children=1 exact_access_violation_controls=5");
    Ok(())
}

#[cfg(test)]
mod controls {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    #[derive(Default)]
    struct State {
        page: Cell<usize>,
        fail: Cell<Option<&'static str>>,
        calls: RefCell<Vec<String>>,
        protection: Cell<u32>,
    }
    #[derive(Clone)]
    struct Fake(Rc<State>);
    impl Fake {
        fn new() -> Self {
            let state = State::default();
            state.page.set(4096);
            state.protection.set(PAGE_READWRITE);
            Self(Rc::new(state))
        }
    }
    impl MemoryApi for Fake {
        fn page_size(&self) -> usize {
            self.0.page.get()
        }
        fn alloc(
            &self,
            address: *mut c_void,
            bytes: usize,
            kind: u32,
            protection: u32,
        ) -> Result<NonNull<c_void>, u32> {
            self.0
                .calls
                .borrow_mut()
                .push(format!("alloc:{address:p}:{bytes}:{kind}:{protection}"));
            if self.0.fail.get()
                == Some(if kind == MEM_RESERVE {
                    "reserve"
                } else {
                    "commit"
                })
            {
                return Err(111);
            }
            Ok(NonNull::new(if address.is_null() {
                0x10000usize as *mut c_void
            } else {
                address
            })
            .unwrap())
        }
        fn protect(&self, address: *mut c_void, bytes: usize, protection: u32) -> Result<u32, u32> {
            self.0
                .calls
                .borrow_mut()
                .push(format!("protect:{address:p}:{bytes}:{protection}"));
            if self.0.fail.get() == Some("protect") {
                return Err(222);
            }
            Ok(self.0.protection.replace(protection))
        }
        fn release(&self, address: *mut c_void) -> Result<(), u32> {
            self.0
                .calls
                .borrow_mut()
                .push(format!("release:{address:p}"));
            if self.0.fail.get() == Some("release") {
                Err(333)
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn failed_release_retains_owner_and_allows_retry() {
        let api = Fake::new();
        let mut guard = Guard::reserve(api.clone()).unwrap();
        guard.commit().unwrap();
        api.0.fail.set(Some("release"));
        assert_eq!(
            guard.try_release().unwrap_err(),
            "VirtualFree GetLastError=333"
        );
        assert!(guard.base.is_some(), "release 失败必须保留 owner");
        assert_eq!(guard.protection, Some(PAGE_READWRITE));
        api.0.fail.set(None);
        guard.try_release().unwrap();
        assert!(guard.base.is_none());
        assert_eq!(guard.protection, None);
        assert_eq!(
            api.0
                .calls
                .borrow()
                .iter()
                .filter(|s| s.starts_with("release:"))
                .count(),
            2
        );
    }
    #[test]
    fn exact_reserve_commit_repeat_readonly_restore_and_release() {
        let api = Fake::new();
        let mut guard = Guard::reserve(api.clone()).unwrap();
        guard.commit().unwrap();
        guard.protect(PAGE_READONLY).unwrap();
        guard.protect(PAGE_READONLY).unwrap();
        guard.protect(PAGE_READWRITE).unwrap();
        guard.try_release().unwrap();
        assert_eq!(
            *api.0.calls.borrow(),
            [
                "alloc:0x0:12288:8192:1",
                "alloc:0x11000:4096:4096:4",
                "protect:0x11000:4096:2",
                "protect:0x11000:4096:2",
                "protect:0x11000:4096:4",
                "release:0x10000"
            ]
        );
    }
    #[test]
    fn page_and_reserve_failures_never_create_owner() {
        let api = Fake::new();
        api.0.page.set(0);
        assert!(Guard::reserve(api.clone()).is_err());
        api.0.page.set(usize::MAX);
        assert!(Guard::reserve(api.clone()).is_err());
        assert!(api.0.calls.borrow().is_empty());
        api.0.page.set(4096);
        api.0.fail.set(Some("reserve"));
        assert!(Guard::reserve(api.clone())
            .err()
            .unwrap()
            .contains("GetLastError=111"));
        assert_eq!(api.0.calls.borrow().len(), 1);
    }
    #[test]
    fn commit_and_protect_errors_preserve_owner_and_first_error() {
        let api = Fake::new();
        let mut guard = Guard::reserve(api.clone()).unwrap();
        api.0.fail.set(Some("commit"));
        assert_eq!(
            guard.commit().unwrap_err(),
            "VirtualAlloc commit GetLastError=111"
        );
        assert!(guard.base.is_some());
        assert_eq!(guard.protection, None);
        api.0.fail.set(None);
        guard.commit().unwrap();
        api.0.fail.set(Some("protect"));
        assert_eq!(
            guard.protect(PAGE_READONLY).unwrap_err(),
            "VirtualProtect GetLastError=222"
        );
        assert_eq!(guard.protection, Some(PAGE_READWRITE));
        assert!(guard.base.is_some());
        api.0.fail.set(None);
        guard.try_release().unwrap();
    }
}
