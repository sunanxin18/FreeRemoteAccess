//! 复用既有cursor guard的三页owner与隔离fault模式；仅本专用example使用。
use super::native::{expected, input, kernels, literal, COUNTS};
use std::{
    io::{self, Write},
    os::unix::process::ExitStatusExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub const CASES: [&str; 6] = [
    "valid",
    "below-read",
    "above-read",
    "below-write",
    "above-write",
    "readonly-write",
];

struct Guard {
    base: *mut libc::c_void,
    page: usize,
    mapped: usize,
}

impl Guard {
    fn new() -> io::Result<Self> {
        // SAFETY: 无参数查询；失败不构造owner。
        let raw_page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = usize::try_from(raw_page)
            .ok()
            .filter(|&page| page > 0)
            .ok_or_else(|| io::Error::other("无效系统page size"))?;
        let mapped = page
            .checked_mul(3)
            .ok_or_else(|| io::Error::other("三页长度溢出"))?;
        // SAFETY: 匿名私有映射，无已有地址/文件；只开放中间一页。
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapped,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let guard = Self { base, page, mapped };
        guard.protect(libc::PROT_READ | libc::PROT_WRITE)?;
        Ok(guard)
    }

    fn protect(&self, protection: libc::c_int) -> io::Result<()> {
        // SAFETY: 中间页位于本owner的有效映射内，地址与长度均page aligned。
        let result = unsafe {
            libc::mprotect(
                self.base.cast::<u8>().add(self.page).cast(),
                self.page,
                protection,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn edge(&self, bytes: usize, end: bool) -> *mut u8 {
        assert!(bytes <= self.page);
        // 空尾允许指向上保护页起点：仅构造零长度切片，不解引用该地址。
        let offset = if end {
            self.page * 2 - bytes
        } else {
            self.page
        };
        // SAFETY: offset在本完整三页mapping范围内，至少对齐u8且非null。
        unsafe { self.base.cast::<u8>().add(offset) }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: owner独占且精确释放原mapping一次。
        assert_eq!(unsafe { libc::munmap(self.base, self.mapped) }, 0);
    }
}

fn valid() -> Result<(), String> {
    for kernel in kernels() {
        literal(kernel);
        let mut cases = 0;
        for pixels in COUNTS {
            let source = input(pixels);
            let want = expected(&source);
            for source_end in [false, true] {
                for destination_end in [false, true] {
                    let source_guard = Guard::new().map_err(|e| e.to_string())?;
                    let destination_guard = Guard::new().map_err(|e| e.to_string())?;
                    let source_ptr = source_guard.edge(source.len(), source_end);
                    let destination_ptr = destination_guard.edge(source.len(), destination_end);
                    // SAFETY: 独立mapping的可写中页，精确长度已检查；len0不访问。
                    unsafe {
                        std::ptr::copy_nonoverlapping(source.as_ptr(), source_ptr, source.len());
                        std::ptr::write_bytes(destination_ptr, 0xa7, source.len());
                    }
                    source_guard
                        .protect(libc::PROT_READ)
                        .map_err(|e| e.to_string())?;
                    // SAFETY: mapping都活到本次调用/检查结束；source先设只读，
                    // destination独立独占，切片只覆盖确切范围，包括合法len0。
                    let guarded_source =
                        unsafe { std::slice::from_raw_parts(source_ptr, source.len()) };
                    let guarded_destination =
                        unsafe { std::slice::from_raw_parts_mut(destination_ptr, source.len()) };
                    kernel.convert(guarded_source, guarded_destination);
                    assert_eq!(guarded_destination, want, "kernel={} pixels={pixels} source_end={source_end} destination_end={destination_end}", kernel.name());
                    assert_eq!(guarded_source, source);
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

pub fn child(case: &str) -> Result<(), String> {
    if case == "valid" {
        valid()?;
        println!("GUARD_CHILD_OK case=valid");
        return Ok(());
    }
    let guard = Guard::new().map_err(|e| e.to_string())?;
    let below = case.starts_with("below-");
    let address = if case == "readonly-write" {
        guard.protect(libc::PROT_READ).map_err(|e| e.to_string())?;
        guard.edge(4, false)
    } else {
        let offset = if below {
            guard.page - 1
        } else {
            guard.page * 2
        };
        // SAFETY: 地址仍在mapping内；以下volatile操作故意触发已保护页的fault。
        unsafe { guard.base.cast::<u8>().add(offset) }
    };
    // 父进程只有收到这个已flush的精确marker才接受随后的SIGSEGV，
    // mmap/mprotect/准入失败、panic或无关早期崩溃均不能冒充故障负控制。
    println!("GUARD_READY case={case} expected=SIGSEGV");
    io::stdout().flush().map_err(|e| e.to_string())?;
    unsafe {
        if case.ends_with("-read") {
            std::ptr::read_volatile(address);
        } else {
            std::ptr::write_volatile(address, 0x31);
        }
    }
    Err(format!("保护页未产生预期fault case={case}"))
}

pub(super) fn terminate_and_reap(child: &mut Child, original: String) -> String {
    // kill失败也继续有界观察；保留原错误及kill结果，不声称一定已回收。
    let kill = match child.kill() {
        Ok(()) => "kill=ok".to_owned(),
        Err(error) => format!("kill=failed error={error}"),
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return format!(
                    "{original}; cleanup=reaped pid={} {kill} status={status}",
                    child.id()
                );
            }
            Err(error) => {
                return format!(
                    "{original}; cleanup=unconfirmed pid={} {kill} reap_error={error}",
                    child.id()
                );
            }
            Ok(None) if Instant::now() >= deadline => {
                return format!(
                    "{original}; cleanup=unconfirmed pid={} {kill} reap_deadline=1s",
                    child.id()
                );
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

// 只供本example及own target宿主控制；不暴露产品API或可调超时。
pub(super) fn observe_child(child: &mut Child, case: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let original = loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Err(error) => break format!("保护页child观察失败 case={case} error={error}"),
            Ok(None) if Instant::now() >= deadline => break format!("保护页child超时 case={case}"),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    // spawn后的观察错误与timeout只有这个出口，均先终止/有界回收再返回原错误。
    Err(terminate_and_reap(child, original))
}

pub fn parent() -> Result<(), String> {
    for case in CASES {
        let mut child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
            .args(["guard-child", case])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        // 固定10秒观察；输出最多几行，不会填满pipe。成功观察时child已回收。
        observe_child(&mut child, case)?;
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        let stdout = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
        let stderr = String::from_utf8(output.stderr).map_err(|e| e.to_string())?;
        if case == "valid" {
            if !output.status.success()
                || !stdout
                    .lines()
                    .any(|line| line == "GUARD_CHILD_OK case=valid")
            {
                return Err(format!(
                    "有效guard失败 status={} stdout={stdout} stderr={stderr}",
                    output.status
                ));
            }
        } else {
            let marker = format!("GUARD_READY case={case} expected=SIGSEGV");
            if output.status.signal() != Some(libc::SIGSEGV)
                || stdout.lines().filter(|line| *line == marker).count() != 1
                || !stderr.is_empty()
            {
                return Err(format!(
                    "fault负控制失败 case={case} status={} stdout={stdout} stderr={stderr}",
                    output.status
                ));
            }
        }
        print!("{stdout}");
        println!("GUARD_CHILD_CONFIRMED case={case} status={}", output.status);
    }
    println!("GUARDS_OK valid_children=1 exact_sigsegv_controls=5");
    Ok(())
}
