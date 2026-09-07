#[test]
fn macos_product_uses_its_platform_adapter_and_shared_protocols() {
    let manifest = include_str!("../Cargo.toml");
    assert!(manifest.contains("frd-platform-macos"));
    assert!(manifest.contains("frd-shell-desktop"));
    assert!(manifest.contains("frd-protocol-rdp"));
    assert!(manifest.contains("frd-protocol-apple"));
    assert!(!manifest.contains("frd-platform-windows"));
    assert!(!manifest.contains("windows-sys"));
    let entry = include_str!("../src/main.rs");
    assert!(entry.contains("RdpClientPlatformIdentity::Macintosh"));
}
