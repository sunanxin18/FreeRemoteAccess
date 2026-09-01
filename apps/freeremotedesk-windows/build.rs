use std::{fs, path::Path};

const PACKAGE_MANIFEST: &str = "../../packaging/windows/ffmpeg-manifest.json";
const PACKAGE_STAGE_SCRIPT: &str = "../../tools/stage-windows-package.ps1";
const PACKAGE_VERIFY_SCRIPT: &str = "../../tools/verify-windows-package.ps1";
const CODEC_DIRECTORY: &str = "codecs/ffmpeg-8.1.2/windows-x86_64";

fn assert_optional_codec_package_contract() {
    let text = fs::read_to_string(PACKAGE_MANIFEST)
        .expect("Windows FFmpeg package manifest 必须存在且可读");
    let manifest: serde_json::Value =
        serde_json::from_str(&text).expect("Windows FFmpeg package manifest 必须是有效 JSON");

    assert_eq!(
        manifest["schema"], "freeremotedesk.windows.ffmpeg-package.v1",
        "Windows FFmpeg package manifest schema 不匹配"
    );
    assert_eq!(
        manifest["codecDirectory"], CODEC_DIRECTORY,
        "Windows FFmpeg package manifest codec 目录不得偏离 loader 合约"
    );
    assert_eq!(
        manifest["libavcodecMajor"], 62,
        "Windows FFmpeg package manifest 必须固定 libavcodec major 62"
    );
}

fn main() {
    for package_input in [
        PACKAGE_MANIFEST,
        PACKAGE_STAGE_SCRIPT,
        PACKAGE_VERIFY_SCRIPT,
    ] {
        println!("cargo:rerun-if-changed={package_input}");
        assert!(
            Path::new(package_input).exists(),
            "打包输入不存在: {package_input}"
        );
    }
    assert_optional_codec_package_contract();

    let icon = "../../assets/app-icon/windows/freeremotedesk.ico";
    println!("cargo:rerun-if-changed={icon}");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(icon);
        resource
            .compile()
            .expect("Windows 应用图标资源必须存在且格式有效");
    }
}
