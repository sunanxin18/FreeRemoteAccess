// 原版/生产 public API 观察器；不包含替代 decoder 或生产派生 expected。
use ironrdp_core::{Decode as _, ReadCursor};
use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdate, FastPathUpdatePdu};
use ironrdp_pdu::surface_commands::{ExtendedBitmapDataPdu, SurfaceCommand};

fn extended(flags: u8, declared: u32, actual: usize) -> Vec<u8> {
    let mut v = vec![32, flags, 0xa5, 0];
    v.extend(1u16.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(declared.to_le_bytes());
    // 四字段使用不同的字节，暴露端序混淆；无 header 时作为普通 data。
    v.extend((0..actual).map(|i| u8::try_from(i % 256).unwrap()));
    v
}

fn command(ext: &[u8], kind: u8) -> Vec<u8> {
    let mut v = vec![kind, 0, 0, 0, 0, 0, 1, 0, 1, 0];
    v.extend(ext);
    v
}

fn frame(command: &[u8]) -> Vec<u8> {
    let mut v = vec![0, u8::try_from(command.len() + 5).unwrap(), 4];
    v.extend(u16::try_from(command.len()).unwrap().to_le_bytes());
    v.extend(command);
    v
}

fn value(v: &ExtendedBitmapDataPdu<'_>, input: &[u8]) -> String {
    let offset = v.data.as_ptr().addr() - input.as_ptr().addr();
    format!(
        "bpp={} codec={} width={} height={} header={:?} data={:?} borrowed_offset={offset}",
        v.bpp, v.codec_id, v.width, v.height, v.header, v.data
    )
}

fn observe(ext: &[u8], route: u8) -> String {
    if route == 0 {
        let mut cursor = ReadCursor::new(ext);
        let result = match ExtendedBitmapDataPdu::decode(&mut cursor) {
            Ok(v) => format!("Ok {}", value(&v, ext)),
            Err(e) => format!("Err {:?}", e.kind()),
        };
        return format!("{result} consumed={}", ext.len() - cursor.len());
    }
    let cmd = command(ext, if route == 1 || route == 3 { 1 } else { 6 });
    if route <= 2 {
        let mut cursor = ReadCursor::new(&cmd);
        let result = match SurfaceCommand::decode(&mut cursor) {
            Ok(SurfaceCommand::SetSurfaceBits(v)) => {
                format!("Ok Set {}", value(&v.extended_bitmap_data, &cmd))
            }
            Ok(SurfaceCommand::StreamSurfaceBits(v)) => {
                format!("Ok Stream {}", value(&v.extended_bitmap_data, &cmd))
            }
            Ok(_) => unreachable!(),
            Err(e) => format!("Err {:?}", e.kind()),
        };
        return format!("{result} consumed={}", cmd.len() - cursor.len());
    }
    let packet = frame(&cmd);
    let mut cursor = ReadCursor::new(&packet);
    let header = FastPathHeader::decode(&mut cursor).unwrap();
    let update = FastPathUpdatePdu::decode(&mut cursor).unwrap();
    assert!(cursor.is_empty());
    let result = match FastPathUpdate::decode_with_code(update.data, update.update_code) {
        Ok(FastPathUpdate::SurfaceCommands(v)) => {
            assert_eq!(v.len(), 1);
            match &v[0] {
                SurfaceCommand::SetSurfaceBits(v) => {
                    format!("Ok Set {}", value(&v.extended_bitmap_data, &packet))
                }
                SurfaceCommand::StreamSurfaceBits(v) => {
                    format!("Ok Stream {}", value(&v.extended_bitmap_data, &packet))
                }
                _ => unreachable!(),
            }
        }
        Ok(_) => unreachable!(),
        Err(e) => format!("Err {:?}", e.kind()),
    };
    format!("{result} outer={header:?} consumed={}", packet.len())
}

fn records() -> String {
    let mut out = String::new();
    for flags in [0u8, 1, 0x80, 0x81] {
        for declared in [0u32, 4, u32::MAX - 24, u32::MAX - 23, u32::MAX] {
            for actual in [0usize, 3, 4, 23, 24, 27, 28, 29] {
                let ext = extended(flags, declared, actual);
                for route in 0..5 {
                    out.push_str(&format!(
                        "flags={flags} declared={declared} actual={actual} route={route}\t{}\n",
                        observe(&ext, route)
                    ));
                }
            }
        }
    }
    let fixed = extended(1, 4, 28);
    for end in 0..12 {
        for route in 0..5 {
            out.push_str(&format!(
                "fixed-short={end} route={route}\t{}\n",
                observe(&fixed[..end], route)
            ));
        }
    }
    out
}

fn main() {
    // 原版不受信长度观察仅在独立 process 内；64 位无 overflow，最大输入 56 byte。
    assert_eq!(usize::BITS, 64, "原版 freeze 当前仅支持实跑 64 位");
    std::fs::write(std::env::args().nth(1).unwrap(), records()).unwrap();
}

#[test]
fn public_extended_set_stream_fastpath_match_frozen_original() {
    assert_eq!(usize::BITS, 64);
    assert_eq!(records(), include_str!("fixtures/surface-original-64.txt"));
}

#[test]
fn legal_header_endian_borrowed_slice_and_trailing_position() {
    for flags in [0u8, 1, 0x80, 0x81] {
        for length in [0u32, 4] {
            let header_len = if flags & 1 != 0 { 24 } else { 0 };
            let input = extended(
                flags,
                length,
                header_len + usize::try_from(length).unwrap() + 1,
            );
            let mut cursor = ReadCursor::new(&input);
            let v = ExtendedBitmapDataPdu::decode(&mut cursor).unwrap();
            assert_eq!(cursor.len(), 1);
            assert_eq!(v.data, &input[12 + header_len..input.len() - 1]);
            assert_eq!(v.data.as_ptr(), input[12 + header_len..].as_ptr());
            if let Some(h) = v.header {
                assert_eq!(h.high_unique_id, 0x03020100);
                assert_eq!(h.low_unique_id, 0x07060504);
                assert_eq!(h.tm_milliseconds, 0x0f0e0d0c0b0a0908);
                assert_eq!(h.tm_seconds, 0x1716151413121110);
            } else {
                assert_eq!(flags & 1, 0);
            }
        }
    }
}
