use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src/ffmpeg_bridge.c");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    if env::var_os("CARGO_FEATURE_NATIVE_FFMPEG").is_none() {
        return;
    }
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

    let mut bridge = cc::Build::new();
    bridge.file(&source).include(&include).warnings(true);
    if target_os == "windows" {
        bridge.flag_if_supported("/std:c11");
    } else {
        bridge.flag_if_supported("-std=c11");
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
