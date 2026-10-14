//! 独立 registry 原版 public Stage 输入及 checkpoint producer；不 import 被测代码。
use ironrdp_graphics::{image_processing::PixelFormat, rlgr};
use ironrdp_pdu::{
    bitmap::{BitmapData, BitmapUpdateData, Compression},
    encode_vec,
    fast_path::{EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode},
    geometry::InclusiveRectangle,
    Action, Decode, ReadCursor,
};
use ironrdp_session::{image::DecodedImage, ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, time::Instant};
const FORMATS: [PixelFormat; 8] = [
    PixelFormat::ARgb32,
    PixelFormat::XRgb32,
    PixelFormat::ABgr32,
    PixelFormat::XBgr32,
    PixelFormat::BgrA32,
    PixelFormat::BgrX32,
    PixelFormat::RgbA32,
    PixelFormat::RgbX32,
];
#[derive(Clone)]
struct Step {
    kind: u8,
    drain: bool,
    data: Vec<u8>,
    rects: Vec<[u16; 4]>,
    domain: u8,
}
struct Work {
    name: String,
    w: u16,
    h: u16,
    format: u8,
    pointer: bool,
    full: bool,
    seed: Vec<Step>,
    warm: [Vec<Step>; 2],
}
fn field(o: &mut Vec<u8>, b: &[u8]) {
    o.extend((b.len() as u32).to_le_bytes());
    o.extend(b);
}
fn packet(code: UpdateCode, data: Vec<u8>) -> Vec<u8> {
    let p = encode_vec(&FastPathUpdatePdu {
        fragmentation: Fragmentation::Single,
        update_code: code,
        compression_flags: None,
        compression_type: None,
        data: &data,
    })
    .unwrap();
    assert!(p.len() < 32760);
    let mut o = encode_vec(&FastPathHeader::new(EncryptionFlags::empty(), p.len())).unwrap();
    o.extend(p);
    let mut framing = ReadCursor::new(&o);
    let h: FastPathHeader = ironrdp_pdu::decode_cursor(&mut framing).unwrap();
    assert_eq!(h.data_length, framing.len());
    assert!(o.len() <= 32767);
    o
}
fn rect(r: [u16; 4]) -> InclusiveRectangle {
    InclusiveRectangle {
        left: r[0],
        top: r[1],
        right: r[2],
        bottom: r[3],
    }
}
fn bitmap(
    w: u16,
    h: u16,
    bpp: u16,
    r: [u16; 4],
    compressed: bool,
    data: Vec<u8>,
    slow: bool,
) -> Step {
    let b = BitmapData {
        rectangle: rect(r),
        width: w,
        height: h,
        bits_per_pixel: bpp,
        compression_flags: if compressed {
            Compression::BITMAP_COMPRESSION | Compression::NO_BITMAP_COMPRESSION_HDR
        } else {
            Compression::empty()
        },
        compressed_data_header: None,
        bitmap_data: &data,
    };
    let data = encode_vec(&BitmapUpdateData {
        rectangles: vec![b],
    })
    .unwrap();
    let data = if slow {
        let mut ud = Vec::new();
        ud.extend(((18 + data.len()) as u16).to_le_bytes());
        ud.extend(0x17u16.to_le_bytes());
        ud.extend(1001u16.to_le_bytes());
        ud.extend(1u32.to_le_bytes());
        ud.extend([0, 2]);
        ud.extend((data.len() as u16).to_le_bytes());
        ud.extend([2, 0, 0, 0]);
        ud.extend(data);
        encode_vec(&ironrdp_pdu::x224::X224(
            ironrdp_pdu::mcs::SendDataIndication {
                initiator_id: 1001,
                channel_id: 1003,
                user_data: ud.into(),
            },
        ))
        .unwrap()
    } else {
        packet(UpdateCode::Bitmap, data)
    };
    Step {
        kind: if slow { 1 } else { 0 },
        drain: true,
        data,
        rects: vec![r],
        domain: 0,
    }
}
fn pixels(w: u16, h: u16, bpp: u16, phase: u8, compressed: bool) -> Vec<u8> {
    let depth = (bpp as usize).div_ceil(8);
    let tight = w as usize * depth;
    let stride = (tight + 3) & !3;
    let mut raw = Vec::new();
    for y in 0..h {
        for x in 0..w {
            for c in 0..depth {
                raw.push(
                    (x as u8)
                        .wrapping_mul(11)
                        .wrapping_add((y as u8).wrapping_mul(43))
                        .wrapping_add(c as u8 * 19)
                        .wrapping_add(phase),
                );
            }
        }
        if !compressed {
            raw.extend(vec![0xeb; stride - tight]);
        }
    }
    if !compressed {
        return raw;
    }
    if bpp == 32 {
        let mut p = vec![0x20];
        for c in [2, 1, 0] {
            p.extend(raw.chunks_exact(4).map(|v| v[c]));
        }
        p.push(0);
        p
    } else {
        let mut p = vec![0xf4];
        p.extend((w * h).to_le_bytes());
        p.extend(raw);
        p
    }
}
fn strips(w: u16, h: u16, bpp: u16, c: bool, phase: u8, slow: bool) -> Vec<Step> {
    let maxh = (15000 / (w as usize * (bpp as usize).div_ceil(8)))
        .max(1)
        .min(16) as u16;
    let mut s = Vec::new();
    let mut y = 0;
    while y < h {
        let sh = maxh.min(h - y);
        s.push(bitmap(
            w,
            sh,
            bpp,
            [0, y, w - 1, y + sh - 1],
            c,
            pixels(w, sh, bpp, phase, c),
            slow,
        ));
        y += sh;
    }
    s
}
fn flat(name: String, w: u16, h: u16, bpp: u16, c: bool, slow: bool) -> Work {
    Work {
        name,
        w,
        h,
        format: 6,
        pointer: false,
        full: true,
        seed: strips(w, h, bpp, c, 17, slow),
        warm: [
            strips(w, h, bpp, c, 31, slow),
            strips(w, h, bpp, c, 79, slow),
        ],
    }
}
fn rfx(
    w: u16,
    h: u16,
    mode: ironrdp_pdu::codecs::rfx::EntropyAlgorithm,
    density: u8,
    phase: u8,
    sparse: bool,
    stream: bool,
) -> Vec<Step> {
    use ironrdp_pdu::codecs::rfx::{self, Block, CodecChannel};
    use ironrdp_pdu::surface_commands::*;
    let original = include_bytes!(
        "../../../../crates/frd-rdp-legacy-kernels/tests/fixtures/encoded_messages.bin"
    );
    let mut cur = ReadCursor::new(original);
    let mut template = Vec::new();
    while !cur.is_empty() {
        template.push(Block::decode(&mut cur).unwrap());
    }
    let mut out = Vec::new();
    let tw = w.div_ceil(64);
    let th = h.div_ceil(64);
    let mut coordinates = Vec::new();
    for y in 0..th {
        for x in 0..tw {
            if !sparse || (x == 0 || x == tw - 1) && y == 0 {
                coordinates.push((x, y));
            }
        }
    }
    for coords in coordinates.chunks(2) {
        let mut wire = Vec::new();
        let mut rs = Vec::new();
        for &(x, y) in coords {
            rs.push([
                x * 64,
                y * 64,
                (x * 64 + if sparse { 3 } else { 63 }).min(w - 1),
                (y * 64 + if sparse { 31 } else { 63 }).min(h - 1),
            ]);
        }
        for block in &template {
            let mut b = block.clone();
            match &mut b {
                Block::Channels(c) => {
                    c.0[0].width = w as i16;
                    c.0[0].height = h as i16;
                }
                Block::CodecChannel(CodecChannel::Context(c)) => c.entropy_algorithm = mode,
                Block::CodecChannel(CodecChannel::Region(r)) => {
                    r.rectangles = rs
                        .iter()
                        .map(|r| rfx::RfxRectangle {
                            x: r[0],
                            y: r[1],
                            width: r[2] - r[0] + 1,
                            height: r[3] - r[1] + 1,
                        })
                        .collect();
                    if sparse {
                        r.rectangles.push(r.rectangles[0].clone());
                    }
                }
                Block::CodecChannel(CodecChannel::TileSet(t)) => {
                    t.entropy_algorithm = mode;
                    let q = if density == 2 {
                        [7, 6, 8, 7, 6, 8, 7, 6, 8, 7]
                    } else {
                        [6; 10]
                    };
                    t.quants = vec![rfx::Quant {
                        hl1: q[0],
                        lh1: q[1],
                        hh1: q[2],
                        hl2: q[3],
                        lh2: q[4],
                        hh2: q[5],
                        hl3: q[6],
                        lh3: q[7],
                        hh3: q[8],
                        ll3: q[9],
                    }];
                    let mut planes = Vec::new();
                    for c in 0..3 {
                        let v: Vec<i16> = (0..4096)
                            .map(|i| match density {
                                0 => {
                                    if i == 4032 {
                                        phase as i16 + c * 2
                                    } else {
                                        0
                                    }
                                }
                                1 => ((i * 13 + phase as usize + c as usize * 7) % 7) as i16 - 3,
                                _ => {
                                    if i % 31 < 4 {
                                        ((i + phase as usize) % 9) as i16 - 4
                                    } else {
                                        0
                                    }
                                }
                            })
                            .collect();
                        let mut data = vec![0; 20000];
                        let n = rlgr::encode(mode, &v, &mut data).unwrap();
                        data.truncate(n);
                        planes.push(Box::leak(data.into_boxed_slice()) as &[u8]);
                    }
                    let tile = t.tiles[0].clone();
                    t.tiles = coords
                        .iter()
                        .map(|&(x, y)| {
                            let mut t = tile.clone();
                            t.x = x;
                            t.y = y;
                            t.y_quant_index = 0;
                            t.cb_quant_index = 0;
                            t.cr_quant_index = 0;
                            t.y_data = planes[0];
                            t.cb_data = planes[1];
                            t.cr_data = planes[2];
                            t
                        })
                        .collect();
                }
                _ => {}
            }
            wire.extend(encode_vec(&b).unwrap());
        }
        let bits = SurfaceBitsPdu {
            destination: ironrdp_pdu::geometry::ExclusiveRectangle {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            },
            extended_bitmap_data: ExtendedBitmapDataPdu {
                bpp: 32,
                codec_id: 3,
                width: w,
                height: h,
                header: None,
                data: &wire,
            },
        };
        let cmd = if stream {
            SurfaceCommand::StreamSurfaceBits(bits)
        } else {
            SurfaceCommand::SetSurfaceBits(bits)
        };
        let data = packet(UpdateCode::SurfaceCommands, encode_vec(&cmd).unwrap());
        // 从真实 encoded Region/TileSet 独立导出声明；不能遗漏 repeated clips。
        let rects = wire_rfx_geometry(0, 0, &data, w, h).unwrap();
        out.push(Step {
            kind: 0,
            drain: true,
            data,
            rects,
            domain: 0,
        });
    }
    out
}
fn pointer_shape(kind: u8) -> Step {
    use ironrdp_pdu::pointer::*;
    let (xor, bpp, and): (Vec<u8>, u16, Vec<u8>) = if kind == 4 {
        (vec![255, 255, 255, 0, 0, 0], 24, vec![0xc0, 0])
    } else {
        let alpha = [255, 0, 128, 255][kind as usize];
        (
            vec![255, 255, 255, alpha, 71, 93, 117, alpha],
            32,
            vec![0, 0],
        )
    };
    let shape = PointerAttribute {
        xor_bpp: bpp,
        color_pointer: ColorPointerAttribute {
            cache_index: 1,
            hot_spot: Point16 { x: 0, y: 0 },
            width: 2,
            height: 1,
            xor_mask: &xor,
            and_mask: &and,
        },
    };
    Step {
        kind: 0,
        drain: true,
        data: packet(UpdateCode::NewPointer, encode_vec(&shape).unwrap()),
        rects: vec![],
        domain: 0,
    }
}
fn workloads() -> Vec<Work> {
    let mut out = Vec::new();
    for b in [8, 15, 16, 24, 32] {
        for c in [false, true] {
            for w in [65, 127] {
                out.push(flat(
                    format!("bitmap-{b}-{}-{w}", if c { "rle" } else { "raw" }),
                    w,
                    17,
                    b,
                    c,
                    w == 127,
                ));
            }
        }
    }
    for (w, h) in [(1280, 720), (1920, 1080)] {
        for c in [false, true] {
            out.push(flat(
                format!("desktop-{w}-{h}-{}", if c { "rdp6" } else { "raw32" }),
                w,
                h,
                32,
                c,
                false,
            ));
        }
    }
    for (n, w, h) in [
        ("64x24_argb_rle", 64, 24),
        ("64x35_ycocg_rle_ss", 64, 35),
        ("32x64_rgb_raw", 32, 64),
        ("64x64_ycocg_raw_ss", 64, 64),
        ("64x64_aycocg_rle", 64, 64),
    ] {
        let data = fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/upstream/{n}.bin")),
        )
        .unwrap();
        let st = bitmap(w, h, 32, [0, 0, w - 1, h - 1], true, data, false);
        out.push(Work {
            name: format!("rdp6-original-{n}"),
            w,
            h,
            format: 6,
            pointer: false,
            full: true,
            seed: vec![st.clone()],
            warm: [vec![st.clone()], vec![st]],
        });
    }
    // 原版 source-crop 的公开行为可能是 Err/Panic；不预裁剪 packet。
    for bpp in [8, 15, 16, 24, 32] {
        for c in [false, true] {
            let mut st = bitmap(
                65,
                17,
                bpp,
                [0, 0, 62, 14],
                c,
                pixels(65, 17, bpp, 29, c),
                false,
            );
            st.domain = 1;
            out.push(Work {
                name: format!("crop-{bpp}-{c}"),
                w: 63,
                h: 15,
                format: 6,
                pointer: false,
                full: true,
                seed: vec![st.clone()],
                warm: [vec![st.clone()], vec![st]],
            });
        }
    }
    for mode in [
        ironrdp_pdu::codecs::rfx::EntropyAlgorithm::Rlgr1,
        ironrdp_pdu::codecs::rfx::EntropyAlgorithm::Rlgr3,
    ] {
        for density in 0..3 {
            let name = format!("rfx-{mode:?}-density{density}");
            out.push(Work {
                name,
                w: 128,
                h: 64,
                format: 6,
                pointer: false,
                full: true,
                seed: rfx(128, 64, mode, density, 17, false, false),
                warm: [
                    rfx(128, 64, mode, density, 31, true, false),
                    rfx(128, 64, mode, density, 79, true, true),
                ],
            });
        }
        for (w, h) in [(1280, 720), (1920, 1080)] {
            out.push(Work {
                name: format!("rfx-desktop-{mode:?}-{w}-{h}"),
                w,
                h,
                format: 6,
                pointer: false,
                full: true,
                seed: rfx(w, h, mode, 2, 17, false, false),
                warm: [
                    rfx(w, h, mode, 2, 31, false, false),
                    rfx(w, h, mode, 2, 79, false, true),
                ],
            });
        }
    }
    for f in 0..8 {
        let mut w = flat(format!("vendor-image-format-{f}"), 65, 17, 32, false, false);
        w.format = f;
        w.full = false;
        w.seed.extend(rfx(
            65,
            17,
            ironrdp_pdu::codecs::rfx::EntropyAlgorithm::Rlgr3,
            2,
            11,
            true,
            false,
        ));
        out.push(w);
    }
    for k in 0..5 {
        let mut w = flat(format!("pointer-{k}"), 127, 17, 32, false, false);
        w.pointer = true;
        w.seed.push(pointer_shape(k));
        w.seed.push(Step {
            kind: 0,
            drain: true,
            data: packet(UpdateCode::CachedPointer, 1u16.to_le_bytes().to_vec()),
            rects: vec![],
            domain: 0,
        });
        for (phase, batch) in w.warm.iter_mut().enumerate() {
            batch.insert(
                0,
                Step {
                    kind: 3,
                    drain: true,
                    data: vec![if phase == 0 { 126 } else { 1 }, 0, 16, 0],
                    rects: vec![],
                    domain: 0,
                },
            );
            batch.push(pointer_shape(k));
        }
        out.push(w);
    }
    // 桌面 sparse 与真实 pointer overlap/away：固定稀疏预算域。
    let mut sparse = flat("desktop-sparse-raw32".into(), 1920, 1080, 32, false, false);
    sparse.warm = std::array::from_fn(|i| {
        vec![bitmap(
            127,
            17,
            32,
            [137, 71, 263, 87],
            false,
            pixels(127, 17, 32, if i == 0 { 31 } else { 79 }, false),
            false,
        )]
    });
    out.push(sparse);
    let mode = ironrdp_pdu::codecs::rfx::EntropyAlgorithm::Rlgr3;
    out.push(Work {
        name: "desktop-sparse-rfx-dense".into(),
        w: 1920,
        h: 1080,
        format: 6,
        pointer: false,
        full: true,
        seed: rfx(1920, 1080, mode, 2, 17, false, false),
        warm: [
            rfx(1920, 1080, mode, 1, 31, true, false),
            rfx(1920, 1080, mode, 1, 79, true, true),
        ],
    });
    for k in [0, 2, 4] {
        let mut w = flat(format!("desktop-pointer-{k}"), 1920, 1080, 32, false, false);
        w.pointer = true;
        w.seed.push(pointer_shape(k));
        w.seed.push(Step {
            kind: 0,
            drain: true,
            data: packet(UpdateCode::CachedPointer, 1u16.to_le_bytes().to_vec()),
            rects: vec![],
            domain: 0,
        });
        w.warm = std::array::from_fn(|i| {
            vec![
                Step {
                    kind: 3,
                    drain: true,
                    data: if i == 0 {
                        vec![0x7f, 7, 0x37, 4]
                    } else {
                        vec![3, 0, 1, 0]
                    },
                    rects: vec![],
                    domain: 0,
                },
                bitmap(
                    65,
                    17,
                    32,
                    if i == 0 {
                        [1500, 600, 1564, 616]
                    } else {
                        [0, 0, 64, 16]
                    },
                    false,
                    pixels(65, 17, 32, if i == 0 { 31 } else { 79 }, false),
                    false,
                ),
                pointer_shape(k),
            ]
        });
        out.push(w);
    }
    let frozen=include_bytes!("../../../../third_party/ironrdp-session-0.11.0/tests/fixtures/rfx-original-supplemental.bin");
    let mut fields = &frozen[12..];
    while !fields.is_empty() {
        let f: [&[u8]; 6] = std::array::from_fn(|_| {
            let n = u32::from_le_bytes(fields[..4].try_into().unwrap()) as usize;
            let v = &fields[4..4 + n];
            fields = &fields[4 + n..];
            v
        });
        let name = std::str::from_utf8(f[0]).unwrap();
        if name.starts_with("session-")
            && (name.ends_with("second-empty-y") || name.ends_with("second-color"))
        {
            use ironrdp_pdu::surface_commands::*;
            let cmd = SurfaceCommand::SetSurfaceBits(SurfaceBitsPdu {
                destination: ironrdp_pdu::geometry::ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 128,
                    bottom: 64,
                },
                extended_bitmap_data: ExtendedBitmapDataPdu {
                    bpp: 32,
                    codec_id: 3,
                    width: 128,
                    height: 64,
                    header: None,
                    data: f[2],
                },
            });
            let st = Step {
                kind: 0,
                drain: true,
                data: packet(UpdateCode::SurfaceCommands, encode_vec(&cmd).unwrap()),
                rects: vec![],
                domain: 2,
            };
            out.push(Work {
                name: format!("failure-{name}"),
                w: 128,
                h: 64,
                format: 6,
                pointer: false,
                full: true,
                seed: vec![st],
                warm: [vec![], vec![]],
            });
        }
    }
    for (name, mut st) in [
        (
            "header",
            Step {
                kind: 0,
                drain: true,
                data: vec![0],
                rects: vec![],
                domain: 2,
            },
        ),
        (
            "strict-underfill",
            bitmap(2, 1, 24, [0, 0, 1, 0], true, vec![0x61, 9, 8, 7], false),
        ),
    ] {
        st.domain = 2;
        st.rects.clear();
        out.push(Work {
            name: format!("failure-{name}"),
            w: 2,
            h: 1,
            format: 6,
            pointer: false,
            full: true,
            seed: vec![st],
            warm: [vec![], vec![]],
        });
    }
    let mut ctl = flat(
        "control-holes-replace-overflow".into(),
        3,
        1,
        24,
        false,
        false,
    );
    let a = bitmap(1, 1, 24, [0, 0, 0, 0], true, vec![0x61, 11, 23, 37], false);
    let b = bitmap(1, 1, 24, [2, 0, 2, 0], true, vec![0x61, 47, 59, 71], false);
    let m = bitmap(1, 1, 24, [1, 0, 1, 0], true, vec![0x61, 83, 97, 109], false);
    ctl.seed = vec![
        a.clone(),
        b.clone(),
        m.clone(),
        Step {
            kind: 2,
            drain: true,
            data: vec![],
            rects: vec![],
            domain: 0,
        },
        a,
        b,
        m.clone(),
    ];
    ctl.seed.last_mut().unwrap().drain = false;
    ctl.seed.push(m);
    out.push(ctl);
    out
}
fn stage(pointer: bool) -> ActiveStage {
    ActiveStageBuilder {
        static_channels: Default::default(),
        user_channel_id: 1001,
        io_channel_id: 1003,
        message_channel_id: None,
        share_id: 1,
        compression_type: None,
        enable_server_pointer: pointer,
        pointer_software_rendering: pointer,
    }
    .build()
}
fn apply(
    st: &mut ActiveStage,
    img: &mut DecodedImage,
    s: &Step,
    pointer: bool,
) -> Result<Vec<ActiveStageOutput>, String> {
    match s.kind {
        2 => {
            *img = DecodedImage::new(img.pixel_format(), img.width(), img.height());
            st.set_fastpath_processor(
                ironrdp_session::fast_path::ProcessorBuilder {
                    io_channel_id: 1003,
                    user_channel_id: 1001,
                    share_id: 2,
                    enable_server_pointer: pointer,
                    pointer_software_rendering: pointer,
                    bulk_decompressor: None,
                }
                .build(),
            );
            Ok(vec![])
        }
        3 => {
            use ironrdp_pdu::input::{
                fast_path::FastPathInputEvent,
                mouse::{MousePdu, PointerFlags},
            };
            st.process_fastpath_input(
                img,
                &[FastPathInputEvent::MouseEvent(MousePdu {
                    flags: PointerFlags::MOVE,
                    number_of_wheel_rotation_units: 0,
                    x_position: u16::from_le_bytes(s.data[..2].try_into().unwrap()),
                    y_position: u16::from_le_bytes(s.data[2..].try_into().unwrap()),
                })],
            )
            .map_err(|e| format!("{e:?}"))
        }
        4 => {
            st.update_mouse_pos(
                u16::from_le_bytes(s.data[..2].try_into().unwrap()),
                u16::from_le_bytes(s.data[2..].try_into().unwrap()),
            );
            Ok(vec![])
        }
        _ => st
            .process(
                img,
                if s.kind == 1 {
                    Action::X224
                } else {
                    Action::FastPath
                },
                &s.data,
            )
            .map_err(|e| format!("{e:?}")),
    }
}
fn checkpoint(
    o: &mut Vec<u8>,
    st: &mut ActiveStage,
    img: &mut DecodedImage,
    s: &Step,
    pointer: bool,
) {
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| apply(st, img, s, pointer)));
    let (status, outputs) = match result {
        Ok(Ok(v)) => (0, format!("{v:?}")),
        Ok(Err(e)) => (1, e),
        Err(_) => (2, "Panic".into()),
    };
    o.push(status);
    o.extend(Sha256::digest(img.data()));
    let bg: Vec<_> = img
        .data()
        .chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], 255])
        .collect();
    o.extend(Sha256::digest(&bg));
    field(o, outputs.as_bytes());
    for i in [0, img.data().len() / 8 * 4, img.data().len() - 4] {
        o.extend(&img.data()[i..i + 4]);
    }
}

// 与同-wire原版结果分开：source-size原版图 + 显式 top-left geometry projection。
fn crop_projection(s: &Step, dw: u16, dh: u16) -> Vec<u8> {
    let mut cur = ReadCursor::new(&s.data);
    let _: FastPathHeader = ironrdp_pdu::decode_cursor(&mut cur).unwrap();
    let p: FastPathUpdatePdu<'_> = ironrdp_pdu::decode_cursor(&mut cur).unwrap();
    let mut b: BitmapUpdateData<'_> = ironrdp_pdu::decode(p.data).unwrap();
    let (sw, sh) = (b.rectangles[0].width, b.rectangles[0].height);
    b.rectangles[0].rectangle = rect([0, 0, sw - 1, sh - 1]);
    let wire = packet(UpdateCode::Bitmap, encode_vec(&b).unwrap());
    let mut reference = DecodedImage::new(PixelFormat::RgbA32, sw, sh);
    stage(false)
        .process(&mut reference, Action::FastPath, &wire)
        .unwrap();
    let mut rgba = Vec::new();
    for y in 0..dh as usize {
        rgba.extend_from_slice(&reference.data()[y * sw as usize * 4..][..dw as usize * 4]);
    }
    let bgrx: Vec<_> = rgba
        .chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], 255])
        .collect();
    let mut out = Vec::new();
    out.extend(Sha256::digest(&rgba));
    out.extend(Sha256::digest(&bgrx));
    for i in [0, rgba.len() / 8 * 4, rgba.len() - 4] {
        out.extend(&rgba[i..i + 4]);
    }
    out.extend(Sha256::digest(&wire));
    out
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--annotate-from") {
        annotate_metadata();
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--warm-reference") {
        generate_warm_reference();
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--bench") {
        bench_original();
        return;
    }

    let small_rfx = std::env::args().nth(1).as_deref() == Some("--generate-small-rfx");
    let dir = std::env::args()
        .nth(if small_rfx { 2 } else { 1 })
        .expect("output directory");
    assert!(!Path::new(&dir).exists(), "新producer目录必须独立且不存在");
    fs::create_dir_all(&dir).unwrap();
    let mut ws = workloads();
    if small_rfx {
        ws.retain(|w| w.name.starts_with("rfx-Rlgr"));
        assert_eq!(ws.len(), 6);
    }
    let producer_sha = format!(
        "{:x}",
        Sha256::digest(
            fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/pipeline.rs")).unwrap()
        )
    );
    let mut manifest = Vec::new();
    let mut bundle = b"FRDBND01".to_vec();
    bundle.extend((ws.len() as u32).to_le_bytes());
    for w in &ws {
        let mut data = b"FRDPIPE1".to_vec();
        field(&mut data, w.name.as_bytes());
        data.extend(w.w.to_le_bytes());
        data.extend(w.h.to_le_bytes());
        data.extend([w.format, w.pointer as u8, w.full as u8]);
        let mut expected = b"FRDEXP01".to_vec();
        let mut st = stage(w.pointer);
        let mut img = DecodedImage::new(FORMATS[w.format as usize], w.w, w.h);
        for batch in [&w.seed, &w.warm[0], &w.warm[1]] {
            data.extend((batch.len() as u32).to_le_bytes());
            for s in batch {
                data.extend([s.kind, s.drain as u8, s.domain]);
                field(&mut data, &s.data);
                data.extend((s.rects.len() as u32).to_le_bytes());
                for r in &s.rects {
                    for v in r {
                        data.extend(v.to_le_bytes());
                    }
                }
                checkpoint(&mut expected, &mut st, &mut img, s, w.pointer);
                field(
                    &mut expected,
                    &if s.domain == 1 {
                        crop_projection(s, w.w, w.h)
                    } else {
                        vec![]
                    },
                );
            }
        }
        field(&mut bundle, &data);
        field(&mut bundle, &expected);
        let mut e = Input(&expected[8..]);
        let mut checkpoints = Vec::new();
        for (batch, steps) in [&w.seed, &w.warm[0], &w.warm[1]].iter().enumerate() {
            for (index, _) in steps.iter().enumerate() {
                let status = e.byte();
                let ih = e.take(32);
                let bh = e.take(32);
                let outputs = String::from_utf8(e.field().to_vec()).unwrap();
                let anchors = e.take(12);
                let proj = e.field();
                if index == 0 || index + 1 == steps.len() {
                    checkpoints.push(serde_json::json!({"batch":batch,"step":index,"status":status,"image_sha256":hex(ih),"bgrx_sha256":hex(bh),"outputs":outputs,"literal_rgba_anchors":anchors,"crop_projection":proj}));
                }
            }
        }
        let max_frame = w
            .seed
            .iter()
            .chain(w.warm.iter().flatten())
            .filter(|s| s.kind <= 1)
            .map(|s| s.data.len())
            .max()
            .unwrap_or(0);
        manifest.push(serde_json::json!({"name":w.name,"width":w.w,"height":w.h,"destination_format":w.format,"adapter_lane":w.full,"pointer":w.pointer,"wire_sha256":format!("{:x}",Sha256::digest(&data)),"expect_sha256":format!("{:x}",Sha256::digest(&expected)),"input_bytes":data.len(),"seed_steps":w.seed.len(),"warm_steps":[w.warm[0].len(),w.warm[1].len()],"max_framed_bytes":max_frame,"framing":"public encoder+public Stage; FastPath<=32767; bitmap payload<=15000; no truncation","checkpoint_anchors":checkpoints}));
    }
    fs::write(Path::new(&dir).join("workloads.bin"), &bundle).unwrap();
    fs::write(Path::new(&dir).join("manifest.json"),serde_json::to_vec_pretty(&serde_json::json!({"schema":1,"source":"registry ironrdp-session=0.11.0/graphics=0.9.0/pdu=0.9.0 public ActiveStage","producer_source_sha256":producer_sha,"bundle_sha256":hex(&Sha256::digest(&bundle)), "bulk":"not negotiated; no-bulk lane only","qoi":"not enabled","warmup":64,"measured":256,"windows":3,"cap":4096,"workloads":manifest})).unwrap()).unwrap();
    println!(
        "generated independent Stage checkpoints workloads={}",
        ws.len()
    );
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}
struct Input<'a>(&'a [u8]);
impl<'a> Input<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        a
    }
    fn byte(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take(2).try_into().unwrap())
    }
    fn u32(&mut self) -> usize {
        u32::from_le_bytes(self.take(4).try_into().unwrap()) as usize
    }
    fn field(&mut self) -> &'a [u8] {
        let n = self.u32();
        self.take(n)
    }
}
fn read_frozen(path: &Path) -> Vec<(Work, [Vec<([u8; 32], u8)>; 3])> {
    let bytes = fs::read(path).unwrap();
    let mut c = Input(&bytes);
    assert_eq!(c.take(8), b"FRDBND01");
    let count = c.u32();
    let mut out = Vec::new();
    for _ in 0..count {
        let mut wire = Input(c.field());
        let mut e = Input(c.field());
        assert_eq!(wire.take(8), b"FRDPIPE1");
        assert_eq!(e.take(8), b"FRDEXP01");
        let name = String::from_utf8(wire.field().to_vec()).unwrap();
        let (w, h, format, pointer, full) = (
            wire.u16(),
            wire.u16(),
            wire.byte(),
            wire.byte() != 0,
            wire.byte() != 0,
        );
        let mut checks: [Vec<([u8; 32], u8)>; 3] = Default::default();
        let batches: [Vec<Step>; 3] = std::array::from_fn(|batch| {
            let n = wire.u32();
            (0..n)
                .map(|_| {
                    let (kind, drain, domain) = (wire.byte(), wire.byte() != 0, wire.byte());
                    let data = wire.field().to_vec();
                    let count = wire.u32();
                    let rects = (0..count)
                        .map(|_| [wire.u16(), wire.u16(), wire.u16(), wire.u16()])
                        .collect();
                    let status = e.byte();
                    let image: [u8; 32] = e.take(32).try_into().unwrap();
                    e.take(32);
                    e.field();
                    e.take(12);
                    e.field();
                    checks[batch].push((image, status));
                    Step {
                        kind,
                        drain,
                        data,
                        rects,
                        domain,
                    }
                })
                .collect()
        });
        let [seed, a, b] = batches;
        out.push((
            Work {
                name,
                w,
                h,
                format,
                pointer,
                full,
                seed,
                warm: [a, b],
            },
            checks,
        ));
        assert!(wire.0.is_empty() && e.0.is_empty());
    }
    assert!(c.0.is_empty());
    out
}
fn percentile(t: &[u128], p: usize) -> u128 {
    let mut s = t.to_vec();
    s.sort_unstable();
    s[(s.len() * p)
        .div_ceil(100)
        .saturating_sub(1)
        .min(s.len() - 1)]
}
fn bench_original() {
    let args: Vec<_> = std::env::args().collect();
    let ws = read_frozen(Path::new(&args[2]));
    let lines = fs::read_to_string(&args[3]).unwrap();
    let warm = args
        .get(5)
        .map(|p| read_warm_reference(Path::new(p)))
        .unwrap_or_default();
    let mut rows = Vec::new();
    for line in lines.lines() {
        let f: Vec<_> = line.split('\t').collect();
        if f.first() != Some(&"PIPELINE_STAGE") || f[3] != "0" {
            continue;
        }
        let window: f64 = f[2].parse().unwrap();
        let count: usize = f[4].parse().unwrap();
        assert!((256..=4096).contains(&count) && count % 256 == 0);
        let (w, expected) = ws.iter().find(|(w, _)| w.name == f[1]).unwrap();
        assert!(w
            .seed
            .iter()
            .chain(w.warm.iter().flatten())
            .all(|s| s.domain == 0));
        let start = Instant::now();
        let mut st = stage(w.pointer);
        let mut img = DecodedImage::new(FORMATS[w.format as usize], w.w, w.h);
        let setup = start.elapsed().as_nanos();
        let start = Instant::now();
        for s in &w.seed {
            std::hint::black_box(apply(&mut st, &mut img, s, w.pointer).unwrap());
        }
        let cold = start.elapsed().as_nanos();
        assert_eq!(
            Sha256::digest(img.data()).as_slice(),
            expected[0].last().unwrap().0
        );
        for k in 0..64 {
            for s in &w.warm[k % 2] {
                std::hint::black_box(apply(&mut st, &mut img, s, w.pointer).unwrap());
            }
        }
        let steady = warm
            .iter()
            .find(|(name, _)| name == &w.name)
            .map(|(_, phases)| phases);
        assert!(
            !w.name.starts_with("desktop-pointer-") || steady.is_some(),
            "stateful measurement requires independent warm sidecar"
        );
        let mut times = Vec::with_capacity(count);
        for k in 0..count {
            if let Some(reference) = steady {
                let phase = k % 2;
                let mut ns = 0;
                for (index, s) in w.warm[phase].iter().enumerate() {
                    let start = Instant::now();
                    let result = apply(&mut st, &mut img, s, w.pointer).unwrap();
                    ns += start.elapsed().as_nanos();
                    assert_eq!(
                        format!("{result:?}"),
                        reference[phase][index].outputs,
                        "{} original warm outputs step{index}",
                        w.name
                    );
                    assert!(
                        img.data() == reference[phase][index].rgba.as_slice(),
                        "{} original complete warm image step{index}",
                        w.name
                    );
                }
                times.push(ns);
            } else {
                let start = Instant::now();
                for s in &w.warm[k % 2] {
                    std::hint::black_box(apply(&mut st, &mut img, s, w.pointer).unwrap());
                }
                times.push(start.elapsed().as_nanos());
                assert_eq!(
                    Sha256::digest(img.data()).as_slice(),
                    expected[1 + k % 2].last().unwrap().0,
                    "{} original persistent image",
                    w.name
                );
            }
        }
        rows.push(serde_json::json!({"lane":"O-stage","workload":w.name,"window":window,"count":count,"minimum_count":256,"warmup":64,"setup_ns":setup,"cold_ns":cold,"p50_ns":percentile(&times,50),"p95_ns":percentile(&times,95),"p99_ns":percentile(&times,99),"max_ns":times.iter().max(),"total_ns":times.iter().sum::<u128>(),"checksum":hex(&Sha256::digest(img.data()))}));
        println!("ORIGINAL_STAGE {} window={window} count={count}", w.name);
    }
    fs::write(&args[4], serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
}

// Metadata-only annotation：不调用 Stage，不读取任何 production receipt/helper。
const ANNOTATION_RECIPE:&str="A2.10A-v5: common domain0 FastPath RFX only; image-bounded inclusive clips sorted by (top,bottom,left,right), multiplicity preserved; TileSet order x clip order intersections; all other work/step bytes and complete original expect blobs unchanged; fresh producer uses this same independent wire geometry contract; stateful warm expectations are an independently generated sidecar and cold records stay identical";
fn geometry_ops(
    width: u16,
    height: u16,
    destination: [u16; 2],
    regions: &[[u16; 4]],
    tiles: &[[u16; 2]],
) -> Vec<[u16; 4]> {
    let mut clips = Vec::new();
    for r in regions {
        let (l, t) = (
            u32::from(destination[0]) + u32::from(r[0]),
            u32::from(destination[1]) + u32::from(r[1]),
        );
        if l >= u32::from(width) || t >= u32::from(height) {
            continue;
        }
        assert!(r[2] > 0 && r[3] > 0);
        clips.push([
            l as u16,
            t as u16,
            (l + u32::from(r[2]) - 1).min(u32::from(width) - 1) as u16,
            (t + u32::from(r[3]) - 1).min(u32::from(height) - 1) as u16,
        ]);
    }
    clips.sort_unstable_by_key(|r| (r[1], r[3], r[0], r[2]));
    let mut out = Vec::new();
    for tile in tiles {
        let (l, t) = (
            u32::from(destination[0]) + u32::from(tile[0]) * 64,
            u32::from(destination[1]) + u32::from(tile[1]) * 64,
        );
        for r in &clips {
            let left = l.max(u32::from(r[0]));
            let top = t.max(u32::from(r[1]));
            let right = (l + 63).min(u32::from(r[2]));
            let bottom = (t + 63).min(u32::from(r[3]));
            if left <= right && top <= bottom {
                out.push([left as u16, top as u16, right as u16, bottom as u16]);
            }
        }
    }
    out
}
fn wire_rfx_geometry(
    kind: u8,
    domain: u8,
    data: &[u8],
    width: u16,
    height: u16,
) -> Option<Vec<[u16; 4]>> {
    if kind != 0 || domain != 0 {
        return None;
    }
    use ironrdp_pdu::{
        codecs::rfx::{Block, CodecChannel},
        decode_cursor,
        surface_commands::SurfaceCommand,
    };
    let mut c = ReadCursor::new(data);
    let h: FastPathHeader = decode_cursor(&mut c).unwrap();
    assert_eq!(h.data_length, c.len());
    let mut out = Vec::new();
    let mut found = false;
    while !c.is_empty() {
        let p: FastPathUpdatePdu<'_> = decode_cursor(&mut c).unwrap();
        if p.update_code != UpdateCode::SurfaceCommands {
            continue;
        }
        let mut commands = ReadCursor::new(p.data);
        while !commands.is_empty() {
            let command: SurfaceCommand<'_> = decode_cursor(&mut commands).unwrap();
            if let SurfaceCommand::SetSurfaceBits(b) | SurfaceCommand::StreamSurfaceBits(b) =
                command
            {
                if b.extended_bitmap_data.codec_id != 3 {
                    continue;
                }
                found = true;
                let mut blocks = ReadCursor::new(b.extended_bitmap_data.data);
                let mut regions = Vec::new();
                while !blocks.is_empty() {
                    let block: Block<'_> = decode_cursor(&mut blocks).unwrap();
                    match block {
                        Block::CodecChannel(CodecChannel::Region(r)) => {
                            regions = r
                                .rectangles
                                .iter()
                                .map(|r| [r.x, r.y, r.width, r.height])
                                .collect()
                        }
                        Block::CodecChannel(CodecChannel::TileSet(t)) => {
                            let tiles: Vec<_> = t.tiles.iter().map(|t| [t.x, t.y]).collect();
                            out.extend(geometry_ops(
                                width,
                                height,
                                [b.destination.left, b.destination.top],
                                &regions,
                                &tiles,
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    found.then_some(out)
}
struct AnnotationProof {
    steps: usize,
    changed: usize,
    unchanged: usize,
    batches: usize,
}
// 删除声明geometry后的完整原始bytes必须完全相同，并保留全部层级数量和顺序。
fn strip_geometry(data: &[u8]) -> Vec<u8> {
    let mut i = Input(data);
    assert_eq!(i.take(8), b"FRDPIPE1");
    i.field();
    i.take(7);
    let prefix = data.len() - i.0.len();
    let mut out = data[..prefix].to_vec();
    for _ in 0..3 {
        let n_bytes = i.take(4);
        out.extend(n_bytes);
        let n = u32::from_le_bytes(n_bytes.try_into().unwrap());
        for _ in 0..n {
            let start = data.len() - i.0.len();
            i.take(3);
            i.field();
            let end = data.len() - i.0.len();
            out.extend(&data[start..end]);
            let count = i.u32();
            i.take(count * 8);
        }
    }
    assert!(i.0.is_empty());
    out
}
fn annotate_wire(data: &[u8], proof: &mut AnnotationProof) -> Vec<u8> {
    let mut i = Input(data);
    assert_eq!(i.take(8), b"FRDPIPE1");
    i.field();
    let width = i.u16();
    let height = i.u16();
    i.take(3);
    let prefix = data.len() - i.0.len();
    let mut out = data[..prefix].to_vec();
    for _ in 0..3 {
        proof.batches += 1;
        let n_bytes = i.take(4);
        out.extend(n_bytes);
        let n = u32::from_le_bytes(n_bytes.try_into().unwrap());
        for _ in 0..n {
            proof.steps += 1;
            let start = data.len() - i.0.len();
            let kind = i.byte();
            i.byte();
            let domain = i.byte();
            let payload = i.field();
            let end = data.len() - i.0.len();
            out.extend(&data[start..end]);
            let count = i.u32();
            let old = i.take(count * 8);
            let new = wire_rfx_geometry(kind, domain, payload, width, height).map(|rects| {
                let mut bytes = Vec::new();
                for r in rects {
                    for v in r {
                        bytes.extend(v.to_le_bytes());
                    }
                }
                bytes
            });
            let bytes = new.as_deref().unwrap_or(old);
            if bytes != old {
                proof.changed += 1;
                assert_eq!(kind, 0);
                assert_eq!(domain, 0);
            } else {
                proof.unchanged += 1;
            }
            out.extend(((bytes.len() / 8) as u32).to_le_bytes());
            out.extend(bytes);
        }
    }
    assert!(i.0.is_empty());
    assert_eq!(
        strip_geometry(data),
        strip_geometry(&out),
        "每个workload除geometry外逐byte一致"
    );
    out
}
fn annotate_metadata() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 4);
    let base = Path::new(&args[2]);
    let destination = Path::new(&args[3]);
    assert!(!destination.exists(), "新annotation目录必须独立且不存在");
    let base_manifest_bytes = fs::read(base.join("manifest.json")).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(&base_manifest_bytes).unwrap();
    let original_source = fs::read(base.join("original-producer.rs")).unwrap();
    let original_exe = fs::read(base.join("original-producer")).unwrap();
    assert_eq!(
        hex(&Sha256::digest(&original_source)),
        manifest["producer_source_sha256"]
    );
    let bytes = fs::read(base.join("workloads.bin")).unwrap();
    assert_eq!(hex(&Sha256::digest(&bytes)), manifest["bundle_sha256"]);
    // 固定独立literal anchors：wire [L,R,L]；逐tile交集应 [L,L,R]。
    let repeated = geometry_ops(
        128,
        64,
        [0, 0],
        &[[0, 0, 4, 32], [64, 0, 4, 32], [0, 0, 4, 32]],
        &[[0, 0], [1, 0]],
    );
    assert_eq!(repeated, [[0, 0, 3, 31], [0, 0, 3, 31], [64, 0, 67, 31]]);
    let edge = geometry_ops(
        65,
        17,
        [0, 0],
        &[[0, 0, 4, 32], [64, 0, 4, 32], [0, 0, 4, 32]],
        &[[0, 0], [1, 0]],
    );
    assert_eq!(edge, [[0, 0, 3, 16], [0, 0, 3, 16], [64, 0, 64, 16]]);
    let mut input = Input(&bytes);
    assert_eq!(input.take(8), b"FRDBND01");
    let count = input.u32();
    assert_eq!(count, manifest["workloads"].as_array().unwrap().len());
    let mut bundle = b"FRDBND01".to_vec();
    bundle.extend((count as u32).to_le_bytes());
    let mut proof = AnnotationProof {
        steps: 0,
        changed: 0,
        unchanged: 0,
        batches: 0,
    };
    let mut input_chain = Sha256::new();
    let mut output_chain = Sha256::new();
    let mut old_expect_chain = Sha256::new();
    let mut new_expect_chain = Sha256::new();
    for (index, row) in manifest["workloads"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        let wire = input.field();
        let expected = input.field();
        assert_eq!(hex(&Sha256::digest(wire)), row["wire_sha256"]);
        assert_eq!(hex(&Sha256::digest(expected)), row["expect_sha256"]);
        let mut name = Input(wire);
        name.take(8);
        assert_eq!(std::str::from_utf8(name.field()).unwrap(), row["name"]);
        let updated = annotate_wire(wire, &mut proof);
        let mut new_name = Input(&updated);
        new_name.take(8);
        assert_eq!(
            std::str::from_utf8(new_name.field()).unwrap(),
            row["name"],
            "workload ordinal {index}"
        );
        input_chain.update(strip_geometry(wire));
        output_chain.update(strip_geometry(&updated));
        old_expect_chain.update(expected);
        field(&mut bundle, &updated);
        field(&mut bundle, expected);
        new_expect_chain.update(expected);
        row["base_wire_sha256"] = row["wire_sha256"].clone();
        row["wire_sha256"] = hex(&Sha256::digest(&updated)).into();
        row["input_bytes"] = updated.len().into();
    }
    assert!(input.0.is_empty());
    let old_sequence = input_chain.finalize();
    let new_sequence = output_chain.finalize();
    let old_expect = old_expect_chain.finalize();
    let new_expect = new_expect_chain.finalize();
    assert_eq!(old_sequence, new_sequence);
    assert_eq!(old_expect, new_expect);
    assert_eq!(proof.steps, proof.changed + proof.unchanged);
    assert_eq!(proof.batches, count * 3);
    let annotation_source =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/pipeline.rs")).unwrap();
    let annotation_exe = fs::read(std::env::current_exe().unwrap()).unwrap();
    manifest["bundle_sha256"] = hex(&Sha256::digest(&bundle)).into();
    manifest["annotation"] = serde_json::json!({"kind":"metadata-only; original Stage was not rerun","base_bundle_sha256":hex(&Sha256::digest(&bytes)),"base_manifest_sha256":hex(&Sha256::digest(&base_manifest_bytes)),"base_directory":base,"original_producer_source_sha256":hex(&Sha256::digest(&original_source)),"original_producer_executable_sha256":hex(&Sha256::digest(&original_exe)),"annotation_source_sha256":hex(&Sha256::digest(&annotation_source)),"annotation_executable_sha256":hex(&Sha256::digest(&annotation_exe)),"command":args,"recipe":ANNOTATION_RECIPE,"recipe_sha256":hex(&Sha256::digest(ANNOTATION_RECIPE.as_bytes())),"equivalence":{"workloads":count,"batches":proof.batches,"steps":proof.steps,"changed_geometry_steps":proof.changed,"unchanged_geometry_steps":proof.unchanged,"non_geometry_sequence_sha256":hex(&old_sequence),"complete_expect_chain_sha256":hex(&old_expect),"all_non_geometry_bytes_identical":true,"all_original_expect_blobs_identical":true},"literal_geometry_anchors":{"repeated":repeated,"edge_65x17":edge}});
    fs::create_dir_all(destination).unwrap();
    fs::write(destination.join("workloads.bin"), bundle).unwrap();
    fs::write(
        destination.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    println!("ANNOTATION_PROOF workloads={count} batches={} steps={} changed_geometry_steps={} all_other_bytes_and_original_expect_blobs_identical=true",proof.batches,proof.steps,proof.changed);
}

struct WarmExpected {
    rgba: Vec<u8>,
    outputs: String,
}
fn read_warm_reference(path: &Path) -> Vec<(String, [Vec<WarmExpected>; 2])> {
    let bytes = fs::read(path).unwrap();
    let mut c = Input(&bytes);
    assert_eq!(c.take(8), b"FRDWRM01");
    assert_eq!(c.u32(), 64);
    assert_eq!(c.u32(), 4096);
    let count = c.u32();
    let mut out = Vec::new();
    for _ in 0..count {
        let name = String::from_utf8(c.field().to_vec()).unwrap();
        let phases = std::array::from_fn(|_| {
            let n = c.u32();
            (0..n)
                .map(|_| {
                    let mut e = Input(c.field());
                    assert_eq!(e.byte(), 0);
                    let image: [u8; 32] = e.take(32).try_into().unwrap();
                    e.take(32);
                    let outputs = String::from_utf8(e.field().to_vec()).unwrap();
                    e.take(12);
                    assert!(e.field().is_empty() && e.0.is_empty());
                    let rgba = decode_rows(c.field());
                    assert_eq!(Sha256::digest(&rgba).as_slice(), image);
                    WarmExpected { rgba, outputs }
                })
                .collect()
        });
        out.push((name, phases));
    }
    assert!(c.0.is_empty());
    out
}
fn generate_warm_reference() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 4);
    let base = Path::new(&args[2]);
    let destination = Path::new(&args[3]);
    assert!(!destination.exists());
    let bundle = fs::read(base.join("workloads.bin")).unwrap();
    let manifest_bytes = fs::read(base.join("manifest.json")).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(hex(&Sha256::digest(&bundle)), manifest["bundle_sha256"]);
    let ws = read_frozen(&base.join("workloads.bin"));
    let selected: Vec<_> = ws
        .iter()
        .filter(|(w, _)| w.name.starts_with("desktop-pointer-"))
        .collect();
    assert_eq!(selected.len(), 3);
    struct Reference {
        blob: Vec<u8>,
        rgba: Vec<u8>,
        outputs: String,
    }
    let mut output = b"FRDWRM01".to_vec();
    output.extend(64u32.to_le_bytes());
    output.extend(4096u32.to_le_bytes());
    output.extend(3u32.to_le_bytes());
    let mut proof = Vec::new();
    for (w, initial) in selected {
        let mut st = stage(w.pointer);
        let mut img = DecodedImage::new(FORMATS[w.format as usize], w.w, w.h);
        for s in &w.seed {
            apply(&mut st, &mut img, s, w.pointer).unwrap();
        }
        assert_eq!(
            Sha256::digest(img.data()).as_slice(),
            initial[0].last().unwrap().0
        );
        let mut once_a = Vec::new();
        for k in 0..64 {
            for s in &w.warm[k % 2] {
                apply(&mut st, &mut img, s, w.pointer).unwrap();
            }
            if k == 0 {
                once_a = img.data().to_vec();
                assert_eq!(
                    Sha256::digest(&once_a).as_slice(),
                    initial[1].last().unwrap().0
                );
            }
            if k == 1 {
                assert_eq!(
                    Sha256::digest(img.data()).as_slice(),
                    initial[2].last().unwrap().0
                );
            }
        }
        let reference: [Vec<Reference>; 2] = std::array::from_fn(|phase| {
            w.warm[phase]
                .iter()
                .map(|s| {
                    let mut blob = Vec::new();
                    checkpoint(&mut blob, &mut st, &mut img, s, w.pointer);
                    field(&mut blob, &[]);
                    let mut e = Input(&blob);
                    assert_eq!(e.byte(), 0);
                    e.take(64);
                    let outputs = String::from_utf8(e.field().to_vec()).unwrap();
                    Reference {
                        blob,
                        rgba: img.data().to_vec(),
                        outputs,
                    }
                })
                .collect()
        });
        let steady_a = &reference[0].last().unwrap().rgba;
        assert_eq!(&once_a[..4], &[55, 36, 17, 255]);
        assert_eq!(&steady_a[..4], &[37, 18, 255, 255]);
        let mut bounds = [u16::MAX, u16::MAX, 0, 0];
        let mut changed = 0;
        for (index, (old, new)) in once_a
            .chunks_exact(4)
            .zip(steady_a.chunks_exact(4))
            .enumerate()
        {
            if old != new {
                let x = (index % w.w as usize) as u16;
                let y = (index / w.w as usize) as u16;
                bounds[0] = bounds[0].min(x);
                bounds[1] = bounds[1].min(y);
                bounds[2] = bounds[2].max(x);
                bounds[3] = bounds[3].max(y);
                changed += 1;
            }
        }
        assert_eq!(bounds, [0, 0, 64, 16]);
        assert_eq!(changed, 1105);
        for k in 0..4096 {
            for (index, s) in w.warm[k % 2].iter().enumerate() {
                let actual = apply(&mut st, &mut img, s, w.pointer).unwrap();
                let r = &reference[k % 2][index];
                assert_eq!(
                    format!("{actual:?}"),
                    r.outputs,
                    "{} steady outputs batch{k} step{index}",
                    w.name
                );
                assert!(
                    img.data() == r.rgba.as_slice(),
                    "{} complete RGBA equality batch{k} step{index}",
                    w.name
                );
            }
        }
        field(&mut output, w.name.as_bytes());
        let mut checkpoints = Vec::new();
        for (phase, rs) in reference.iter().enumerate() {
            output.extend((rs.len() as u32).to_le_bytes());
            for (step, r) in rs.iter().enumerate() {
                field(&mut output, &r.blob);
                field(&mut output, &encode_rows(&r.rgba, w.w as usize * 4));
                let mut e = Input(&r.blob);
                let status = e.byte();
                let image = e.take(32);
                let bgrx = e.take(32);
                let outputs = String::from_utf8(e.field().to_vec()).unwrap();
                let anchors = e.take(12);
                checkpoints.push(serde_json::json!({"phase":phase,"step":step,"status":status,"image_sha256":hex(image),"bgrx_sha256":hex(bgrx),"outputs":outputs,"literal_rgba_anchors":anchors}));
            }
        }
        proof.push(serde_json::json!({"name":w.name,"warmup_batches":64,"reference_batches_after_warmup":[0,1],"additional_verified_continuous_batches":4096,"actual_verified_step_calls":4096*(w.warm[0].len()+w.warm[1].len())/2,"all_steps_complete_rgba_and_outputs_equal":true,"initial_once_a_sha256":hex(&Sha256::digest(&once_a)),"steady_a_sha256":hex(&Sha256::digest(steady_a)),"literal_changed_bbox":bounds,"literal_changed_pixels":changed,"literal_initial_origin_rgba":[55,36,17,255],"literal_steady_origin_rgba":[37,18,255,255],"checkpoints":checkpoints}));
        println!("WARM_REFERENCE {} full_step_rgba_and_outputs_verified=4096batches bbox={bounds:?} changed={changed}",w.name);
    }
    let source =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/pipeline.rs")).unwrap();
    let exe = fs::read(std::env::current_exe().unwrap()).unwrap();
    fs::write(destination, &output).unwrap();
    manifest["warm_reference"] = serde_json::json!({"kind":"independent stock Stage period2 after64warm; first two reference batches plus4096 continuous batches all per-step completeRGBA+outputs equal; no reset/drop/production helper","file":destination.file_name().unwrap().to_str().unwrap(),"sha256":hex(&Sha256::digest(&output)),"base_bundle_sha256":hex(&Sha256::digest(&bundle)),"base_annotation_manifest_sha256":hex(&Sha256::digest(&manifest_bytes)),"producer_source_sha256":hex(&Sha256::digest(&source)),"producer_executable_sha256":hex(&Sha256::digest(&exe)),"command":args,"bgrx_contract":"[RGBA[2],RGBA[1],RGBA[0],255]; SHA calculated from independent full original bytes once per periodic step; all4096 live original fullRGBA images compared byte-for-byte to those references","proof":proof});
    fs::write(
        base.join("annotation-manifest-before-warm.json"),
        manifest_bytes,
    )
    .unwrap();
    fs::write(
        base.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

// Deterministic first-use row dictionary; large independent images stay target-only.
fn encode_rows(data: &[u8], row_len: usize) -> Vec<u8> {
    use std::collections::HashMap;
    assert_eq!(data.len() % row_len, 0);
    let mut dictionary: Vec<Vec<u8>> = Vec::new();
    let mut lookup = HashMap::new();
    let mut indices = Vec::new();
    for row in data.chunks_exact(row_len) {
        let index = if let Some(index) = lookup.get(row) {
            *index
        } else {
            let index = dictionary.len() as u32;
            dictionary.push(row.to_vec());
            lookup.insert(row.to_vec(), index);
            index
        };
        indices.push(index);
    }
    let mut out = Vec::new();
    out.extend((row_len as u32).to_le_bytes());
    out.extend((indices.len() as u32).to_le_bytes());
    out.extend((dictionary.len() as u32).to_le_bytes());
    for row in dictionary {
        field(&mut out, &row);
    }
    for index in indices {
        out.extend(index.to_le_bytes());
    }
    assert_eq!(decode_rows(&out), data);
    out
}
fn decode_rows(data: &[u8]) -> Vec<u8> {
    let mut c = Input(data);
    let row_len = c.u32();
    let rows = c.u32();
    let count = c.u32();
    assert!(row_len > 0 && row_len % 4 == 0 && count <= rows);
    let dictionary: Vec<_> = (0..count)
        .map(|_| {
            let row = c.field();
            assert_eq!(row.len(), row_len);
            row
        })
        .collect();
    let mut out = Vec::with_capacity(row_len * rows);
    for _ in 0..rows {
        out.extend(dictionary[c.u32()]);
    }
    assert!(c.0.is_empty());
    out
}
