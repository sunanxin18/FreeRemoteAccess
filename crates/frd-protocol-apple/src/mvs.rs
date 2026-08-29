//! MVS（Multi Variant Stream）generation-scoped 原生解码事务。
//!
//! 经逆向证据校正后的 type-0/type-1/type-2 wire grammar 由
//! [`crate::mvs_wire`] 独占；type-0/type-1 像素只通过原生 decoder
//! 的 prepare/apply/commit 边界发布。

use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::mvs_full::{
    DecodedMvsRect, MvsFullDecoder, PreparedMvsFull, PreparedMvsOpaqueState, PreparedPartialPixels,
};
use crate::mvs_stream::MvsRect;
use crate::mvs_wire::{self, MvsWirePayload};

pub const MVS_RGB_CHANNEL_BYTES: usize = 3;
pub const MVS_RGB_RED_OFFSET: usize = 0;
pub const MVS_RGB_GREEN_OFFSET: usize = 1;
pub const MVS_RGB_BLUE_OFFSET: usize = 2;
const _: () = assert!(MVS_RGB_BLUE_OFFSET < MVS_RGB_CHANNEL_BYTES);
pub const MVS_QUANTIZATION_TABLE_BYTES: usize = 64;
pub const MVS_TABLE_INITIALIZATION_BYTES: usize = 1 + 2 * MVS_QUANTIZATION_TABLE_BYTES;
/// 供被动捕获统计复用的 type-0 tag；不是多字节 magic signature。
pub const MVS_FULL_FRAME_SIGNATURE: [u8; 1] = [0x00];
pub const MAX_MVS_DECODE_PIXELS: usize =
    crate::protocol::limits::MAX_UPDATE_RAW_BYTES / MVS_RGB_CHANNEL_BYTES;

fn expected_mvs_rgb_bytes(width: u16, height: u16) -> Result<usize> {
    let pixels = usize::from(width)
        .checked_mul(usize::from(height))
        .context("MVS 解码像素数量溢出")?;
    if pixels == 0 || pixels > MAX_MVS_DECODE_PIXELS {
        bail!("MVS 解码尺寸超过资源预算: {width}x{height}");
    }
    pixels
        .checked_mul(MVS_RGB_CHANNEL_BYTES)
        .context("MVS RGB 字节数溢出")
}

pub fn validate_decoded_rgb_layout(width: u16, height: u16, actual_len: usize) -> Result<usize> {
    let expected = expected_mvs_rgb_bytes(width, height)?;
    if actual_len != expected {
        bail!("MVS RGB 长度不匹配: 实际 {actual_len}，期望 {expected}");
    }
    Ok(expected)
}

/// MVS 外层记录的严格分类结果。
#[derive(Debug, PartialEq, Eq)]
pub enum MvsRecordKind<'a> {
    Tables(&'a [u8]),
    Frame(&'a [u8]),
}

#[derive(Debug, PartialEq, Eq)]
pub enum MvsResyncReason {
    MissingTables,
    MalformedPayload,
}

#[derive(Debug)]
pub struct PreparedGenerationMvs {
    generation: u64,
    owner: Arc<()>,
    prepared: PreparedMvsFull,
}

#[derive(Debug)]
pub struct PreparedOpaqueMvsState {
    generation: u64,
    owner: Arc<()>,
    prepared: PreparedMvsOpaqueState,
}

impl PreparedGenerationMvs {
    pub fn decoded(&self) -> &DecodedMvsRect {
        MvsFullDecoder::decoded(&self.prepared)
    }
}

impl PreparedOpaqueMvsState {
    pub fn partial_pixels(&self) -> &PreparedPartialPixels {
        MvsFullDecoder::partial_pixels(&self.prepared)
    }
}

#[derive(Debug)]
// Keep the verified MVS publication path allocation-free; boxing the prepared
// generation would add a heap allocation to every decoded update.
#[allow(clippy::large_enum_variant)]
pub enum MvsDecodeDecision {
    Prepared(PreparedGenerationMvs),
    PreparedOpaque(PreparedOpaqueMvsState),
    RequestFull(MvsResyncReason),
    IgnoreStale,
}

/// Generation-scoped ownership for the transactional native MVS decoder.
pub struct MvsDecodeState {
    generation: u64,
    decoder: Option<MvsFullDecoder>,
    owner: Arc<()>,
    awaiting_full: bool,
}

impl MvsDecodeState {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            decoder: None,
            owner: Arc::new(()),
            awaiting_full: true,
        }
    }

    pub fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.decoder = None;
        self.owner = Arc::new(());
        self.awaiting_full = true;
    }

    pub fn install_tables(&mut self, generation: u64, init: &[u8]) -> Result<()> {
        if generation != self.generation {
            bail!("MVS 表属于过期 generation: {generation}");
        }
        self.awaiting_full = true;
        let MvsWirePayload::Tables(tables) = mvs_wire::parse_payload(init)? else {
            bail!("MVS 量化表初始化不是 type-2 记录");
        };
        if let Some(decoder) = self.decoder.as_mut() {
            decoder.replace_tables(tables);
        } else {
            self.decoder = Some(MvsFullDecoder::new(tables));
        }
        self.owner = Arc::new(());
        Ok(())
    }

    /// Record that a full-frame resynchronization has been requested.
    pub fn request_full(&mut self, generation: u64) -> Result<()> {
        if generation != self.generation {
            bail!("MVS 全量重同步属于过期 generation: {generation}");
        }
        self.awaiting_full = true;
        Ok(())
    }

    pub fn prepare(
        &mut self,
        generation: u64,
        payload: &[u8],
        width: u16,
        height: u16,
    ) -> Result<MvsDecodeDecision> {
        self.prepare_rect(
            generation,
            payload,
            MvsRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            width,
            height,
        )
    }

    pub fn prepare_rect(
        &mut self,
        generation: u64,
        payload: &[u8],
        rect: MvsRect,
        surface_width: u16,
        surface_height: u16,
    ) -> Result<MvsDecodeDecision> {
        if generation != self.generation {
            return Ok(MvsDecodeDecision::IgnoreStale);
        }
        let parsed = match mvs_wire::parse_payload(payload) {
            Ok(parsed) => parsed,
            Err(_) => {
                self.request_full(generation)?;
                return Ok(MvsDecodeDecision::RequestFull(
                    MvsResyncReason::MalformedPayload,
                ));
            }
        };
        let decision = match parsed {
            MvsWirePayload::Partial(_) | MvsWirePayload::Full(_) if self.decoder.is_none() => {
                MvsDecodeDecision::RequestFull(MvsResyncReason::MissingTables)
            }
            MvsWirePayload::Partial(partial) => {
                let decoder = self.decoder.as_ref().expect("decoder presence checked");
                let prepared = match decoder.prepare_partial(
                    partial,
                    usize::from(rect.x),
                    usize::from(rect.y),
                    usize::from(rect.width),
                    usize::from(rect.height),
                    usize::from(surface_width),
                    usize::from(surface_height),
                ) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        self.awaiting_full = true;
                        return Err(error.context("MVS type-1 incremental prepare 失败"));
                    }
                };
                return Ok(MvsDecodeDecision::PreparedOpaque(PreparedOpaqueMvsState {
                    generation,
                    owner: Arc::clone(&self.owner),
                    prepared,
                }));
            }
            MvsWirePayload::Full(full) => {
                let decoder = self.decoder.as_ref().expect("decoder presence checked");
                let prepared = match decoder.prepare_rect(
                    &full,
                    usize::from(rect.x),
                    usize::from(rect.y),
                    usize::from(rect.width),
                    usize::from(rect.height),
                    usize::from(surface_width),
                    usize::from(surface_height),
                ) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        self.awaiting_full = true;
                        return Err(error.context("MVS type-0 原生 prepare 失败"));
                    }
                };
                return Ok(MvsDecodeDecision::Prepared(PreparedGenerationMvs {
                    generation,
                    owner: Arc::clone(&self.owner),
                    prepared,
                }));
            }
            MvsWirePayload::Tables(_) => {
                self.awaiting_full = true;
                bail!("MVS type-2 表记录不能作为像素帧 prepare")
            }
        };
        if matches!(decision, MvsDecodeDecision::RequestFull(_)) {
            self.request_full(generation)?;
        }
        Ok(decision)
    }

    pub fn commit(&mut self, prepared: PreparedGenerationMvs) -> Result<DecodedMvsRect> {
        if prepared.generation != self.generation {
            bail!(
                "MVS prepared frame 属于过期 generation: {}",
                prepared.generation
            );
        }
        if !Arc::ptr_eq(&prepared.owner, &self.owner) {
            bail!("MVS prepared frame 不属于当前 decoder 状态");
        }
        let decoder = self
            .decoder
            .as_mut()
            .context("MVS commit 前缺少原生 decoder")?;
        let decoded = decoder.commit(prepared.prepared);
        // 每次 commit 都换 owner token，使同一基态派生的其他 preparation 失效。
        self.owner = Arc::new(());
        self.awaiting_full = false;
        Ok(decoded)
    }

    pub fn commit_opaque(
        &mut self,
        prepared: PreparedOpaqueMvsState,
    ) -> Result<PreparedPartialPixels> {
        if prepared.generation != self.generation {
            bail!(
                "MVS prepared opaque state 属于过期 generation: {}",
                prepared.generation
            );
        }
        if !Arc::ptr_eq(&prepared.owner, &self.owner) {
            bail!("MVS prepared opaque state 不属于当前 decoder 状态");
        }
        let decoder = self
            .decoder
            .as_mut()
            .context("MVS opaque commit 前缺少原生 decoder")?;
        let partial_pixels = decoder.commit_opaque(prepared.prepared);
        // Partial commit 也旋转 owner，但不改变 awaiting_full：type-1
        // 像素/cache 不能充当首个 type-0 codec 基态证据。
        self.owner = Arc::new(());
        Ok(partial_pixels)
    }

    pub fn awaiting_full(&self) -> bool {
        self.awaiting_full
    }
}

/// 严格区分 MVS 表初始化和像素帧，零尺寸畸形记录绝不能进入像素解码路径。
pub fn classify_mvs_record(rect: MvsRect, payload: &[u8]) -> Result<MvsRecordKind<'_>> {
    let zero_rectangle = rect.x == 0 && rect.y == 0 && rect.width == 0 && rect.height == 0;
    match mvs_wire::parse_payload(payload)? {
        MvsWirePayload::Tables(_) => {
            if zero_rectangle {
                Ok(MvsRecordKind::Tables(payload))
            } else {
                bail!("MVS type-2 表初始化矩形必须全部为零: {rect:?}")
            }
        }
        MvsWirePayload::Full(_) | MvsWirePayload::Partial(_) => {
            if rect.width == 0 || rect.height == 0 {
                bail!("MVS 零尺寸记录不能作为像素帧: {rect:?}");
            }
            Ok(MvsRecordKind::Frame(payload))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestBitWriter {
        bytes: Vec<u8>,
        current: u8,
        used: u8,
    }

    impl TestBitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                used: 0,
            }
        }

        fn write_bits(&mut self, value: u32, count: u8) {
            for shift in (0..count).rev() {
                self.current = (self.current << 1) | (((value >> shift) & 1) as u8);
                self.used += 1;
                if self.used == 8 {
                    self.bytes.push(self.current);
                    self.current = 0;
                    self.used = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.used != 0 {
                self.current <<= 8 - self.used;
                self.bytes.push(self.current);
            }
            self.bytes
        }
    }

    fn type_two_tables() -> [u8; MVS_TABLE_INITIALIZATION_BYTES] {
        let mut payload = [1u8; MVS_TABLE_INITIALIZATION_BYTES];
        payload[0] = 2;
        payload
    }

    fn mode_zero_full_payload() -> Vec<u8> {
        let mut mode = TestBitWriter::new();
        mode.write_bits(1, 1);
        mode.write_bits(0, 3);
        mode.write_bits(0, 1);
        mode.write_bits(0x6d, 8);
        let mode = mode.finish();

        let mut data = TestBitWriter::new();
        data.write_bits(0x6d, 8);
        let data = data.finish();
        let data_offset = 6 + mode.len();
        let mut payload = vec![
            0,
            0,
            0,
            u8::try_from(data_offset >> 16).unwrap(),
            u8::try_from((data_offset >> 8) & 0xff).unwrap(),
            u8::try_from(data_offset & 0xff).unwrap(),
        ];
        payload.extend_from_slice(&mode);
        payload.extend_from_slice(&data);
        payload
    }

    fn full_payload(mode_value: u32, data_fields: impl FnOnce(&mut TestBitWriter)) -> Vec<u8> {
        let mut mode = TestBitWriter::new();
        mode.write_bits(1, 1);
        mode.write_bits(mode_value, 3);
        mode.write_bits(0, 1);
        mode.write_bits(0x6d, 8);
        let mode = mode.finish();

        let mut data = TestBitWriter::new();
        data_fields(&mut data);
        data.write_bits(0x6d, 8);
        let data = data.finish();
        let data_offset = 6 + mode.len();
        let mut payload = vec![
            0,
            5,
            0,
            u8::try_from(data_offset >> 16).unwrap(),
            u8::try_from((data_offset >> 8) & 0xff).unwrap(),
            u8::try_from(data_offset & 0xff).unwrap(),
        ];
        payload.extend_from_slice(&mode);
        payload.extend_from_slice(&data);
        payload
    }

    fn mode_five_seed_full_payload() -> Vec<u8> {
        full_payload(5, |data| {
            data.write_bits(0, 3); // non-copy, no chroma reuse, threshold A
            data.write_bits(0, 2); // Cb DC zero
            data.write_bits(0, 2); // Cr DC zero
            data.write_bits(0, 2); // Y DC zero
            data.write_bits(0b0010, 4); // Y AC EOB
        })
    }

    fn mode_six_full_payload(index: u16) -> Vec<u8> {
        full_payload(6, |data| {
            data.write_bits(u32::from(index >> 8), 8);
            data.write_bits(u32::from(index & 0xff), 8);
        })
    }

    fn partial_payload(fields: impl FnOnce(&mut TestBitWriter), cb: u8, cr: u8) -> Vec<u8> {
        let mut bits = TestBitWriter::new();
        fields(&mut bits);
        bits.write_bits(0x6d, 8);
        bits.write_bits(0x76, 8);
        bits.write_bits(0x73, 8);
        let mut payload = vec![1, cb, cr];
        payload.extend(bits.finish());
        payload
    }

    fn opcode_one_partial_payload() -> Vec<u8> {
        partial_payload(
            |bits| {
                bits.write_bits(1, 2);
                bits.write_bits(0, 6);
                bits.write_bits(0, 1);
                bits.write_bits(0b1010, 4);
                bits.write_bits(0, 1);
                bits.write_bits(0b1010, 4);
            },
            1,
            1,
        )
    }

    fn opcode_zero_partial_payload() -> Vec<u8> {
        partial_payload(|bits| bits.write_bits(0, 2), 0, 0)
    }

    fn prepared(decision: MvsDecodeDecision) -> PreparedGenerationMvs {
        let MvsDecodeDecision::Prepared(prepared) = decision else {
            panic!("expected native prepared MVS frame");
        };
        prepared
    }

    fn opaque(decision: MvsDecodeDecision) -> PreparedOpaqueMvsState {
        let MvsDecodeDecision::PreparedOpaque(prepared) = decision else {
            panic!("expected native prepared opaque MVS state");
        };
        prepared
    }

    #[test]
    fn native_type_zero_before_tables_requests_full() {
        let mut state = MvsDecodeState::new(7);
        assert!(matches!(
            state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap(),
            MvsDecodeDecision::RequestFull(MvsResyncReason::MissingTables)
        ));
        assert!(state.awaiting_full());
    }

    #[test]
    fn type_two_installation_preserves_native_tile_and_cache_state() {
        let mut state = MvsDecodeState::new(7);
        assert_eq!(state.install_tables(7, &type_two_tables()).unwrap(), ());
        let seed = prepared(
            state
                .prepare(7, &mode_five_seed_full_payload(), 8, 8)
                .unwrap(),
        );
        state.commit(seed).unwrap();
        let partial = opaque(
            state
                .prepare(7, &opcode_one_partial_payload(), 8, 8)
                .unwrap(),
        );
        state.commit_opaque(partial).unwrap();

        state.install_tables(7, &type_two_tables()).unwrap();

        assert!(state.awaiting_full());
        assert!(matches!(
            state.prepare(7, &mode_six_full_payload(1), 8, 8).unwrap(),
            MvsDecodeDecision::Prepared(_)
        ));
    }

    #[test]
    fn native_subrectangle_prepares_rgb_while_codec_awaits_full() {
        let mut state = MvsDecodeState::new(7);
        state.install_tables(7, &type_two_tables()).unwrap();
        let prepared = prepared(state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        assert_eq!(prepared.decoded().rgb, vec![255; 8 * 8 * 3]);
        assert!(state.awaiting_full());
    }

    #[test]
    fn dropped_preparation_leaves_committed_state_unchanged() {
        let mut state = MvsDecodeState::new(7);
        state.install_tables(7, &type_two_tables()).unwrap();
        let candidate = prepared(state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        drop(candidate);
        assert!(state.awaiting_full());

        let replacement = prepared(state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        assert_eq!(replacement.decoded().rgb, vec![255; 8 * 8 * 3]);
    }

    #[test]
    fn commit_is_generation_bound_and_clears_codec_awaiting_full() {
        let mut state = MvsDecodeState::new(7);
        state.install_tables(7, &type_two_tables()).unwrap();
        let prepared = prepared(state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        let decoded = state.commit(prepared).unwrap();
        assert_eq!(decoded.rgb, vec![255; 8 * 8 * 3]);
        assert!(!state.awaiting_full());
    }

    #[test]
    fn type_one_prepares_pixels_and_commit_installs_cache_atomically() {
        let mut state = MvsDecodeState::new(7);
        state.install_tables(7, &type_two_tables()).unwrap();
        let full = prepared(
            state
                .prepare(7, &mode_five_seed_full_payload(), 8, 8)
                .unwrap(),
        );
        state.commit(full).unwrap();
        assert!(!state.awaiting_full());

        let first = opaque(
            state
                .prepare(7, &opcode_one_partial_payload(), 8, 8)
                .unwrap(),
        );
        assert_eq!(first.partial_pixels().operations.len(), 1);
        let same_owner = opaque(
            state
                .prepare(7, &opcode_one_partial_payload(), 8, 8)
                .unwrap(),
        );
        assert_eq!(state.commit_opaque(first).unwrap().operations.len(), 1);
        assert!(!state.awaiting_full());
        assert!(state.commit_opaque(same_owner).is_err());

        assert!(matches!(
            state.prepare(7, &mode_six_full_payload(1), 8, 8).unwrap(),
            MvsDecodeDecision::Prepared(_)
        ));
    }

    #[test]
    fn slice_d_partial_drop_stale_wrong_owner_and_awaiting_full_are_transactional() {
        let mut state = MvsDecodeState::new(7);
        state.install_tables(7, &type_two_tables()).unwrap();
        let full = prepared(
            state
                .prepare(7, &mode_five_seed_full_payload(), 8, 8)
                .unwrap(),
        );
        let committed_pixels = full.decoded().rgb.clone();
        state.commit(full).unwrap();

        let dropped = opaque(
            state
                .prepare(7, &opcode_one_partial_payload(), 8, 8)
                .unwrap(),
        );
        drop(dropped);
        assert!(!state.awaiting_full());
        let cache_miss = prepared(state.prepare(7, &mode_six_full_payload(1), 8, 8).unwrap());
        assert_eq!(cache_miss.decoded().rgb, committed_pixels);

        assert!(matches!(
            state
                .prepare(6, &opcode_zero_partial_payload(), 8, 8)
                .unwrap(),
            MvsDecodeDecision::IgnoreStale
        ));

        let wrong_owner = opaque(
            state
                .prepare(7, &opcode_zero_partial_payload(), 8, 8)
                .unwrap(),
        );
        let mut other = MvsDecodeState::new(7);
        other.install_tables(7, &type_two_tables()).unwrap();
        assert!(other.commit_opaque(wrong_owner).is_err());
        assert!(other.awaiting_full());

        state.request_full(7).unwrap();
        let preserving = opaque(
            state
                .prepare(7, &opcode_zero_partial_payload(), 8, 8)
                .unwrap(),
        );
        state.commit_opaque(preserving).unwrap();
        assert!(state.awaiting_full());
    }

    #[test]
    fn slice_d_malformed_partial_requests_full_but_preserves_committed_cache() {
        let mut state = MvsDecodeState::new(7);
        state.install_tables(7, &type_two_tables()).unwrap();
        let full = prepared(
            state
                .prepare(7, &mode_five_seed_full_payload(), 8, 8)
                .unwrap(),
        );
        state.commit(full).unwrap();
        let opaque = opaque(
            state
                .prepare(7, &opcode_one_partial_payload(), 8, 8)
                .unwrap(),
        );
        state.commit_opaque(opaque).unwrap();

        let mut malformed = opcode_zero_partial_payload();
        malformed.pop();
        assert!(state.prepare(7, &malformed, 8, 8).is_err());
        assert!(state.awaiting_full());
        assert!(matches!(
            state.prepare(7, &mode_six_full_payload(1), 8, 8).unwrap(),
            MvsDecodeDecision::Prepared(_)
        ));
    }

    #[test]
    fn malformed_type_zero_preserves_decoder_and_marks_awaiting_full() {
        let mut state = MvsDecodeState::new(7);
        state.install_tables(7, &type_two_tables()).unwrap();
        let first = prepared(state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        state.commit(first).unwrap();
        assert!(!state.awaiting_full());

        let mut malformed = mode_zero_full_payload();
        malformed.pop();
        assert!(state.prepare(7, &malformed, 8, 8).is_err());
        assert!(state.awaiting_full());

        let replacement = prepared(state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        assert_eq!(replacement.decoded().rgb, vec![255; 8 * 8 * 3]);
    }

    #[test]
    fn reset_invalidates_old_tables_and_preparation() {
        let mut state = MvsDecodeState::new(7);
        state.install_tables(7, &type_two_tables()).unwrap();
        let old = prepared(state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        state.reset(8);

        assert!(state.commit(old).is_err());
        assert!(matches!(
            state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap(),
            MvsDecodeDecision::IgnoreStale
        ));
        assert!(matches!(
            state.prepare(8, &mode_zero_full_payload(), 8, 8).unwrap(),
            MvsDecodeDecision::RequestFull(MvsResyncReason::MissingTables)
        ));
    }

    #[test]
    fn preparation_cannot_commit_into_another_same_generation_state() {
        let mut source = MvsDecodeState::new(7);
        source.install_tables(7, &type_two_tables()).unwrap();
        let prepared = prepared(source.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        let mut other = MvsDecodeState::new(7);
        other.install_tables(7, &type_two_tables()).unwrap();

        assert!(other.commit(prepared).is_err());
        assert!(other.awaiting_full());
    }

    #[test]
    fn failed_consumer_can_drop_then_decode_identically() {
        let mut state = MvsDecodeState::new(7);
        state.install_tables(7, &type_two_tables()).unwrap();
        let dropped = prepared(state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        let expected = dropped.decoded().clone();
        drop(dropped);
        state.request_full(7).unwrap();

        let retry = prepared(state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap());
        assert_eq!(retry.decoded(), &expected);
        assert_eq!(state.commit(retry).unwrap(), expected);
    }

    #[test]
    fn rgb_channel_offsets_match_independent_rgb24_layout() {
        assert_eq!(
            [
                MVS_RGB_RED_OFFSET,
                MVS_RGB_GREEN_OFFSET,
                MVS_RGB_BLUE_OFFSET,
            ],
            [0, 1, 2]
        );
    }

    #[test]
    fn decoded_rgb_layout_requires_exact_bounded_dimensions() {
        assert_eq!(validate_decoded_rgb_layout(10, 20, 600).unwrap(), 600);
        assert!(validate_decoded_rgb_layout(10, 20, 599).is_err());
        assert!(validate_decoded_rgb_layout(u16::MAX, u16::MAX, 0).is_err());
    }

    #[test]
    fn mvs_table_classifier_rejects_nonzero_or_zero_sized_rectangles() {
        let table = [2; MVS_TABLE_INITIALIZATION_BYTES];
        assert!(matches!(
            classify_mvs_record(
                MvsRect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                &table,
            ),
            Ok(MvsRecordKind::Tables(_))
        ));
        assert!(classify_mvs_record(
            MvsRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            &[0; MVS_TABLE_INITIALIZATION_BYTES],
        )
        .is_err());
        for rect in [
            MvsRect {
                x: 1,
                y: 0,
                width: 0,
                height: 0,
            },
            MvsRect {
                x: 0,
                y: 1,
                width: 0,
                height: 0,
            },
            MvsRect {
                x: 0,
                y: 0,
                width: 1,
                height: 0,
            },
            MvsRect {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            },
        ] {
            assert!(classify_mvs_record(rect, &table).is_err());
        }
        assert!(classify_mvs_record(
            MvsRect {
                x: 1,
                y: 2,
                width: 0,
                height: 3,
            },
            &[0; 7],
        )
        .is_err());
        assert!(matches!(
            classify_mvs_record(
                MvsRect {
                    x: 1,
                    y: 2,
                    width: 3,
                    height: 4,
                },
                &[0, 1, 2, 0, 0, 7, 0xaa],
            ),
            Ok(MvsRecordKind::Frame(_))
        ));
    }

    #[test]
    fn classifier_does_not_mistake_129_byte_type_zero_for_tables() {
        let mut full = [0; MVS_TABLE_INITIALIZATION_BYTES];
        full[1] = 15;
        full[2] = 25;
        full[5] = 7;
        full[6] = 0xaa;

        assert!(matches!(
            classify_mvs_record(
                MvsRect {
                    x: 1,
                    y: 2,
                    width: 3,
                    height: 4,
                },
                &full,
            ),
            Ok(MvsRecordKind::Frame(payload)) if payload == full
        ));
    }

    #[test]
    fn malformed_mvs_zero_rectangle_never_becomes_a_frame() {
        let zero_rectangle = MvsRect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };

        assert!(classify_mvs_record(zero_rectangle, &[0; 128]).is_err());
        assert!(classify_mvs_record(zero_rectangle, &[0; 130]).is_err());
        assert!(classify_mvs_record(
            MvsRect {
                x: 2,
                y: 3,
                width: 0,
                height: 4,
            },
            &[0; 7],
        )
        .is_err());
    }
}
