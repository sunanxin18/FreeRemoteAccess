use core::fmt;

pub const WINDOWS_INSTANCE_ALREADY_RUNNING: &str = "windows_instance_already_running";
const WINDOWS_INSTANCE_UNAVAILABLE: &str = "windows_instance_unavailable";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsSingleInstanceError {
    AlreadyRunning,
    Unavailable,
}

impl fmt::Display for WindowsSingleInstanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AlreadyRunning => WINDOWS_INSTANCE_ALREADY_RUNNING,
            Self::Unavailable => WINDOWS_INSTANCE_UNAVAILABLE,
        })
    }
}

impl std::error::Error for WindowsSingleInstanceError {}

#[cfg(windows)]
pub struct WindowsSingleInstanceGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl fmt::Debug for WindowsSingleInstanceGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WindowsSingleInstanceGuard")
    }
}

#[cfg(windows)]
impl WindowsSingleInstanceGuard {
    pub fn acquire_for_product(product_id: &str) -> Result<Self, WindowsSingleInstanceError> {
        use sha2::{Digest, Sha256};
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
        use windows_sys::Win32::System::Threading::CreateMutexW;

        let sid = current_user_sid()?;
        let product_hash = Sha256::digest(product_id.as_bytes());
        let product_hash = product_hash[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let name = format!("Global\\FreeRemoteDesk-{sid}-{product_hash}");
        let wide_name = name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();

        // SAFETY：名称以 NUL 结尾，并在调用期间保持有效。
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide_name.as_ptr()) };
        if handle.is_null() {
            return Err(WindowsSingleInstanceError::Unavailable);
        }
        // SAFETY：必须在 CreateMutexW 后立即读取 GetLastError。
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already_exists {
            // SAFETY：CreateMutexW 返回了自有的非空句柄。
            unsafe { CloseHandle(handle) };
            return Err(WindowsSingleInstanceError::AlreadyRunning);
        }

        Ok(Self { handle })
    }
}

#[cfg(windows)]
impl Drop for WindowsSingleInstanceGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;

        // SAFETY：guard 在 Drop 前独占该非空互斥体句柄。
        unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(windows)]
fn current_user_sid() -> Result<String, WindowsSingleInstanceError> {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY：token 指向可写存储，GetCurrentProcess 返回有效伪句柄。
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(WindowsSingleInstanceError::Unavailable);
    }

    let result = (|| {
        let mut required = 0u32;
        // SAFETY：首次调用仅用于查询所需缓冲区大小。
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required) };
        if required == 0 {
            return Err(WindowsSingleInstanceError::Unavailable);
        }

        let word_count = usize::try_from(required)
            .ok()
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<usize>() - 1))
            .map(|bytes| bytes / std::mem::size_of::<usize>())
            .ok_or(WindowsSingleInstanceError::Unavailable)?;
        let mut buffer = vec![0usize; word_count];
        // SAFETY：usize 缓冲区对 TOKEN_USER 具有足够大小和对齐。
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        } == 0
        {
            return Err(WindowsSingleInstanceError::Unavailable);
        }

        // SAFETY：GetTokenInformation 已在缓冲区起点初始化 TOKEN_USER。
        let token_user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
        let mut string_sid = std::ptr::null_mut();
        // SAFETY：在此作用域内 token_user.User.Sid 由令牌信息缓冲区持有。
        if unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut string_sid) } == 0 {
            return Err(WindowsSingleInstanceError::Unavailable);
        }
        let sid = {
            let mut length = 0usize;
            // SAFETY：ConvertSidToStringSidW 返回以 NUL 结尾的 LocalAlloc 字符串。
            while unsafe { *string_sid.add(length) } != 0 {
                length += 1;
            }
            // SAFETY：前述循环已确定初始化完成的 UTF-16 范围。
            String::from_utf16(unsafe { std::slice::from_raw_parts(string_sid, length) })
                .map_err(|_| WindowsSingleInstanceError::Unavailable)
        };
        // SAFETY：ConvertSidToStringSidW 规定该分配必须由 LocalFree 释放。
        unsafe { LocalFree(string_sid.cast()) };
        sid
    })();

    // SAFETY：OpenProcessToken 返回了自有令牌句柄。
    unsafe { CloseHandle(token) };
    result
}

#[cfg(not(windows))]
#[derive(Debug)]
pub struct WindowsSingleInstanceGuard;

#[cfg(not(windows))]
impl WindowsSingleInstanceGuard {
    pub fn acquire_for_product(_: &str) -> Result<Self, WindowsSingleInstanceError> {
        Err(WindowsSingleInstanceError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{WindowsSingleInstanceGuard, WINDOWS_INSTANCE_ALREADY_RUNNING};

    static NEXT_TEST_NAME: AtomicU64 = AtomicU64::new(1);

    #[cfg(windows)]
    #[test]
    fn a_second_same_user_instance_returns_the_exact_duplicate_error() {
        let suffix = format!(
            "task-10-test-{}-{}",
            std::process::id(),
            NEXT_TEST_NAME.fetch_add(1, Ordering::Relaxed)
        );
        let first = WindowsSingleInstanceGuard::acquire_for_product(&suffix)
            .expect("first product instance owns the mutex");

        let duplicate = WindowsSingleInstanceGuard::acquire_for_product(&suffix)
            .expect_err("second product instance is rejected");

        assert_eq!(duplicate.to_string(), WINDOWS_INSTANCE_ALREADY_RUNNING);
        drop(first);
        WindowsSingleInstanceGuard::acquire_for_product(&suffix)
            .expect("dropping the guard releases the mutex");
    }
}
