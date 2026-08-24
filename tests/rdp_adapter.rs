#![cfg(all(feature = "cli", feature = "rdp"))]

use freeremotedesk::app::connection::{validate_connection, ConnectionRequest, ServiceKind};
use freeremotedesk::core::{RemotePixelFormat, RenderUpdate};
use freeremotedesk::protocols::rdp::{build_rdp_config, normalize_rdp_image, RdpDesktopConfig};
use secrecy::SecretString;

fn connection() -> freeremotedesk::app::connection::ValidatedConnection {
    validate_connection(ConnectionRequest {
        service: ServiceKind::WindowsRdp,
        host: "windows.example".to_owned(),
        port: None,
        username: "desktop-user".to_owned(),
        password: SecretString::from("rdp-secret".to_owned()),
        domain: Some("WORKGROUP".to_owned()),
    })
    .unwrap()
}

#[test]
fn rdp_config_enforces_nla_and_preserves_desktop_identity() {
    let config = build_rdp_config(connection(), RdpDesktopConfig::new(1280, 720).unwrap()).unwrap();

    assert_eq!(config.destination().name(), "windows.example");
    assert_eq!(config.destination().port(), 3389);
    assert_eq!(config.connector().desktop_size.width, 1280);
    assert_eq!(config.connector().desktop_size.height, 720);
    assert!(config.connector().enable_credssp);
    assert!(!config.connector().enable_tls);
    assert_eq!(config.connector().domain.as_deref(), Some("WORKGROUP"));
    assert!(!format!("{config:?}").contains("rdp-secret"));
}

#[test]
fn rdp_desktop_dimensions_are_bounded_before_ironrdp() {
    assert_eq!(
        RdpDesktopConfig::new(0, 720).unwrap_err().code(),
        "rdp_desktop_dimensions_invalid"
    );
    assert_eq!(
        RdpDesktopConfig::new(u32::from(u16::MAX) + 1, 720)
            .unwrap_err()
            .code(),
        "rdp_desktop_dimensions_invalid"
    );
}

#[test]
fn decoded_rdp_image_is_normalized_to_bgra_render_contract() {
    let updates = normalize_rdp_image(7, &[0x0011_2233, 0x0044_5566], 2, 1).unwrap();

    assert!(matches!(
        updates[0],
        RenderUpdate::DirtyRect {
            generation: 7,
            format: RemotePixelFormat::Bgra8Srgb,
            bytes_per_row: 8,
            ref pixels,
            ..
        } if pixels.as_ref() == [0x33, 0x22, 0x11, 0xff, 0x66, 0x55, 0x44, 0xff]
    ));
    assert_eq!(updates[1], RenderUpdate::present(7));
}
