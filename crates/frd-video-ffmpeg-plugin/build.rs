use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src/ffmpeg_bridge.c");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    if env::var_os("CARGO_FEATURE_NATIVE_FFMPEG").is_none() {
        return;
    }
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo 必须提供目标操作系统");
    if !matches!(target_os.as_str(), "windows" | "macos" | "linux") {
        panic!("native-ffmpeg 仅支持 Windows MSVC、macOS 和 Linux plugin 构建");
    }
    if target_os == "windows" && env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        panic!("native-ffmpeg 的 Windows plugin 构建必须使用 MSVC");
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest.join("src/ffmpeg_bridge.c");
    let ffmpeg =
        PathBuf::from(env::var_os("FFMPEG_DIR").expect("native-ffmpeg 构建必须设置 FFMPEG_DIR"));
    let include = ffmpeg.join("include");
    let lib_dir = ffmpeg.join("lib");
    require_file(&include.join("libavcodec/avcodec.h"));
    require_directory(&lib_dir);
    if target_os == "windows" {
        require_file(&lib_dir.join("avcodec.lib"));
        require_file(&lib_dir.join("avutil.lib"));
    }

    let ohos_sdk = if target_env == "ohos" {
        assert!(
            target_os == "linux"
                && env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
                && env::var("TARGET").as_deref() == Ok("aarch64-unknown-linux-ohos"),
            "native-ffmpeg OHOS 仅支持 aarch64-unknown-linux-ohos"
        );
        println!("cargo:rerun-if-env-changed=OHOS_SDK_ROOT");
        let sdk = PathBuf::from(
            env::var_os("OHOS_SDK_ROOT").expect("OHOS 必须显式设置官方 OHOS_SDK_ROOT"),
        );
        println!("cargo:rerun-if-env-changed=CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER");
        let linker = PathBuf::from(
            env::var_os("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER")
                .expect("OHOS 必须显式设置官方 target linker"),
        );
        assert_eq!(
            linker.canonicalize().expect("OHOS linker 必须存在"),
            sdk.join("native/llvm/bin/aarch64-unknown-linux-ohos-clang")
                .canonicalize()
                .expect("OHOS SDK compiler 必须存在"),
            "OHOS linker 必须使用已验证官方 SDK compiler"
        );
        let root = manifest
            .parent()
            .and_then(Path::parent)
            .expect("plugin 必须位于 workspace crates 目录");
        let verifier = root.join("tools/ohos-ffmpeg-bundle.py");
        println!("cargo:rerun-if-changed={}", verifier.display());
        println!("cargo:rerun-if-changed={}", ffmpeg.display());
        let status = std::process::Command::new("python3")
            .arg(&verifier)
            .arg("--sdk")
            .arg(&sdk)
            .arg("verify-inputs")
            .arg(&ffmpeg)
            .arg("--repo")
            .arg(root)
            .status()
            .expect("OHOS 编译输入验证需要 python3");
        assert!(status.success(), "OHOS FFmpeg SDK/来源/架构验证失败");
        Some(sdk)
    } else {
        None
    };

    let mut bridge = cc::Build::new();
    bridge.file(&source).include(&include).warnings(true);
    if target_os == "windows" {
        bridge.flag_if_supported("/std:c11");
    } else {
        bridge.flag_if_supported("-std=c11");
    }
    if let Some(sdk) = &ohos_sdk {
        // 不接受宿主编译器回退；SDK 输入已由独立 verifier 核对。
        bridge
            .compiler(sdk.join("native/llvm/bin/aarch64-unknown-linux-ohos-clang"))
            .archiver(sdk.join("native/llvm/bin/llvm-ar"))
            .flag(format!("--sysroot={}/native/sysroot", sdk.display()));
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libfreeremotedesk_ffmpeg.so");
    }
    bridge.compile("frd_ffmpeg_bridge");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=avcodec");
    println!("cargo:rustc-link-lib=dylib=avutil");
}

fn require_file(path: &Path) {
    assert!(
        path.is_file(),
        "缺少 native-ffmpeg 构建输入: {}",
        path.display()
    );
}

fn require_directory(path: &Path) {
    assert!(
        path.is_dir(),
        "缺少 native-ffmpeg 库目录: {}",
        path.display()
    );
}
