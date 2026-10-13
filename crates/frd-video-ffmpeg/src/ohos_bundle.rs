//! OHOS 自身模块邻接库加载；仅信任公开应用上下文报告的不可写安装代码。
//!
//! 路径与权限合同故意保守；正常签名 HAP 的实际沙箱/namespace 仍需独立验收。
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use frd_media_api::{VideoDecodeError, VideoDecodeErrorCode};
use libloading::Library;

use crate::abi::{FrdGetFfmpegApiV1, FrdStatus, RawFrdFfmpegApiV1, FRD_FFMPEG_API_SYMBOL};

const ANCHOR_NAME: &str = "libfrd_shell_ohos_ffi.so";
const NAMES: [&str; 3] = [
    "libavutil.so.60",
    "libavcodec.so.62",
    "libfreeremotedesk_ffmpeg.so",
];

// 独特私有数据避免 PLT、可抢占导出符号或链接器函数合并改变锚点。
#[used]
static MODULE_MARKER: [u8; 34] = *b"frd.ohos.ffmpeg.module.anchor.v1!!";

#[cfg(target_env = "ohos")]
#[link(name = "ability_runtime")]
extern "C" {
    fn OH_AbilityRuntime_ApplicationContextGetBundleCodeDir(
        buffer: *mut libc::c_char,
        buffer_size: i32,
        write_length: *mut i32,
    ) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    Origin,
    Path,
    Permission,
    Identity,
    Symbol,
    Library,
    Abi,
}

impl From<Failure> for VideoDecodeError {
    fn from(failure: Failure) -> Self {
        VideoDecodeError::new(if failure == Failure::Abi {
            VideoDecodeErrorCode::BackendVersionMismatch
        } else {
            VideoDecodeErrorCode::BackendUnavailable
        })
    }
}

type Result<T> = std::result::Result<T, Failure>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

struct Lease {
    path: PathBuf,
    file: File,
    identity: Identity,
    directory: bool,
}

impl Lease {
    fn verify(&self) -> Result<()> {
        let path = fs::symlink_metadata(&self.path).map_err(|_| Failure::Identity)?;
        let opened = self.file.metadata().map_err(|_| Failure::Identity)?;
        if path.file_type().is_symlink()
            || path.is_dir() != self.directory
            || (!self.directory && !path.is_file())
            || Identity::of(&path) != self.identity
            || Identity::of(&opened) != self.identity
        {
            return Err(Failure::Identity);
        }
        verify_permissions(&self.path, &path)
    }
}

/// decoder 与接收缓冲区保护器通过 LoadedPlugin 的 Arc 共同持有此资源。
/// 卸载顺序是 plugin → codec → util，随后才释放路径身份租约。
pub(crate) struct Resources {
    libraries: ReverseLibraries<Library>,
    leases: Vec<Lease>,
}

struct ReverseLibraries<T>(Vec<T>);

impl<T> Drop for ReverseLibraries<T> {
    fn drop(&mut self) {
        while let Some(library) = self.0.pop() {
            drop(library);
        }
    }
}

impl Resources {
    fn verify(&self) -> Result<()> {
        for lease in &self.leases {
            lease.verify()?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Image {
    path: PathBuf,
    base: usize,
}

fn image_at(address: usize) -> Result<Image> {
    if address == 0 {
        return Err(Failure::Symbol);
    }
    // 安全：dladdr 只查询地址，不解引用调用方地址；输出由 libc 初始化。
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    if unsafe { libc::dladdr(address as *const libc::c_void, &mut info) } == 0
        || info.dli_fname.is_null()
        || info.dli_fbase.is_null()
    {
        return Err(Failure::Symbol);
    }
    // 安全：成功的 dladdr 返回映射持有期间有效的 NUL 结尾名称。
    let path = unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes();
    let path = PathBuf::from(std::ffi::OsString::from_vec(path.to_vec()));
    clean_absolute(&path)?;
    Ok(Image {
        path,
        base: info.dli_fbase as usize,
    })
}

fn clean_absolute(path: &Path) -> Result<()> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.contains(&0)
        || bytes.contains(&b'!')
        || bytes[1..]
            .split(|b| *b == b'/')
            .any(|part| part.is_empty() || part == b"." || part == b".." || part.ends_with(b".hap"))
    {
        return Err(Failure::Path);
    }
    Ok(())
}

fn code_path_from_reply(status: i32, length: i32, buffer: &[u8]) -> Result<PathBuf> {
    let length = usize::try_from(length).map_err(|_| Failure::Origin)?;
    if status != 0
        || length == 0
        || length >= buffer.len()
        || buffer[length] != 0
        || buffer[..length].contains(&0)
    {
        return Err(Failure::Origin);
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(buffer[..length].to_vec()));
    clean_absolute(&path)?;
    if path == Path::new("/") {
        return Err(Failure::Origin);
    }
    Ok(path)
}

#[cfg(target_env = "ohos")]
fn application_code_dir() -> Result<PathBuf> {
    // 使用非零哨兵要求系统实际写入终止符，不把预先清零误认为完整输出。
    let mut buffer = [0xa5_u8; 4096];
    let mut length = -1;
    // 安全：公开 API16+ 合同只写入 buffer_size 字节及一个 i32 长度。
    let status = unsafe {
        OH_AbilityRuntime_ApplicationContextGetBundleCodeDir(
            buffer.as_mut_ptr().cast(),
            buffer.len() as i32,
            &mut length,
        )
    };
    code_path_from_reply(status, length, &buffer)
}

fn neighbor_paths(code: &Path, anchor: &Path) -> Result<[PathBuf; 3]> {
    clean_absolute(code)?;
    clean_absolute(anchor)?;
    let parent = anchor.parent().ok_or(Failure::Path)?;
    if code == Path::new("/")
        || anchor.file_name() != Some(std::ffi::OsStr::new(ANCHOR_NAME))
        || !parent.starts_with(code)
        || parent == code
    {
        return Err(Failure::Origin);
    }
    Ok(NAMES.map(|name| parent.join(name)))
}

fn permission_facts(mode: u32, owner: u32, effective_uid: u32, writable: bool) -> Result<()> {
    // 安装链不能由应用拥有（即使当前只读，owner 仍可能 chmod），也不能开放组/其他写入。
    if mode & 0o022 != 0 || owner == effective_uid || writable {
        Err(Failure::Permission)
    } else {
        Ok(())
    }
}

fn verify_permissions(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    let cpath = CString::new(path.as_os_str().as_bytes()).map_err(|_| Failure::Path)?;
    // 安全：AT_EACCESS 显式采用有效身份；权限未知/不支持均失败关闭。
    let access =
        unsafe { libc::faccessat(libc::AT_FDCWD, cpath.as_ptr(), libc::W_OK, libc::AT_EACCESS) };
    let writable = if access == 0 {
        true
    } else {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EACCES | libc::EROFS) => false,
            _ => return Err(Failure::Permission),
        }
    };
    // 安全：geteuid 没有参数或内存前提。
    permission_facts(
        metadata.mode(),
        metadata.uid(),
        unsafe { libc::geteuid() },
        writable,
    )
}

fn open_lease(path: &Path, directory: bool) -> Result<Lease> {
    let before = fs::symlink_metadata(path).map_err(|_| Failure::Path)?;
    if before.file_type().is_symlink()
        || before.is_dir() != directory
        || (!directory && !before.is_file())
    {
        return Err(Failure::Path);
    }
    verify_permissions(path, &before)?;
    let flags = libc::O_NOFOLLOW | libc::O_CLOEXEC | if directory { libc::O_DIRECTORY } else { 0 };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(|_| Failure::Path)?;
    let lease = Lease {
        path: path.to_owned(),
        file,
        identity: Identity::of(&before),
        directory,
    };
    lease.verify()?;
    Ok(lease)
}

fn prepare(code: &Path, anchor: &Path) -> Result<(Resources, [PathBuf; 3])> {
    let paths = neighbor_paths(code, anchor)?;
    let parent = anchor.parent().ok_or(Failure::Path)?;
    let mut resources = Resources {
        libraries: ReverseLibraries(Vec::new()),
        leases: Vec::new(),
    };
    // 从根到 native 目录逐级先核验再打开；已验证的父目录不可由应用替换。
    let mut ancestors = parent.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for directory in ancestors {
        resources.leases.push(open_lease(directory, true)?);
    }
    for path in std::iter::once(anchor).chain(paths.iter().map(PathBuf::as_path)) {
        resources.leases.push(open_lease(path, false)?);
    }
    resources.verify()?;
    Ok((resources, paths))
}

fn same_image(expected: &Image, actual: &Image) -> Result<()> {
    if expected.base == 0 || expected != actual {
        Err(Failure::Symbol)
    } else {
        Ok(())
    }
}

fn symbol_image(library: &Library, name: &[u8], expected_path: &Path) -> Result<(usize, Image)> {
    // 安全：先读取裸地址，不在确认 image 身份前构造可调用函数指针。
    let symbol =
        unsafe { library.get::<*const libc::c_void>(name) }.map_err(|_| Failure::Symbol)?;
    let address = *symbol as usize;
    function_address(address)?;
    let image = image_at(address)?;
    if image.path != expected_path {
        return Err(Failure::Symbol);
    }
    Ok((address, image))
}

fn function_address(address: usize) -> Result<()> {
    // 首版仅支持 AArch64；函数入口必须满足指令的四字节对齐。
    if address == 0 || address % 4 != 0 {
        Err(Failure::Symbol)
    } else {
        Ok(())
    }
}

fn checked_callbacks(
    raw: &RawFrdFfmpegApiV1,
    mut check: impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    crate::loader::validate_raw_api_header(raw).map_err(|_| Failure::Abi)?;
    for address in [
        raw.create_decoder,
        raw.submit,
        raw.receive,
        raw.flush,
        raw.destroy,
        raw.reclaim_frame,
    ] {
        function_address(address)?;
        check(address)?;
    }
    Ok(())
}

fn load_in_order<T>(
    paths: &[PathBuf; 3],
    mut open: impl FnMut(&Path) -> Result<T>,
) -> Result<ReverseLibraries<T>> {
    let mut loaded = ReverseLibraries(Vec::with_capacity(3));
    for path in paths {
        loaded.0.push(open(path)?);
    }
    Ok(loaded)
}

#[cfg(target_env = "ohos")]
pub(crate) fn load() -> std::result::Result<(RawFrdFfmpegApiV1, Resources), VideoDecodeError> {
    load_from_context(application_code_dir()?).map_err(Into::into)
}

fn load_from_context(code: PathBuf) -> Result<(RawFrdFfmpegApiV1, Resources)> {
    let marker_address = MODULE_MARKER.as_ptr() as usize;
    let anchor = image_at(marker_address)?;
    let (mut resources, paths) = prepare(&code, &anchor.path)?;
    resources.libraries = load_in_order(&paths, |path| {
        resources.verify()?;
        use libloading::os::unix::{Library as UnixLibrary, RTLD_LOCAL, RTLD_NOW};
        // 安全：固定邻接普通文件、完整不可写安装链和保留身份均已验证；仅使用当前 app namespace。
        unsafe { UnixLibrary::open(Some(path), RTLD_NOW | RTLD_LOCAL) }
            .map(Into::into)
            .map_err(|_| Failure::Library)
    })?;
    resources.verify()?;
    same_image(&anchor, &image_at(marker_address)?)?;
    let (util_version, _) =
        symbol_image(&resources.libraries.0[0], b"avutil_version\0", &paths[0])?;
    let (codec_version, _) =
        symbol_image(&resources.libraries.0[1], b"avcodec_version\0", &paths[1])?;
    let (getter, plugin_image) =
        symbol_image(&resources.libraries.0[2], FRD_FFMPEG_API_SYMBOL, &paths[2])?;
    resources.verify()?;
    // 安全：三个地址均来自对应、已保留且绑定未改变的安装 ELF；函数签名是固定公开版本合同。
    let (status, raw) = unsafe {
        let util: unsafe extern "C" fn() -> u32 = std::mem::transmute(util_version);
        let codec: unsafe extern "C" fn() -> u32 = std::mem::transmute(codec_version);
        let get: FrdGetFfmpegApiV1 = std::mem::transmute(getter);
        let mut raw = RawFrdFfmpegApiV1::default();
        let util_major = util() >> 16;
        let codec_major = codec() >> 16;
        if util_major != 60 || codec_major != 62 {
            return Err(Failure::Abi);
        }
        let status = get(&mut raw, std::mem::size_of::<RawFrdFfmpegApiV1>());
        (status, raw)
    };
    if status != FrdStatus::OK {
        return Err(Failure::Abi);
    }
    checked_callbacks(&raw, |address| {
        same_image(&plugin_image, &image_at(address)?)
    })?;
    resources.verify()?;
    same_image(&anchor, &image_at(marker_address)?)?;
    Ok((raw, resources))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::*;
    use std::sync::{Arc, Mutex};

    fn raw() -> RawFrdFfmpegApiV1 {
        RawFrdFfmpegApiV1 {
            struct_size: FRD_FFMPEG_API_V1_SIZE,
            struct_alignment: FRD_FFMPEG_API_V1_ALIGNMENT,
            abi_version: FRD_FFMPEG_ABI_VERSION,
            avcodec_major: FRD_FFMPEG_AVCODEC_MAJOR,
            contract_flags: FRD_API_CONTRACT_REQUIRED,
            codec_capabilities: FRD_CODEC_CAP_H264_AVC420,
            create_decoder: 4,
            submit: 8,
            receive: 12,
            flush: 16,
            destroy: 20,
            reclaim_frame: 24,
        }
    }

    #[test]
    fn context_reply_requires_success_exact_length_and_written_nul() {
        let path = b"/os/code\0";
        assert_eq!(
            code_path_from_reply(0, 8, path).unwrap(),
            Path::new("/os/code")
        );
        for (status, length, bytes) in [
            (1, 8, path.as_slice()),
            (0, -1, path),
            (0, 0, path),
            (0, 9, path),
            (0, 7, path),
            (0, 8, b"/os/code!"),
            (0, 8, b"/os\0code\0"),
        ] {
            assert!(code_path_from_reply(status, length, bytes).is_err());
        }
    }

    #[test]
    fn anchor_requires_clean_absolute_ordinary_path_and_exact_module() {
        for path in [
            "relative/libfrd_shell_ohos_ffi.so",
            "/os/code/./libfrd_shell_ohos_ffi.so",
            "/os/code/../libfrd_shell_ohos_ffi.so",
            "/os//code/libfrd_shell_ohos_ffi.so",
            "/os/code/app.hap/libs/libfrd_shell_ohos_ffi.so",
            "/os/code/app.hap!/libfrd_shell_ohos_ffi.so",
            "/os/code/libs/other.so",
            "/elsewhere/libfrd_shell_ohos_ffi.so",
            "/os/code2/libs/libfrd_shell_ohos_ffi.so",
            "/os/code/libfrd_shell_ohos_ffi.so",
        ] {
            assert!(
                neighbor_paths(Path::new("/os/code"), Path::new(path)).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn os_code_origin_selects_only_fixed_same_parent_neighbors() {
        let paths = neighbor_paths(
            Path::new("/os/code"),
            Path::new("/os/code/libs/arm64/libfrd_shell_ohos_ffi.so"),
        )
        .unwrap();
        for (path, name) in paths.iter().zip(NAMES) {
            assert_eq!(path.parent(), Some(Path::new("/os/code/libs/arm64")));
            assert_eq!(path.file_name().unwrap(), name);
        }
    }

    #[test]
    fn mocked_install_facts_reject_app_owner_group_other_and_effective_write() {
        assert!(permission_facts(0o755, 0, 20020074, false).is_ok());
        for (mode, owner, writable) in [
            (0o775, 0, false),
            (0o757, 0, false),
            (0o555, 20020074, false),
            (0o555, 0, true),
        ] {
            assert_eq!(
                permission_facts(mode, owner, 20020074, writable),
                Err(Failure::Permission)
            );
        }
    }

    #[test]
    fn loaded_symbol_rejects_wrong_path_or_mapping_base() {
        let image = Image {
            path: PathBuf::from("/os/code/libs/plugin.so"),
            base: 4096,
        };
        assert!(same_image(&image, &image).is_ok());
        for actual in [
            Image {
                path: image.path.clone(),
                base: 8192,
            },
            Image {
                path: PathBuf::from("/outside/plugin.so"),
                base: 4096,
            },
        ] {
            assert_eq!(same_image(&image, &actual), Err(Failure::Symbol));
        }
        assert!(image_at(0).is_err());
    }

    #[test]
    fn every_callback_must_belong_to_the_plugin_before_conversion() {
        let api = raw();
        let mut checked = Vec::new();
        checked_callbacks(&api, |address| {
            checked.push(address);
            Ok(())
        })
        .unwrap();
        assert_eq!(checked, [4, 8, 12, 16, 20, 24]);
        for outside in [4, 8, 12, 16, 20, 24] {
            assert_eq!(
                checked_callbacks(&api, |address| if address == outside {
                    Err(Failure::Symbol)
                } else {
                    Ok(())
                }),
                Err(Failure::Symbol)
            );
        }
    }

    #[test]
    fn misaligned_callback_is_rejected_before_image_query() {
        let mut api = raw();
        api.create_decoder = 5;
        assert_eq!(
            checked_callbacks(&api, |_| panic!("不得查询未对齐函数")),
            Err(Failure::Symbol)
        );
    }

    #[test]
    fn invalid_abi_and_null_slots_never_reach_address_verifier() {
        let mut cases = Vec::new();
        for slot in 0..6 {
            let mut api = raw();
            let slots = [
                &mut api.create_decoder,
                &mut api.submit,
                &mut api.receive,
                &mut api.flush,
                &mut api.destroy,
                &mut api.reclaim_frame,
            ];
            *slots.into_iter().nth(slot).unwrap() = 0;
            cases.push(api);
        }
        for field in 0..6 {
            let mut api = raw();
            match field {
                0 => api.struct_size = 0,
                1 => api.struct_alignment = 1,
                2 => api.abi_version += 1,
                3 => api.avcodec_major += 1,
                4 => api.contract_flags = 0,
                _ => api.codec_capabilities = u32::MAX,
            }
            cases.push(api);
        }
        for api in cases {
            assert_eq!(
                checked_callbacks(&api, |_| panic!("ABI 无效不得查询或转换 callback")),
                Err(Failure::Abi)
            );
        }
    }

    struct DropRecord(usize, Arc<Mutex<Vec<usize>>>);
    impl Drop for DropRecord {
        fn drop(&mut self) {
            self.1.lock().unwrap().push(self.0);
        }
    }

    #[test]
    fn partial_dependency_failure_releases_only_loaded_handles_in_reverse_order() {
        let paths = NAMES.map(PathBuf::from);
        for fail_index in 0..3 {
            let dropped = Arc::new(Mutex::new(Vec::new()));
            let mut index = 0;
            let result = load_in_order(&paths, |_| {
                let current = index;
                index += 1;
                if current == fail_index {
                    Err(Failure::Library)
                } else {
                    Ok(DropRecord(current, dropped.clone()))
                }
            });
            assert!(result.is_err());
            assert_eq!(
                *dropped.lock().unwrap(),
                (0..fail_index).rev().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn successful_handle_set_releases_plugin_before_dependencies() {
        let dropped = Arc::new(Mutex::new(Vec::new()));
        let mut index = 0;
        let handles = load_in_order(&NAMES.map(PathBuf::from), |_| {
            index += 1;
            Ok(DropRecord(index, dropped.clone()))
        })
        .unwrap();
        assert!(dropped.lock().unwrap().is_empty());
        drop(handles);
        assert_eq!(*dropped.lock().unwrap(), [3, 2, 1]);
    }

    #[test]
    fn replaced_inode_and_symlink_cannot_satisfy_retained_lease() {
        // 此夹具只验证负例身份，绝不将用户临时目录当作受信安装目录。
        let root = std::env::temp_dir().join(format!("frd-ohos-identity-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("image.so");
        fs::write(&path, b"original").unwrap();
        let file = File::open(&path).unwrap();
        let identity = Identity::of(&file.metadata().unwrap());
        let lease = Lease {
            path: path.clone(),
            file,
            identity,
            directory: false,
        };
        fs::rename(&path, root.join("old.so")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(lease.verify().is_err());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(root.join("old.so"), &path).unwrap();
        assert!(lease.verify().is_err());
        assert!(open_lease(&path, false).is_err());
        assert!(open_lease(&root, true).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symlink_directory_is_rejected_before_opening_descendants() {
        let root = std::env::temp_dir().join(format!("frd-ohos-symlink-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let link = root.join("alias");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(open_lease(&link, true).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn host_module_cannot_masquerade_as_installed_ohos_anchor() {
        assert!(load_from_context(PathBuf::from("/os/code")).is_err());
    }
}
