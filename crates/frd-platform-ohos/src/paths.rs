use frd_platform_api::PlatformError;
use std::path::Path;

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
    struct ContextRoot {
        path: std::path::PathBuf,
        directory: File,
    }
    static CONTEXT_ROOT: std::sync::Mutex<Option<ContextRoot>> = std::sync::Mutex::new(None);

    /// root 必须来自 Ability Context.filesDir 加产品目录，不接受连接表单路径。
    /// OS 管理的 sandbox 祖先不套用桌面 uid 规则；信任固定在已打开的目录 fd。
    pub(crate) fn initialize_context_storage(root: &Path) -> Result<(), PlatformError> {
        use std::os::unix::fs::OpenOptionsExt;
        if !root.is_absolute()
            || root
                .components()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err(PlatformError::StorageFailed);
        }
        let mut capability = CONTEXT_ROOT
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?;
        if let Some(current) = capability.as_ref() {
            return if current.path == root {
                Ok(())
            } else {
                Err(PlatformError::StorageFailed)
            };
        }
        let parent = root.parent().ok_or(PlatformError::StorageFailed)?;
        let parent = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|_| PlatformError::StorageFailed)?;
        let metadata = parent
            .metadata()
            .map_err(|_| PlatformError::StorageFailed)?;
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(PlatformError::StorageFailed);
        }
        let directory = child(
            &parent,
            root.file_name().ok_or(PlatformError::StorageFailed)?,
        )?;
        owned_private(&directory, true)?;
        *capability = Some(ContextRoot {
            path: root.to_owned(),
            directory,
        });
        Ok(())
    }
    fn child(parent: &File, component: &OsStr) -> Result<File, PlatformError> {
        let component = name(component)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut fd = unsafe { libc::openat(parent.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            let result = unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) };
            if result != 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(PlatformError::StorageFailed);
            }
            fd = unsafe { libc::openat(parent.as_raw_fd(), component.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err(PlatformError::StorageFailed);
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
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
            let anchored = {
                let capability = CONTEXT_ROOT
                    .lock()
                    .map_err(|_| PlatformError::StorageFailed)?;
                capability
                    .as_ref()
                    .and_then(|root| {
                        path.strip_prefix(&root.path)
                            .ok()
                            .map(|relative| (root, relative))
                    })
                    .map(|(root, relative)| {
                        root.directory
                            .try_clone()
                            .map(|fd| (fd, relative.to_owned()))
                    })
                    .transpose()
                    .map_err(|_| PlatformError::StorageFailed)?
            };
            if let Some((mut current, relative)) = anchored {
                owned_private(&current, true)?;
                for component in relative.components() {
                    let Component::Normal(component) = component else {
                        return Err(PlatformError::StorageFailed);
                    };
                    current = child(&current, component)?;
                    owned_private(&current, true)?;
                }
                return Ok(Self(current));
            }
            // 生产 OHOS 存储只能在 Context 注册的能力下，不回退到任意绝对路径。
            #[cfg(target_env = "ohos")]
            return Err(PlatformError::StorageFailed);
            #[cfg(not(target_env = "ohos"))]
            Self::open_unanchored(&components)
        }
        #[cfg(not(target_env = "ohos"))]
        fn open_unanchored(components: &[Component<'_>]) -> Result<Self, PlatformError> {
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
    pub(crate) fn remove(path: &Path) -> Result<(), PlatformError> {
        let (dir, filename) = parent(path)?;
        if dir.open_file(filename, false)?.is_none() {
            return Ok(());
        }
        let filename = name(filename)?;
        if unsafe { libc::unlinkat(dir.0.as_raw_fd(), filename.as_ptr(), 0) } != 0 {
            return Err(PlatformError::StorageFailed);
        }
        dir.0.sync_all().map_err(|_| PlatformError::StorageFailed)
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
pub(crate) use native::{initialize_context_storage, lock_file, read, remove, write_atomic};
#[cfg(not(unix))]
pub(crate) fn initialize_context_storage(_: &Path) -> Result<(), PlatformError> {
    Err(PlatformError::Unavailable)
}
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
    #[cfg(unix)]
    #[test]
    fn context_capability_is_immutable_and_keeps_private_nofollow_rules() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = crate::private_tempdir();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("application");
        initialize_context_storage(&root).unwrap();
        initialize_context_storage(&root).unwrap();
        assert!(initialize_context_storage(&base.join("replacement")).is_err());
        write_atomic(&root.join("data"), b"original").unwrap();
        symlink(base.join("outside"), root.join("link")).unwrap();
        assert!(write_atomic(&root.join("link/file"), b"invalid").is_err());
        std::fs::create_dir(root.join("public")).unwrap();
        std::fs::set_permissions(root.join("public"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(write_atomic(&root.join("public/file"), b"invalid").is_err());
        std::fs::rename(&root, base.join("original")).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        initialize_context_storage(&root).unwrap();
        write_atomic(&root.join("later"), b"anchored").unwrap();
        assert!(!root.join("later").exists());
        assert_eq!(
            std::fs::read(base.join("original/later")).unwrap(),
            b"anchored"
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

#[cfg(not(unix))]
pub(crate) fn remove(_: &Path) -> Result<(), PlatformError> {
    Err(PlatformError::Unavailable)
}
