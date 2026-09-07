//! Linux 随包 FFmpeg 动态库的受信路径检查。
//!
//! Linux 没有 Windows 的安装器 ACL 链或 macOS 的代码签名，因此生产加载只接受：
//! 所有路径组件均为真实目录/文件、codec 目录位于当前 executable 目录下、对象由同一
//! 安装所有者拥有，并且没有 group/other 写权限。加载器随后按固定顺序把依赖以
//! `RTLD_GLOBAL` 打开，再以 `RTLD_LOCAL` 打开插件，使随包 SONAME 依赖不会回退到系统库。

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

pub(crate) struct TrustedBundle {
    pub(crate) dependencies: Vec<PathBuf>,
    pub(crate) plugin: PathBuf,
}

pub(crate) fn prepare(
    executable: &Path,
    platform: &str,
    dependencies: &[&str],
    plugin: &str,
) -> Result<TrustedBundle, ()> {
    if !executable.is_absolute()
        || !single_name(platform)
        || !single_name(plugin)
        || dependencies.iter().any(|name| !single_name(name))
    {
        return Err(());
    }
    reject_symlink_components(executable)?;
    let executable_metadata = fs::symlink_metadata(executable).map_err(|_| ())?;
    if !executable_metadata.is_file() {
        return Err(());
    }
    let application_dir = executable.parent().ok_or(())?;
    let canonical_application_dir = application_dir.canonicalize().map_err(|_| ())?;
    let owner = fs::symlink_metadata(&canonical_application_dir)
        .map_err(|_| ())?
        .uid();
    verify_object(&canonical_application_dir, true, owner)?;

    let codecs_dir = canonical_application_dir.join("codecs");
    let version_dir = codecs_dir.join("ffmpeg-8.1.2");
    let platform_dir = version_dir.join(platform);
    for directory in [&codecs_dir, &version_dir, &platform_dir] {
        reject_symlink_components(directory)?;
        verify_object(directory, true, owner)?;
    }

    checked_file(executable, &canonical_application_dir, owner)?;
    let dependencies = dependencies
        .iter()
        .map(|name| checked_file(&platform_dir.join(name), &platform_dir, owner))
        .collect::<Result<Vec<_>, _>>()?;
    let plugin = checked_file(&platform_dir.join(plugin), &platform_dir, owner)?;

    Ok(TrustedBundle {
        dependencies,
        plugin,
    })
}

fn checked_file(path: &Path, parent: &Path, owner: u32) -> Result<PathBuf, ()> {
    reject_symlink_components(path)?;
    let canonical = path.canonicalize().map_err(|_| ())?;
    if canonical.parent() != Some(parent) {
        return Err(());
    }
    verify_object(&canonical, false, owner)?;
    Ok(canonical)
}

fn verify_object(path: &Path, directory: bool, owner: u32) -> Result<(), ()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.file_type().is_symlink()
        || metadata.uid() != owner
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(());
    }
    if directory != metadata.is_dir() || (!directory && !metadata.is_file()) {
        return Err(());
    }
    Ok(())
}

fn reject_symlink_components(path: &Path) -> Result<(), ()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(());
    }
    let mut cursor = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => cursor.push(Path::new("/")),
            Component::Normal(name) => cursor.push(name),
            _ => return Err(()),
        }
        if fs::symlink_metadata(&cursor)
            .map_err(|_| ())?
            .file_type()
            .is_symlink()
        {
            return Err(());
        }
    }
    Ok(())
}

fn single_name(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        executable: PathBuf,
        platform: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "frd-linux-bundle-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let application = root.join("app");
            let executable = application.join("freeremotedesk");
            let platform = application.join("codecs/ffmpeg-8.1.2/linux-test");
            fs::create_dir_all(&platform).unwrap();
            fs::copy("/usr/bin/true", &executable).unwrap();
            fs::copy("/usr/bin/true", platform.join("libavutil.so.60")).unwrap();
            fs::copy("/usr/bin/true", platform.join("libavcodec.so.62")).unwrap();
            fs::copy(
                "/usr/bin/true",
                platform.join("libfreeremotedesk_ffmpeg.so"),
            )
            .unwrap();
            Self {
                root,
                executable,
                platform,
            }
        }

        fn prepare(&self) -> Result<TrustedBundle, ()> {
            prepare(
                &self.executable,
                "linux-test",
                &["libavutil.so.60", "libavcodec.so.62"],
                "libfreeremotedesk_ffmpeg.so",
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn accepts_owned_non_group_writable_bundle() {
        let fixture = Fixture::new();
        let bundle = fixture.prepare().expect("安全 Linux bundle 应通过");
        assert_eq!(bundle.dependencies.len(), 2);
        assert_eq!(
            bundle.plugin,
            fixture.platform.join("libfreeremotedesk_ffmpeg.so")
        );
    }

    #[test]
    fn rejects_group_writable_plugin() {
        let fixture = Fixture::new();
        let plugin = fixture.platform.join("libfreeremotedesk_ffmpeg.so");
        let mut permissions = fs::metadata(&plugin).unwrap().permissions();
        permissions.set_mode(0o664);
        fs::set_permissions(&plugin, permissions).unwrap();
        assert!(fixture.prepare().is_err());
    }

    #[test]
    fn rejects_symlinked_plugin() {
        let fixture = Fixture::new();
        let plugin = fixture.platform.join("libfreeremotedesk_ffmpeg.so");
        let outside = fixture.root.join("outside.so");
        fs::copy("/usr/bin/true", &outside).unwrap();
        fs::remove_file(&plugin).unwrap();
        std::os::unix::fs::symlink(&outside, &plugin).unwrap();
        assert!(fixture.prepare().is_err());
    }
}
