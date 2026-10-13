use std::fmt;
use std::path::Path;
#[derive(Debug)]
pub struct OhosSingleInstanceGuard {
    _file: std::fs::File,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OhosSingleInstanceError {
    AlreadyRunning,
    Unavailable,
}
impl fmt::Display for OhosSingleInstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyRunning => "ohos_instance_already_running",
            Self::Unavailable => "ohos_instance_unavailable",
        })
    }
}
impl std::error::Error for OhosSingleInstanceError {}
impl OhosSingleInstanceGuard {
    pub fn acquire_at(path: &Path) -> Result<Self, OhosSingleInstanceError> {
        Self::lock_at(path, true)
    }
    pub(crate) fn lock_at(path: &Path, nonblocking: bool) -> Result<Self, OhosSingleInstanceError> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let file =
                super::paths::lock_file(path).map_err(|_| OhosSingleInstanceError::Unavailable)?;
            let operation = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
            loop {
                // SAFETY：file 持有有效描述符；flock 不保留 Rust 内存引用。
                if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
                    OhosSingleInstanceError::AlreadyRunning
                } else {
                    OhosSingleInstanceError::Unavailable
                });
            }
            // 文件描述符关闭即释放锁；不删除锁文件，避免后继进程锁住不同 inode。
            Ok(Self { _file: file })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, nonblocking);
            Err(OhosSingleInstanceError::Unavailable)
        }
    }
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn lock_excludes_second_instance_and_releases_on_drop_without_unlinking() {
        let dir = crate::private_tempdir();
        let path = dir.path().canonicalize().unwrap().join("instance.lock");
        let first = OhosSingleInstanceGuard::acquire_at(&path).unwrap();
        assert_eq!(
            OhosSingleInstanceGuard::acquire_at(&path).unwrap_err(),
            OhosSingleInstanceError::AlreadyRunning
        );
        drop(first);
        let _next = OhosSingleInstanceGuard::acquire_at(&path).unwrap();
        assert!(path.exists());
    }
}
