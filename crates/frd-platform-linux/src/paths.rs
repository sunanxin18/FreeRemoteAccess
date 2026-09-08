use frd_platform_api::PlatformError;
use std::path::{Path, PathBuf};

pub(crate) fn application_support() -> Result<PathBuf, PlatformError> {
    resolve_data_home(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}
fn resolve_data_home(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, PlatformError> {
    let base = match xdg.filter(|v| !v.is_empty() && Path::new(v).is_absolute()) {
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(
            home.filter(|v| !v.is_empty())
                .ok_or(PlatformError::Unavailable)?,
        )
        .join(".local/share"),
    };
    if !base.is_absolute()
        || base
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(PlatformError::Unavailable);
    }
    Ok(base.join("FreeRemoteDesk"))
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::{
        ffi::{CString, OsStr},
        fs::File,
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
        path::Component,
    };
    const MAX_DOCUMENT: u64 = 4 * 1024 * 1024;
    pub(crate) struct Directory(File);
    fn name(value: &OsStr) -> Result<CString, PlatformError> {
        CString::new(value.as_bytes()).map_err(|_| PlatformError::StorageFailed)
    }
    fn owned_private(file: &File, directory: bool) -> Result<(), PlatformError> {
        let m = file.metadata().map_err(|_| PlatformError::StorageFailed)?;
        // SAFETY：geteuid 不读写 Rust 内存。
        let uid = unsafe { libc::geteuid() };
        if m.uid() != uid
            || m.mode() & 0o077 != 0
            || (directory && !m.is_dir())
            || (!directory && (!m.is_file() || m.nlink() != 1))
        {
            return Err(PlatformError::StorageFailed);
        }
        Ok(())
    }
    impl Directory {
        pub(crate) fn open(path: &Path) -> Result<Self, PlatformError> {
            if !path.is_absolute() {
                return Err(PlatformError::StorageFailed);
            }
            let components: Vec<_> = path.components().collect();
            if components.len() < 2
                || components
                    .iter()
                    .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
            {
                return Err(PlatformError::StorageFailed);
            }
            let mut current = File::open("/").map_err(|_| PlatformError::StorageFailed)?;
            for (index, component) in components.iter().enumerate().skip(1) {
                let Component::Normal(component) = component else {
                    return Err(PlatformError::StorageFailed);
                };
                let component = name(component)?;
                // SAFETY：目录描述符有效，名字以 NUL 结尾；不跟随符号链接。
                let mut fd = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        component.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
                {
                    // SAFETY：同上，创建权限由 umask 进一步收紧。
                    let status =
                        unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), 0o700) };
                    if status != 0
                        && std::io::Error::last_os_error().kind()
                            != std::io::ErrorKind::AlreadyExists
                    {
                        return Err(PlatformError::StorageFailed);
                    }
                    fd = unsafe {
                        libc::openat(
                            current.as_raw_fd(),
                            component.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                        )
                    };
                }
                if fd < 0 {
                    return Err(PlatformError::StorageFailed);
                }
                // SAFETY：openat 成功返回独占的新描述符。
                current = unsafe { File::from_raw_fd(fd) };
                let m = current
                    .metadata()
                    .map_err(|_| PlatformError::StorageFailed)?;
                let uid = unsafe { libc::geteuid() };
                // 祖先必须由当前用户或 root 控制；共享临时目录只有 sticky 形式可接受。
                if (m.uid() != uid && m.uid() != 0)
                    || (m.mode() & 0o022 != 0 && m.mode() & 0o1000 == 0)
                {
                    return Err(PlatformError::StorageFailed);
                }
                if index == components.len() - 1 {
                    owned_private(&current, true)?;
                }
            }
            Ok(Self(current))
        }
        fn open_file(&self, filename: &OsStr, create: bool) -> Result<Option<File>, PlatformError> {
            let filename = name(filename)?;
            let flags = libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | if create {
                    libc::O_RDWR | libc::O_CREAT
                } else {
                    libc::O_RDONLY
                };
            let mut fd =
                unsafe { libc::openat(self.0.as_raw_fd(), filename.as_ptr(), flags, 0o600) };
            // Darwin 的并发首次 O_CREAT|O_NOFOLLOW 可暂时返回 ENOENT；只重试同一目录描述符。
            for _ in 0..8 {
                if fd >= 0
                    || !create
                    || std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound
                {
                    break;
                }
                fd = unsafe { libc::openat(self.0.as_raw_fd(), filename.as_ptr(), flags, 0o600) };
            }
            if fd < 0 {
                return if !create
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
                {
                    Ok(None)
                } else {
                    Err(PlatformError::StorageFailed)
                };
            }
            let file = unsafe { File::from_raw_fd(fd) };
            owned_private(&file, false)?;
            Ok(Some(file))
        }
    }
    fn parent(path: &Path) -> Result<(Directory, &OsStr), PlatformError> {
        Ok((
            Directory::open(path.parent().ok_or(PlatformError::StorageFailed)?)?,
            path.file_name().ok_or(PlatformError::StorageFailed)?,
        ))
    }
    pub(crate) fn lock_file(path: &Path) -> Result<File, PlatformError> {
        let (dir, filename) = parent(path)?;
        dir.open_file(filename, true)?
            .ok_or(PlatformError::StorageFailed)
    }
    pub(crate) fn read(path: &Path) -> Result<Option<Vec<u8>>, PlatformError> {
        let (dir, filename) = parent(path)?;
        let Some(file) = dir.open_file(filename, false)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.take(MAX_DOCUMENT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PlatformError::StorageFailed)?;
        if bytes.len() as u64 > MAX_DOCUMENT {
            return Err(PlatformError::StorageFailed);
        }
        Ok(Some(bytes))
    }
    pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
        if bytes.len() as u64 > MAX_DOCUMENT {
            return Err(PlatformError::StorageFailed);
        }
        let (dir, filename) = parent(path)?;
        // 拒绝已存在的链接/非私有目标，即使 rename 本身不会跟随它。
        dir.open_file(filename, false)?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temporary = CString::new(format!(
            ".atomic-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
        .unwrap();
        let fd = unsafe {
            libc::openat(
                dir.0.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(PlatformError::StorageFailed);
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(bytes)
                .map_err(|_| PlatformError::StorageFailed)?;
            file.sync_all().map_err(|_| PlatformError::StorageFailed)?;
            let destination = name(filename)?;
            if unsafe {
                libc::renameat(
                    dir.0.as_raw_fd(),
                    temporary.as_ptr(),
                    dir.0.as_raw_fd(),
                    destination.as_ptr(),
                )
            } != 0
            {
                return Err(PlatformError::StorageFailed);
            }
            dir.0.sync_all().map_err(|_| PlatformError::StorageFailed)
        })();
        // SAFETY：仅清理同一已打开目录中的临时名字；成功 rename 后此调用无作用。
        unsafe {
            libc::unlinkat(dir.0.as_raw_fd(), temporary.as_ptr(), 0);
        }
        result
    }
}
#[cfg(unix)]
pub(crate) use native::{lock_file, read, write_atomic};
#[cfg(not(unix))]
pub(crate) fn lock_file(_: &Path) -> Result<std::fs::File, PlatformError> {
    Err(PlatformError::Unavailable)
}
#[cfg(not(unix))]
pub(crate) fn read(_: &Path) -> Result<Option<Vec<u8>>, PlatformError> {
    Err(PlatformError::Unavailable)
}
#[cfg(not(unix))]
pub(crate) fn write_atomic(_: &Path, _: &[u8]) -> Result<(), PlatformError> {
    Err(PlatformError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xdg_paths_reject_relative_and_traversal_and_use_home_fallback() {
        assert_eq!(
            resolve_data_home(Some("relative".into()), Some("/home/test".into())).unwrap(),
            PathBuf::from("/home/test/.local/share/FreeRemoteDesk")
        );
        assert!(resolve_data_home(None, Some("relative".into())).is_err());
        assert!(resolve_data_home(Some("/data/../escape".into()), None).is_err());
        assert_eq!(
            resolve_data_home(None, Some("/home/test".into())).unwrap(),
            PathBuf::from("/home/test/.local/share/FreeRemoteDesk")
        );
        assert_eq!(
            resolve_data_home(Some("/data".into()), None).unwrap(),
            PathBuf::from("/data/FreeRemoteDesk")
        );
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_hardlink_public_directory_and_public_file() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = crate::private_tempdir();
        let root = temp.path().canonicalize().unwrap();
        let file = root.join("data");
        write_atomic(&file, b"fixture").unwrap();
        symlink(&file, root.join("linked")).unwrap();
        assert!(read(&root.join("linked")).is_err());
        std::fs::hard_link(&file, root.join("hard")).unwrap();
        assert!(read(&file).is_err());
        std::fs::remove_file(root.join("hard")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&file).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(write_atomic(&root.join("other"), b"x").is_err());
    }
}
