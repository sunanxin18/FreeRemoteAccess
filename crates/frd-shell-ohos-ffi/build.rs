fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("ohos") {
        // 供原生 HAP 导入的 ELF 必须有固定 SONAME，避免 CMake 把宿主绝对路径
        // 写入 DT_NEEDED，导致已签名/安装的包在 guest 仍无法加载。
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libfrd_shell_ohos_ffi.so");
    }
}
