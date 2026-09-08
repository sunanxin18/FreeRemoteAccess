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

#[test]
fn macos_production_composition_keeps_egfx_live_gate_closed() {
    let entry = include_str!("../src/main.rs");
    assert_eq!(
        entry
            .matches("RdpProtocolFactory::with_egfx_decoder_provider(")
            .count(),
        1,
        "macOS composition must keep the default legacy-only graphics gate"
    );
    assert!(
        entry.contains("legacy_rdp_factory(platform)"),
        "macOS composition must keep the legacy fallback when the codec bundle is unavailable"
    );
    assert!(
        entry.contains("fn legacy_rdp_factory(platform: RdpClientPlatformIdentity)"),
        "macOS composition must retain an explicit legacy factory boundary"
    );
    assert!(
        entry.contains("RdpGraphicsAdvertisementGate::ValidationOnly"),
        "macOS opt-in experiment must use validation-only gate"
    );
    assert!(
        !entry.contains("RdpGraphicsAdvertisementGate::LiveInteroperable"),
        "macOS production composition must not carry a live interoperability claim"
    );
}
