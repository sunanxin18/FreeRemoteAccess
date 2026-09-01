use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/ffmpeg_bridge.c");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    if env::var_os("CARGO_FEATURE_NATIVE_FFMPEG").is_none() {
        return;
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        panic!("native-ffmpeg 当前只支持 Windows MSVC plugin 构建");
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest.join("src/ffmpeg_bridge.c");
    let ffmpeg =
        PathBuf::from(env::var_os("FFMPEG_DIR").expect("native-ffmpeg 构建必须设置 FFMPEG_DIR"));
    let include = ffmpeg.join("include");
    let lib_dir = ffmpeg.join("lib");
    require_file(&include.join("libavcodec/avcodec.h"));
    require_file(&lib_dir.join("avcodec.lib"));
    require_file(&lib_dir.join("avutil.lib"));

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let object = out_dir.join("ffmpeg_bridge.obj");
    let archive = out_dir.join("frd_ffmpeg_bridge.lib");
    run(
        Command::new("cl.exe")
            .arg("/nologo")
            .arg("/c")
            .arg("/O2")
            .arg("/MD")
            .arg("/std:c11")
            .arg("/W4")
            .arg(format!("/I{}", include.display()))
            .arg(format!("/Fo{}", object.display()))
            .arg(&source),
        "编译 FFmpeg C bridge",
    );
    run(
        Command::new("lib.exe")
            .arg("/nologo")
            .arg(format!("/OUT:{}", archive.display()))
            .arg(&object),
        "归档 FFmpeg C bridge",
    );

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=frd_ffmpeg_bridge");
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

fn run(command: &mut Command, action: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("无法{action}: {error}"));
    assert!(status.success(), "{action}失败: {status}");
}
