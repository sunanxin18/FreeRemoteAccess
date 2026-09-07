use std::fmt;
use std::path::Path;
#[derive(Debug)]
pub struct MacSingleInstanceGuard {
    _file: std::fs::File,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MacSingleInstanceError {
    AlreadyRunning,
    Unavailable,
}
impl fmt::Display for MacSingleInstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyRunning => "macos_instance_already_running",
            Self::Unavailable => "macos_instance_unavailable",
        })
    }
}
impl std::error::Error for MacSingleInstanceError {}
impl MacSingleInstanceGuard {
    pub fn acquire_for_product(product: &str) -> Result<Self, MacSingleInstanceError> {
        use sha2::{Digest, Sha256};
        let name = Sha256::digest(product.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let root =
            super::paths::application_support().map_err(|_| MacSingleInstanceError::Unavailable)?;
        Self::acquire_at(&root.join(format!("instance-{name}.lock")))
    }
    pub fn acquire_at(path: &Path) -> Result<Self, MacSingleInstanceError> {
        Self::lock_at(path, true)
    }
    pub(crate) fn lock_at(path: &Path, nonblocking: bool) -> Result<Self, MacSingleInstanceError> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::OpenOptionsExt;
            let parent = path.parent().ok_or(MacSingleInstanceError::Unavailable)?;
            super::paths::create_private_directory(parent)
                .map_err(|_| MacSingleInstanceError::Unavailable)?;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
                .map_err(|_| MacSingleInstanceError::Unavailable)?;
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
                    MacSingleInstanceError::AlreadyRunning
                } else {
                    MacSingleInstanceError::Unavailable
                });
            }
            // 文件描述符关闭即释放锁；不删除锁文件，避免后继进程锁住不同 inode。
            Ok(Self { _file: file })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, nonblocking);
            Err(MacSingleInstanceError::Unavailable)
        }
    }
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn lock_excludes_second_instance_and_releases_on_drop_without_unlinking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance.lock");
        let first = MacSingleInstanceGuard::acquire_at(&path).unwrap();
        assert_eq!(
            MacSingleInstanceGuard::acquire_at(&path).unwrap_err(),
            MacSingleInstanceError::AlreadyRunning
        );
        drop(first);
        let _next = MacSingleInstanceGuard::acquire_at(&path).unwrap();
        assert!(path.exists());
    }
}
