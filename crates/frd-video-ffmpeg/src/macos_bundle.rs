//! macOS 本地签名应用包内的固定 FFmpeg 插件路径。
use std::path::{Path, PathBuf};

pub(crate) struct TrustedBundle {
    pub dependencies: Vec<PathBuf>,
    pub plugin: PathBuf,
}

pub(crate) fn prepare(
    executable: &Path,
    platform: &str,
    dependencies: &[&str],
    plugin: &str,
) -> Result<TrustedBundle, ()> {
    use std::fs;
    use std::process::{Command, Stdio};
    if !executable.is_absolute()
        || !single_name(platform)
        || !single_name(plugin)
        || dependencies.iter().any(|name| !single_name(name))
    {
        return Err(());
    }
    let application_dir = executable.parent().ok_or(())?;
    let contents = application_dir.parent().ok_or(())?;
    let app = contents.parent().ok_or(())?;
    if application_dir.file_name() != Some(std::ffi::OsStr::new("MacOS"))
        || contents.file_name() != Some(std::ffi::OsStr::new("Contents"))
        || app.extension() != Some(std::ffi::OsStr::new("app"))
    {
        return Err(());
    }
    let canonical_app = app.canonicalize().map_err(|_| ())?;
    let codecs = application_dir.join("codecs/ffmpeg-8-1-2").join(platform);
    let checked_path = |path: &Path, file: bool| -> Result<PathBuf, ()> {
        let relative = path.strip_prefix(app).map_err(|_| ())?;
        let mut cursor = app.to_path_buf();
        if fs::symlink_metadata(&cursor)
            .map_err(|_| ())?
            .file_type()
            .is_symlink()
        {
            return Err(());
        }
        for component in relative.components() {
            if !matches!(component, std::path::Component::Normal(_)) {
                return Err(());
            }
            cursor.push(component);
            if fs::symlink_metadata(&cursor)
                .map_err(|_| ())?
                .file_type()
                .is_symlink()
            {
                return Err(());
            }
        }
        let canonical = path.canonicalize().map_err(|_| ())?;
        if !canonical.starts_with(&canonical_app) || (file && !canonical.is_file()) {
            return Err(());
        }
        Ok(canonical)
    };
    checked_path(executable, true)?;
    checked_path(&contents.join("Info.plist"), true)?;
    let dependencies = dependencies
        .iter()
        .map(|name| checked_path(&codecs.join(name), true))
        .collect::<Result<Vec<_>, _>>()?;
    let plugin = checked_path(&codecs.join(plugin), true)?;
    // ad-hoc 签名只证明此本地包的资源完整性，不证明开发者身份或 Apple 公证。
    let status = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&canonical_app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| ())?;
    if !status.success() {
        return Err(());
    }
    Ok(TrustedBundle {
        dependencies,
        plugin,
    })
}

fn single_name(value: &str) -> bool {
    let mut parts = Path::new(value).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::{fs, process::Command};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        executable: PathBuf,
        codecs: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "frd-macos-bundle-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let app = root.join("Fixture.app");
            let executable = app.join("Contents/MacOS/fixture");
            let codecs = app.join("Contents/MacOS/codecs/ffmpeg-8-1-2/macos-test");
            fs::create_dir_all(&codecs).unwrap();
            fs::copy("/usr/bin/true", &executable).unwrap();
            fs::write(app.join("Contents/Info.plist"), r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>fixture</string><key>CFBundleIdentifier</key><string>org.freeremotedesk.fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
            fs::copy("/usr/bin/true", codecs.join("dependency.dylib")).unwrap();
            fs::copy("/usr/bin/true", codecs.join("plugin.dylib")).unwrap();
            Self {
                root,
                executable,
                codecs,
            }
        }
        fn sign(&self) {
            let output = Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(self.root.join("Fixture.app"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fn prepare(&self) -> Result<TrustedBundle, ()> {
            prepare(
                &self.executable,
                "macos-test",
                &["dependency.dylib"],
                "plugin.dylib",
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn locally_signed_bundle_accepts_only_absolute_internal_paths() {
        let f = Fixture::new();
        f.sign();
        let trusted = f.prepare().expect("本地 ad-hoc 包应通过完整性校验");
        assert_eq!(
            trusted.plugin,
            f.codecs.join("plugin.dylib").canonicalize().unwrap()
        );
        assert_eq!(trusted.dependencies.len(), 1);
    }
    #[test]
    fn unsigned_or_modified_bundle_is_rejected() {
        let f = Fixture::new();
        assert!(f.prepare().is_err());
        f.sign();
        fs::write(f.codecs.join("plugin.dylib"), b"tampered").unwrap();
        assert!(f.prepare().is_err());
    }
    #[test]
    fn symbolic_link_plugin_is_rejected_even_if_the_bundle_is_signed() {
        let f = Fixture::new();
        let outside = f.root.join("outside.dylib");
        fs::copy("/usr/bin/true", &outside).unwrap();
        fs::remove_file(f.codecs.join("plugin.dylib")).unwrap();
        std::os::unix::fs::symlink(outside, f.codecs.join("plugin.dylib")).unwrap();
        f.sign();
        assert!(f.prepare().is_err());
    }
    #[test]
    fn bare_executable_and_directory_escape_are_rejected() {
        assert!(prepare(
            Path::new("/usr/bin/true"),
            "macos-test",
            &[],
            "plugin.dylib"
        )
        .is_err());
        let f = Fixture::new();
        f.sign();
        assert!(prepare(&f.executable, "../macos-test", &[], "plugin.dylib").is_err());
    }
}
