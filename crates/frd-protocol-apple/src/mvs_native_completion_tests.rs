//! 原生补全的固定答案、逐掩码差分、裁剪和位流消耗边界。
use super::*;

const ONE: [u8; 3] = [0x11, 0x82, 0xf3];
const ZERO: [u8; 3] = [0xe4, 0x65, 0x26];

#[test]
fn native_completion_bitmap_literal_order_and_all_masks() {
    assert_eq!(
        crate::mvs_pixels::bitmap_rgb_row(0x81, ONE, ZERO),
        [
            0x11, 0x82, 0xf3, 0xe4, 0x65, 0x26, 0xe4, 0x65, 0x26, 0xe4, 0x65, 0x26, 0xe4, 0x65,
            0x26, 0xe4, 0x65, 0x26, 0xe4, 0x65, 0x26, 0x11, 0x82, 0xf3
        ]
    );
    for mask in 0..=255u8 {
        let expected: [u8; 24] = std::array::from_fn(|byte| {
            let color = if (u16::from(mask) >> (7 - byte / 3)) % 2 == 1 {
                ONE
            } else {
                ZERO
            };
            color[byte % 3]
        });
        assert_eq!(
            crate::mvs_pixels::bitmap_rgb_row(mask, ONE, ZERO),
            expected,
            "mask={mask}"
        );
        assert_eq!(
            crate::mvs_pixels::bitmap_rgb_row(mask, ZERO, ONE),
            expected.map(|b| match b {
                0x11 => 0xe4,
                0x82 => 0x65,
                0xf3 => 0x26,
                0xe4 => 0x11,
                0x65 => 0x82,
                0x26 => 0xf3,
                _ => unreachable!(),
            })
        );
    }
}

#[test]
fn native_completion_bitmap_and_fill_clipped_layout_canaries() {
    let mut cases = 0;
    for live_height in 1..=8 {
        for live_width in 1..=8 {
            for (tile_x, tile_y) in [(0, 0), (2, 3)] {
                let width = tile_x + live_width;
                let height = tile_y + live_height;
                let length = width * height * 3;
                for row_mask in [0, 0x81, 0x5a, 0xff] {
                    let masks = [0x81, 0x42, 0x24, 0x18, 0xe7, 0xbd, 0x66, 0x99];
                    let mut wire = vec![row_mask];
                    for (dy, mask) in masks.iter().enumerate() {
                        if row_mask & (0x80 >> dy) == 0 {
                            wire.push(*mask);
                        }
                    }
                    wire.push(0xd3);
                    let mut bits = BitReader::new(&wire);
                    let mut actual = vec![0xac; length + 18];
                    let mut expected = actual.clone();
                    for y in tile_y..height {
                        for x in tile_x..width {
                            let dy = y - tile_y;
                            let dx = x - tile_x;
                            let one = row_mask & (0x80 >> dy) != 0 || masks[dy] & (0x80 >> dx) != 0;
                            let offset = 9 + (y * width + x) * 3;
                            expected[offset..offset + 3].copy_from_slice(if one {
                                &ONE
                            } else {
                                &ZERO
                            });
                        }
                    }
                    decode_bitmap_tile(
                        &mut bits,
                        &mut actual[9..9 + length],
                        width,
                        height,
                        tile_x,
                        tile_y,
                        ONE,
                        ZERO,
                    )
                    .unwrap();
                    assert_eq!(
                        actual, expected,
                        "{live_width}x{live_height}, offset=({tile_x},{tile_y}), rows={row_mask}"
                    );
                    assert_eq!(bits.bit_position(), (wire.len() - 1) * 8);
                    assert_eq!(bits.read_u8().unwrap(), 0xd3);
                    cases += 1;
                }
                let mut actual = vec![0xac; length + 18];
                let mut expected = actual.clone();
                for y in tile_y..height {
                    for x in tile_x..width {
                        let offset = 9 + (y * width + x) * 3;
                        expected[offset..offset + 3].copy_from_slice(&ONE);
                    }
                }
                fill_tile(
                    &mut actual[9..9 + length],
                    width,
                    height,
                    tile_x,
                    tile_y,
                    ONE,
                );
                assert_eq!(actual, expected);
            }
        }
    }
    assert_eq!(cases, 512);
}

#[test]
fn native_completion_bitmap_clipped_rows_still_require_all_wire_masks() {
    for available_rows in 0..8 {
        let wire = vec![0; available_rows + 1];
        let mut bits = BitReader::new(&wire);
        let mut rgb = [0xac; 3];
        let error = decode_bitmap_tile(&mut bits, &mut rgb, 1, 1, 0, 0, ONE, ZERO).unwrap_err();
        assert_eq!(error.to_string(), "MVS type-0 bitmap 缺少像素掩码");
        assert_eq!(
            error.root_cause().to_string(),
            format!(
                "MVS 位流耗尽: bit offset {} 请求 8 位，仅余 0 位",
                wire.len() * 8
            )
        );
        assert_eq!(bits.bit_position(), wire.len() * 8);
        assert_eq!(rgb, if available_rows == 0 { [0xac; 3] } else { ZERO });
    }
    let error = decode_bitmap_tile(
        &mut BitReader::new(&[]),
        &mut [0xac; 3],
        1,
        1,
        0,
        0,
        ONE,
        ZERO,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "MVS type-0 bitmap 缺少行掩码");
}
