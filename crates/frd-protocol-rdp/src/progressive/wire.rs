//! Progressive 容器的有界解析；不修改解码状态，也不解包熵编码。
//! MS-RDPEGFX 2.2.4.2.1：每层 blockLen/tileDataSize 必须精确消费。
use ironrdp::core::{Decode, ReadCursor};
use ironrdp::pdu::codecs::rfx::{progressive::*, RfxRectangle};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ITEMS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WireError {
    Length,
    Budget,
    Structure,
    Field,
}

fn take<'a>(src: &mut ReadCursor<'a>, n: usize) -> Result<&'a [u8], WireError> {
    if src.len() < n {
        return Err(WireError::Length);
    }
    Ok(src.read_slice(n))
}
fn exact<'a, T: Decode<'a>>(bytes: &'a [u8]) -> Result<T, WireError> {
    let mut src = ReadCursor::new(bytes);
    let value = T::decode(&mut src).map_err(|_| WireError::Field)?;
    if !src.is_empty() {
        return Err(WireError::Length);
    }
    Ok(value)
}
fn block<'a>(src: &mut ReadCursor<'a>) -> Result<(u16, &'a [u8]), WireError> {
    let header = take(src, 6)?;
    let kind = u16::from_le_bytes([header[0], header[1]]);
    let length = u32::from_le_bytes(header[2..6].try_into().unwrap());
    let body = usize::try_from(length.checked_sub(6).ok_or(WireError::Length)?)
        .map_err(|_| WireError::Length)?;
    Ok((kind, take(src, body)?))
}

fn region(bytes: &[u8]) -> Result<ProgressiveRegion<'_>, WireError> {
    let mut src = ReadCursor::new(bytes);
    let header = take(&mut src, 12)?;
    let count_rects = usize::from(u16::from_le_bytes([header[1], header[2]]));
    let count_quant = usize::from(header[3]);
    let count_progressive = usize::from(header[4]);
    let count_tiles = usize::from(u16::from_le_bytes([header[6], header[7]]));
    let tile_bytes = usize::try_from(u32::from_le_bytes(header[8..12].try_into().unwrap()))
        .map_err(|_| WireError::Length)?;
    if header[0] != 64 || count_rects == 0 || count_quant > 7 {
        return Err(WireError::Field);
    }
    if count_rects > MAX_ITEMS || count_tiles > MAX_ITEMS {
        return Err(WireError::Budget);
    }
    let mut rects = Vec::with_capacity(count_rects);
    for _ in 0..count_rects {
        rects.push(exact::<RfxRectangle>(take(&mut src, 8)?)?);
    }
    let mut quant_vals = Vec::with_capacity(count_quant);
    for _ in 0..count_quant {
        quant_vals.push(exact::<ComponentCodecQuant>(take(&mut src, 5)?)?);
    }
    let mut quant_prog_vals = Vec::with_capacity(count_progressive);
    for _ in 0..count_progressive {
        quant_prog_vals.push(exact::<ProgressiveCodecQuant>(take(&mut src, 16)?)?);
    }
    let mut tile_src = ReadCursor::new(take(&mut src, tile_bytes)?);
    if !src.is_empty() {
        return Err(WireError::Length);
    }
    let mut tiles = Vec::with_capacity(count_tiles);
    for _ in 0..count_tiles {
        let (kind, body) = block(&mut tile_src)?;
        let tile = match kind {
            0xccc5 => ProgressiveTile::Simple(exact::<TileSimple<'_>>(body)?),
            0xccc6 => ProgressiveTile::First(exact::<TileFirst<'_>>(body)?),
            0xccc7 => ProgressiveTile::Upgrade(exact::<TileUpgrade<'_>>(body)?),
            _ => return Err(WireError::Structure),
        };
        tiles.push(tile);
    }
    if !tile_src.is_empty() {
        return Err(WireError::Length);
    }
    Ok(ProgressiveRegion {
        tile_size: header[0],
        rects,
        quant_vals,
        quant_prog_vals,
        flags: header[5],
        tiles,
    })
}

/// 保留块顺序供会话层验证；允许 CONTEXT 位于 FRAME_BEGIN 之后。
/// 会话层负责帧状态、quant 索引、区域覆盖和跨消息上下文验证。
pub(super) fn decode(data: &[u8]) -> Result<Vec<ProgressiveBlock<'_>>, WireError> {
    if data.len() > MAX_BYTES {
        return Err(WireError::Budget);
    }
    if data.is_empty() {
        return Err(WireError::Length);
    }
    let mut src = ReadCursor::new(data);
    let mut blocks = Vec::new();
    let mut total_tiles = 0usize;
    while !src.is_empty() {
        if blocks.len() >= MAX_ITEMS {
            return Err(WireError::Budget);
        }
        let (kind, body) = block(&mut src)?;
        let item = match kind {
            0xccc0 => {
                // MS-RDPEGFX 2.2.4.2.1.1：应忽略magic/version及重复、乱序SYNC。
                // 仍要求精确的块长度，不能借此容忍截断。
                if body.len() != 6 {
                    return Err(WireError::Length);
                }
                ProgressiveBlock::Sync(ProgressiveSyncPdu)
            }
            0xccc1 => ProgressiveBlock::FrameBegin(exact(body)?),
            0xccc2 => ProgressiveBlock::FrameEnd(exact(body)?),
            0xccc3 => ProgressiveBlock::Context(exact(body)?),
            0xccc4 => {
                let region = region(body)?;
                total_tiles = total_tiles
                    .checked_add(region.tiles.len())
                    .ok_or(WireError::Budget)?;
                if total_tiles > MAX_ITEMS {
                    return Err(WireError::Budget);
                }
                ProgressiveBlock::Region(region)
            }
            // 未识别扩展明确不可用，禁止跳过后把缺失图像当作成功。
            _ => return Err(WireError::Structure),
        };
        blocks.push(item);
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn encoded() -> Vec<u8> {
        encode_progressive_stream(&[
            ProgressiveBlock::Sync(ProgressiveSyncPdu),
            ProgressiveBlock::Context(ProgressiveContextPdu {
                context_id: 0,
                tile_size: 64,
                flags: 1,
            }),
            ProgressiveBlock::Region(ProgressiveRegion {
                tile_size: 64,
                rects: vec![RfxRectangle {
                    x: 0,
                    y: 0,
                    width: 64,
                    height: 64,
                }],
                quant_vals: vec![],
                quant_prog_vals: vec![],
                flags: 1,
                tiles: vec![],
            }),
        ])
        .unwrap()
    }
    #[test]
    fn consumes_exact_containers_and_preserves_context_flag() {
        let data = encoded();
        let blocks = decode(&data).unwrap();
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[1], ProgressiveBlock::Context(c) if c.flags == 1));
    }
    #[test]
    fn ignores_sync_magic_version_and_repeated_out_of_sequence_sync() {
        let mut sync =
            encode_progressive_stream(&[ProgressiveBlock::Sync(ProgressiveSyncPdu)]).unwrap();
        sync[6..12].copy_from_slice(&[0, 1, 2, 3, 0xff, 0xff]);
        assert_eq!(
            decode(&sync).unwrap(),
            vec![ProgressiveBlock::Sync(ProgressiveSyncPdu)]
        );
        let mut data = encoded();
        data.extend_from_slice(&sync);
        data.extend_from_slice(&sync);
        let blocks = decode(&data).unwrap();
        assert_eq!(blocks.len(), 5);
        assert!(matches!(blocks[3], ProgressiveBlock::Sync(_)));
        assert!(matches!(blocks[4], ProgressiveBlock::Sync(_)));
    }
    #[test]
    fn ignored_sync_fields_do_not_relax_envelope_size_or_truncation() {
        for length in [6u32, 11, 13] {
            let mut bytes = vec![0xc0, 0xcc];
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes.resize(length as usize, 0);
            assert_eq!(decode(&bytes), Err(WireError::Length));
        }
        let sync =
            encode_progressive_stream(&[ProgressiveBlock::Sync(ProgressiveSyncPdu)]).unwrap();
        for length in 0..sync.len() {
            assert_eq!(decode(&sync[..length]), Err(WireError::Length));
        }
        let mut excessive = sync.repeat(MAX_ITEMS + 1);
        // 非标准magic仍计入块数量预算。
        excessive[6] = 0;
        assert_eq!(decode(&excessive), Err(WireError::Budget));
    }
    #[test]
    fn rejects_trailing_partial_header_and_truncated_body() {
        let mut data = encoded();
        data.push(0);
        assert_eq!(decode(&data), Err(WireError::Length));
        data.truncate(data.len() - 2);
        assert_eq!(decode(&data), Err(WireError::Length));
    }
    #[test]
    fn rejects_tile_data_size_that_does_not_match_region() {
        let mut data = encoded();
        // SYNC 12 + CONTEXT 10 + REGION header 6 + tileDataSize offset 8.
        data[36..40].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(decode(&data), Err(WireError::Length));
    }
    #[test]
    fn rejects_extra_bytes_inside_tile_even_when_outer_lengths_match() {
        let region = ProgressiveRegion {
            tile_size: 64,
            rects: vec![RfxRectangle {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            }],
            quant_vals: vec![ComponentCodecQuant::LOSSLESS],
            quant_prog_vals: vec![],
            flags: 0,
            tiles: vec![ProgressiveTile::Simple(TileSimple {
                quant_idx_y: 0,
                quant_idx_cb: 0,
                quant_idx_cr: 0,
                x_idx: 0,
                y_idx: 0,
                flags: 0,
                y_data: &[1],
                cb_data: &[2],
                cr_data: &[3],
                tail_data: &[],
            })],
        };
        let mut data = encode_progressive_stream(&[ProgressiveBlock::Region(region)]).unwrap();
        assert!(decode(&data).is_ok());
        // 外层与 tileDataSize 均包含额外字节，仍要求 Tile 自身字段精确消费。
        let length = data.len() as u32 + 1;
        data[2..6].copy_from_slice(&length.to_le_bytes());
        let tile_len = u32::from_le_bytes(data[14..18].try_into().unwrap()) + 1;
        data[14..18].copy_from_slice(&tile_len.to_le_bytes());
        let tile_start = 6 + 12 + 8 + 5;
        data[tile_start + 2..tile_start + 6].copy_from_slice(&tile_len.to_le_bytes());
        data.push(0);
        assert_eq!(decode(&data), Err(WireError::Length));
    }
    #[test]
    fn rejects_excessive_block_count_before_growing_unbounded_metadata() {
        let frame_end = [0xc2, 0xcc, 6, 0, 0, 0];
        let data = frame_end.repeat(MAX_ITEMS + 1);
        assert_eq!(decode(&data), Err(WireError::Budget));
    }
    #[test]
    fn rejects_unknown_block_and_empty_input() {
        assert_eq!(decode(&[]), Err(WireError::Length));
        assert_eq!(decode(&[0, 0, 6, 0, 0, 0]), Err(WireError::Structure));
    }
}
