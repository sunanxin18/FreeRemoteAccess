//! 生产动态库加载前的受信安装链验证。
//!
//! 这里只接受由受信系统主体拥有且普通用户不可改写的现有对象。所有路径组件和动态库
//! 本身都保持打开，直到调用平台加载器之后；测试路径覆盖由 `loader` 私有入口完成，不能
//! 改变生产入口绑定当前可执行文件目录的规则。

use std::fs::File;
use std::path::{Component, Path, PathBuf};

pub(crate) struct TrustedLoadPaths {
    pub(crate) dependencies: Vec<PathBuf>,
    pub(crate) plugin: PathBuf,
    _open_objects: Vec<VerifiedObject>,
}

struct VerifiedObject {
    path: PathBuf,
    kind: ExpectedKind,
    identity: platform::FileIdentity,
    _file: File,
}

#[derive(Clone, Copy)]
enum ExpectedKind {
    AncestorDirectory,
    SearchDirectory,
    RegularFile,
}

pub(crate) fn prepare(
    application_dir: &Path,
    platform_name: &str,
    dependency_names: &[&str],
    plugin_name: &str,
) -> Result<TrustedLoadPaths, ()> {
    let codec_dir = production_codec_dir(application_dir, platform_name);
    prepare_at_codec_dir(application_dir, &codec_dir, dependency_names, plugin_name)
}

fn production_codec_dir(application_dir: &Path, platform_name: &str) -> PathBuf {
    application_dir
        .join("codecs")
        .join("ffmpeg-8.1.2")
        .join(platform_name)
}

#[cfg(test)]
pub(crate) fn production_codec_dir_for_test(
    application_dir: &Path,
    platform_name: &str,
) -> PathBuf {
    production_codec_dir(application_dir, platform_name)
}

#[cfg(test)]
pub(crate) fn prepare_existing_platform_for_test(
    application_dir: &Path,
    codec_dir: &Path,
    dependency_names: &[&str],
    plugin_name: &str,
) -> Result<TrustedLoadPaths, ()> {
    prepare_at_codec_dir(application_dir, codec_dir, dependency_names, plugin_name)
}

fn prepare_at_codec_dir(
    application_dir: &Path,
    codec_dir: &Path,
    dependency_names: &[&str],
    plugin_name: &str,
) -> Result<TrustedLoadPaths, ()> {
    validate_absolute_clean_path(application_dir)?;
    validate_absolute_clean_path(codec_dir)?;
    if !codec_dir.starts_with(application_dir) {
        return Err(());
    }
    for name in dependency_names
        .iter()
        .copied()
        .chain(std::iter::once(plugin_name))
    {
        validate_file_name(name)?;
    }

    let dependencies = dependency_names
        .iter()
        .map(|name| codec_dir.join(name))
        .collect::<Vec<_>>();
    let plugin = codec_dir.join(plugin_name);

    let mut directory_paths = codec_dir
        .ancestors()
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    directory_paths.reverse();

    let mut open_objects = Vec::new();
    for path in directory_paths {
        let kind = if path == application_dir || path == codec_dir {
            ExpectedKind::SearchDirectory
        } else {
            ExpectedKind::AncestorDirectory
        };
        open_objects.push(verify_object(path, kind)?);
    }
    let first_library_index = open_objects.len();
    for path in dependencies.iter().chain(std::iter::once(&plugin)) {
        open_objects.push(verify_object(path.clone(), ExpectedKind::RegularFile)?);
    }

    // Re-open and compare every object after the complete chain has passed ownership/ACL checks.
    // Windows share modes also deny replacement while the retained handles are live. On Unix the
    // root-owned, non-writable ancestor chain supplies that guarantee.
    for object in &open_objects {
        let second = platform::open_verified(&object.path, object.kind)?;
        if second.identity != object.identity {
            return Err(());
        }
    }

    let canonical_application_dir = application_dir.canonicalize().map_err(|_| ())?;
    let canonical_codec_dir = codec_dir.canonicalize().map_err(|_| ())?;
    if !canonical_codec_dir.starts_with(&canonical_application_dir) {
        return Err(());
    }
    let mut canonical_libraries = dependencies
        .iter()
        .chain(std::iter::once(&plugin))
        .map(|path| path.canonicalize().map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?;
    for (path, object) in canonical_libraries
        .iter()
        .zip(&open_objects[first_library_index..])
    {
        if path.parent() != Some(canonical_codec_dir.as_path())
            || platform::open_verified(path, ExpectedKind::RegularFile)?.identity != object.identity
        {
            return Err(());
        }
    }
    let canonical_plugin = canonical_libraries.pop().ok_or(())?;
    let dependencies = canonical_libraries
        .into_iter()
        .map(platform::loader_path)
        .collect::<Result<Vec<_>, _>>()?;
    let plugin = platform::loader_path(canonical_plugin)?;

    Ok(TrustedLoadPaths {
        dependencies,
        plugin,
        _open_objects: open_objects,
    })
}

#[cfg(test)]
impl TrustedLoadPaths {
    pub(crate) fn retained_object_count_for_test(&self) -> usize {
        self._open_objects.len()
    }
}

#[cfg(test)]
pub(crate) fn verify_install_root(path: &Path) -> Result<(), ()> {
    validate_absolute_clean_path(path)?;
    verify_object(path.to_path_buf(), ExpectedKind::SearchDirectory).map(|_| ())
}

fn verify_object(path: PathBuf, kind: ExpectedKind) -> Result<VerifiedObject, ()> {
    let opened = platform::open_verified(&path, kind)?;
    platform::verify_trust(&path, &opened.file, kind)?;
    Ok(VerifiedObject {
        path,
        kind,
        identity: opened.identity,
        _file: opened.file,
    })
}

fn validate_absolute_clean_path(path: &Path) -> Result<(), ()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(());
    }
    Ok(())
}

fn validate_file_name(name: &str) -> Result<(), ()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(());
    }
    Ok(())
}

mod platform {
    use super::{ExpectedKind, File};
    use std::path::Path;

    #[derive(Clone, Copy, Eq, PartialEq)]
    pub(super) struct FileIdentity {
        first: u64,
        second: u64,
    }

    pub(super) struct OpenedObject {
        pub(super) file: File,
        pub(super) identity: FileIdentity,
    }

    #[cfg(windows)]
    pub(super) fn loader_path(path: std::path::PathBuf) -> Result<std::path::PathBuf, ()> {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        const VERBATIM_PREFIX: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        const VERBATIM_UNC_PREFIX: &[u16] = &[
            b'\\' as u16,
            b'\\' as u16,
            b'?' as u16,
            b'\\' as u16,
            b'U' as u16,
            b'N' as u16,
            b'C' as u16,
            b'\\' as u16,
        ];
        let loader_wide = if wide.starts_with(VERBATIM_UNC_PREFIX) {
            let mut ordinary_unc = vec![b'\\' as u16, b'\\' as u16];
            ordinary_unc.extend_from_slice(&wide[VERBATIM_UNC_PREFIX.len()..]);
            ordinary_unc
        } else if wide.starts_with(VERBATIM_PREFIX) {
            wide[VERBATIM_PREFIX.len()..].to_vec()
        } else {
            wide
        };
        let loader_path = std::path::PathBuf::from(OsString::from_wide(&loader_wide));
        if !loader_path.is_absolute() {
            return Err(());
        }
        Ok(loader_path)
    }

    #[cfg(windows)]
    pub(super) fn open_verified(path: &Path, expected: ExpectedKind) -> Result<OpenedObject, ()> {
        use std::fs::OpenOptions;
        use std::mem::MaybeUninit;
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ,
        };

        let before = std::fs::symlink_metadata(path).map_err(|_| ())?;
        if before.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(());
        }
        match expected {
            ExpectedKind::AncestorDirectory | ExpectedKind::SearchDirectory if !before.is_dir() => {
                return Err(())
            }
            ExpectedKind::RegularFile if !before.is_file() => return Err(()),
            _ => {}
        }

        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .map_err(|_| ())?;
        let after = file.metadata().map_err(|_| ())?;
        if after.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(());
        }
        match expected {
            ExpectedKind::AncestorDirectory | ExpectedKind::SearchDirectory
                if after.file_attributes() & FILE_ATTRIBUTE_DIRECTORY == 0 =>
            {
                return Err(())
            }
            ExpectedKind::RegularFile
                if after.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 =>
            {
                return Err(())
            }
            _ => {}
        }

        let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: `file` owns a valid kernel handle and output points to correctly sized storage.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) }
            == 0
        {
            return Err(());
        }
        // SAFETY: the successful API call initialized the complete structure.
        let information = unsafe { information.assume_init() };
        let identity = FileIdentity {
            first: u64::from(information.dwVolumeSerialNumber),
            second: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        };
        Ok(OpenedObject { file, identity })
    }

    #[cfg(windows)]
    pub(super) fn verify_trust(
        _path: &Path,
        file: &File,
        expected: ExpectedKind,
    ) -> Result<(), ()> {
        use std::ffi::c_void;
        use std::os::windows::io::AsRawHandle;
        use std::ptr;
        use windows_sys::Win32::Foundation::{LocalFree, GENERIC_ALL, GENERIC_WRITE};
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSidToSidW, GetSecurityInfo, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::{
            GetAce, IsValidSid, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
            OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
            FILE_WRITE_EA, WRITE_DAC, WRITE_OWNER,
        };

        struct LocalAllocation(*mut c_void);
        impl Drop for LocalAllocation {
            fn drop(&mut self) {
                // SAFETY: the pointer is returned by a Win32 LocalAlloc-family API.
                unsafe { LocalFree(self.0) };
            }
        }

        let mut owner: PSID = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `file` owns the exact identity-checked object and all output pointers refer to
        // writable local slots. The descriptor is released with `LocalFree` below.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 || descriptor.is_null() || owner.is_null() || dacl.is_null() {
            return Err(());
        }
        let _descriptor = LocalAllocation(descriptor);
        if unsafe { IsValidSid(owner) } == 0 || !is_trusted_sid(owner) {
            return Err(());
        }

        let dangerous = match expected {
            // Creating a sibling does not replace an already-open exact child. Every ancestor
            // itself must still deny deletion and security-descriptor takeover.
            ExpectedKind::AncestorDirectory => {
                GENERIC_ALL | DELETE | FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER
            }
            // The application and codec directories participate in DLL resolution. Untrusted
            // principals must not be able to add a candidate library there.
            ExpectedKind::SearchDirectory => {
                GENERIC_ALL
                    | GENERIC_WRITE
                    | DELETE
                    | FILE_WRITE_DATA
                    | FILE_APPEND_DATA
                    | FILE_WRITE_EA
                    | FILE_WRITE_ATTRIBUTES
                    | FILE_DELETE_CHILD
                    | WRITE_DAC
                    | WRITE_OWNER
            }
            ExpectedKind::RegularFile => {
                GENERIC_ALL
                    | GENERIC_WRITE
                    | DELETE
                    | FILE_WRITE_DATA
                    | FILE_APPEND_DATA
                    | FILE_WRITE_EA
                    | FILE_WRITE_ATTRIBUTES
                    | WRITE_DAC
                    | WRITE_OWNER
            }
        };
        // SAFETY: `dacl` belongs to the live security descriptor.
        let ace_count = unsafe { (*dacl).AceCount };
        for index in 0..u32::from(ace_count) {
            let mut ace = ptr::null_mut();
            // SAFETY: descriptor lifetime covers the DACL and `GetAce` validates the index.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
                return Err(());
            }
            // SAFETY: a successful `GetAce` returns at least an ACE header.
            let header = unsafe { ptr::read_unaligned(ace.cast::<ACE_HEADER>()) };
            const INHERIT_ONLY_ACE: u8 = 0x08;
            if header.AceFlags & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            let ace_bytes = header.AceSize as usize;
            if ace_bytes < 8 {
                return Err(());
            }
            // All supported allow ACE layouts place the access mask at byte offset four.
            let mask = unsafe { ptr::read_unaligned(ace.cast::<u8>().add(4).cast::<u32>()) };
            if mask & dangerous == 0 {
                continue;
            }

            let sid_offset = match header.AceType {
                0 | 9 => 8, // ACCESS_ALLOWED_ACE / ACCESS_ALLOWED_CALLBACK_ACE
                5 | 11 => {
                    if ace_bytes < 12 {
                        return Err(());
                    }
                    // ACCESS_ALLOWED_OBJECT_ACE has optional object GUIDs before SidStart.
                    let flags =
                        unsafe { ptr::read_unaligned(ace.cast::<u8>().add(8).cast::<u32>()) };
                    12 + if flags & 1 != 0 { 16 } else { 0 } + if flags & 2 != 0 { 16 } else { 0 }
                }
                _ => return Err(()),
            };
            if sid_offset >= ace_bytes {
                return Err(());
            }
            let sid = unsafe { ace.cast::<u8>().add(sid_offset).cast::<c_void>() };
            // SAFETY: the SID stays within a descriptor-owned ACE; validity is checked first.
            if unsafe { IsValidSid(sid) } == 0 || !is_trusted_sid(sid) {
                return Err(());
            }
        }

        fn is_trusted_sid(sid: PSID) -> bool {
            use windows_sys::Win32::Foundation::LocalFree;
            use windows_sys::Win32::Security::{
                EqualSid, IsWellKnownSid, WinBuiltinAdministratorsSid, WinLocalSystemSid,
            };

            // SAFETY: callers first obtain the SID from a valid security descriptor or validate
            // it with `IsValidSid`.
            if unsafe { IsWellKnownSid(sid, WinLocalSystemSid) } != 0
                || unsafe { IsWellKnownSid(sid, WinBuiltinAdministratorsSid) } != 0
            {
                return true;
            }

            let trusted_installer =
                "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464\0"
                    .encode_utf16()
                    .collect::<Vec<_>>();
            let mut trusted_installer_sid: PSID = std::ptr::null_mut();
            // SAFETY: input is NUL-terminated and output points to a writable pointer slot.
            if unsafe {
                ConvertStringSidToSidW(trusted_installer.as_ptr(), &mut trusted_installer_sid)
            } == 0
                || trusted_installer_sid.is_null()
            {
                return false;
            }
            // SAFETY: both inputs are valid SIDs for the duration of this call.
            let equal = unsafe { EqualSid(sid, trusted_installer_sid) } != 0;
            // SAFETY: `ConvertStringSidToSidW` returns LocalAlloc-owned memory.
            unsafe { LocalFree(trusted_installer_sid) };
            equal
        }

        Ok(())
    }

    #[cfg(not(windows))]
    pub(super) fn open_verified(_path: &Path, _expected: ExpectedKind) -> Result<OpenedObject, ()> {
        Err(())
    }

    #[cfg(not(windows))]
    pub(super) fn loader_path(_path: std::path::PathBuf) -> Result<std::path::PathBuf, ()> {
        Err(())
    }

    #[cfg(not(windows))]
    pub(super) fn verify_trust(
        _path: &Path,
        _file: &File,
        _expected: ExpectedKind,
    ) -> Result<(), ()> {
        Err(())
    }
}
