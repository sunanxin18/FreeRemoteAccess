fn main() {
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
