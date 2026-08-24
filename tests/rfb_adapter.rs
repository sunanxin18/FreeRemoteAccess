#![cfg(feature = "cli")]

use freeremotedesk::core::{RemotePixelFormat, RenderUpdate};
use freeremotedesk::protocols::rfb::normalize_rect_ops;
use freeremotedesk::vnc::client::RectOp;
use freeremotedesk::vnc::protocol;

#[test]
fn full_raw_frame_emits_reset_rect_and_present_for_one_generation() {
    let updates = normalize_rect_ops(
        1,
        2,
        1,
        vec![RectOp::Raw {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            pixels: vec![0x0011_2233, 0x0044_5566],
        }],
        true,
    )
    .unwrap();

    assert!(matches!(
        updates[0],
        RenderUpdate::Reset {
            generation: 1,
            width: 2,
            height: 1,
            format: RemotePixelFormat::Bgra8Srgb,
        }
    ));
    let RenderUpdate::DirtyRect { ref pixels, .. } = updates[1] else {
        panic!("expected dirty rectangle");
    };
    assert_eq!(
        pixels.as_ref(),
        &[0x33, 0x22, 0x11, 0xff, 0x66, 0x55, 0x44, 0xff]
    );
    assert!(matches!(
        updates[2],
        RenderUpdate::Present { generation: 1 }
    ));
}

#[test]
fn out_of_bounds_rectangle_is_rejected_before_render_emission() {
    let error = normalize_rect_ops(
        2,
        4,
        4,
        vec![RectOp::Raw {
            x: 3,
            y: 3,
            w: 2,
            h: 1,
            pixels: vec![0; 2],
        }],
        false,
    )
    .unwrap_err();

    assert_eq!(error.code(), "rfb_rect_out_of_bounds");
}

#[test]
fn client_cut_text_uses_type_six_and_exact_big_endian_length() {
    let message = protocol::msg_client_cut_text("桌面").unwrap();

    assert_eq!(message[0], 6);
    assert_eq!(&message[1..4], &[0, 0, 0]);
    assert_eq!(&message[4..8], &6u32.to_be_bytes());
    assert_eq!(&message[8..], "桌面".as_bytes());
}
