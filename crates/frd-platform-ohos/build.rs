fn main() {
    println!("cargo:rerun-if-changed=src/huks_native.c");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("ohos") {
        cc::Build::new()
            .file("src/huks_native.c")
            .warnings_into_errors(true)
            .compile("frd_huks");
        println!("cargo:rustc-link-lib=huks_ndk.z");
        println!("cargo:rustc-link-lib=ohcrypto");
    }
}
