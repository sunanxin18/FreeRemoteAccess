//! RDPGFX/EGFX registration and surface-output seam.
//!
//! This module owns the IronRDP graphics-pipeline DVC boundary. The default
//! connector remains legacy-only until a real decoder and live capability gate
//! are supplied. The surface publisher below is deliberately queue based: the
//! IronRDP DVC processor is driven from the session thread, while
//! `ProtocolRuntime` owns generation admission and publication. This keeps the
//! EGFX handler independent from a renderer or a platform window API.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use frd_core::{PixelRect, PixelSize, SessionId};
use frd_frame::{FrameCompleteness, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};
use frd_media_api::{
    ChromaFormat, ChromaLocation, DecodeOutcome, EncodedVideoAccessUnit, VideoBackendAvailability,
    VideoBitstreamFormat, VideoCodec, VideoColorimetry, VideoDecodeQuery, VideoDecoder,
    VideoDecoderFactory, VideoParameterSets, VideoPixelFormat, VideoProfile, VideoRange,
    VideoStreamConfig, VideoStreamConfigInput, VideoStreamIdentity, VideoTimeBase, VideoTimestamp,
};
use ironrdp::core::impl_as_any;
use ironrdp::dvc::{DvcMessage, DvcProcessor};
use ironrdp::pdu::geometry::{ExclusiveRectangle, InclusiveRectangle, Rectangle};
use ironrdp::pdu::{Decode, PduResult, ReadCursor};
use ironrdp_egfx::client::{
    BitmapUpdate, GraphicsPipelineClient, GraphicsPipelineHandler, Surface,
};
use ironrdp_egfx::decode::{DecodedFrame, DecoderError, DecoderResult, H264Decoder};
use ironrdp_egfx::pdu::{Avc444BitmapStream, Encoding as Avc444Encoding, GfxPdu};
use ironrdp_egfx::pdu::{
    CapabilitiesV107Flags, CapabilitiesV81Flags, CapabilitiesV8Flags, CapabilitySet, Codec1Type,
};

#[path = "egfx_backing.rs"]
mod backing;

use crate::avc444::{
    Avc444ChromaLayout, Avc444ReconstructionMode, Yuv420Frame, Yuv420Plane, Yuv444Frame,
    Yuv444Reconstructor,
};
use crate::factory::{RdpEgfxDiagnostics, RdpEgfxFailure};
use crate::pixel_convert::convert_rgba_to_bgrx;
use crate::surface::validate_surface_size;
use crate::yuv_convert::{convert_yuv420_to_rgba, convert_yuv444_to_rgba};

/// Application-owned H.264 decoder construction boundary for EGFX.
///
/// The RDP adapter never selects FFmpeg, a platform framework, or a hardware
/// API itself.  A composition root may provide a decoder only after it has
/// applied its exact backend/profile/architecture gates.  Returning `None`
/// keeps the connection on the legacy Bitmap/RemoteFX path and therefore
/// cannot cause an unbacked AVC capability advertisement.
#[allow(private_interfaces)]
pub trait EgfxDecoderProvider: Send + Sync {
    fn create_decoder(
        &self,
        session_id: SessionId,
        coded_size: PixelSize,
    ) -> Option<Box<dyn H264Decoder>>;

    /// Optional AVC444 provider. The default keeps existing providers
    /// AVC420-only and therefore cannot change capability advertisement.
    fn create_avc444_decoder(
        &self,
        _session_id: SessionId,
        _coded_size: PixelSize,
    ) -> Option<Box<dyn Avc444Decoder>> {
        None
    }
}

/// Optional AVC444 decoder boundary.
///
/// IronRDP 0.3 forwards AVC444 and AVC444v2 wire PDUs to the graphics handler
/// instead of decoding them.  This trait keeps that handoff explicit and
/// protocol-local: an implementation owns two YUV420 sub-stream decoders and
/// the YUV420/Chroma420 reconstruction, while the publisher owns only
/// generation, surface and RGBA-to-BGRX publication.  No concrete provider is
/// installed until its exact decoder and live-interoperability gates are met.
pub trait Avc444Decoder: Send {
    fn reset(&mut self, width: u32, height: u32) -> DecoderResult<()>;

    fn on_surface_created(&mut self, _surface: &Surface) -> DecoderResult<()> {
        Ok(())
    }

    fn on_surface_deleted(&mut self, _surface_id: u16) -> DecoderResult<()> {
        Ok(())
    }

    fn on_surface_mapped(
        &mut self,
        _surface_id: u16,
        _origin_x: u32,
        _origin_y: u32,
    ) -> DecoderResult<()> {
        Ok(())
    }

    fn decode(
        &mut self,
        codec_id: Codec1Type,
        surface_id: u16,
        bitmap: &ValidatedAvc444Bitmap<'_>,
        destination: &ExclusiveRectangle,
    ) -> DecoderResult<DecodedFrame>;
}

/// Exact AVC420 provider backed by the protocol-neutral media decoder API.
///
/// The caller supplies parameter sets obtained from the negotiated EGFX video
/// stream.  A backend is accepted only when the internal exact AVC420 bridge
/// confirms H.264/AVC420, 8-bit YUV420 and a YUV420P8 output.  This type keeps
/// the FFmpeg/native implementation outside the RDP crate while making the
/// composition-root wiring concrete for every client architecture.
pub struct Avc420DecoderProvider {
    factory: Arc<dyn VideoDecoderFactory>,
    parameter_sets: Option<VideoParameterSets>,
}

impl Avc420DecoderProvider {
    pub fn new(factory: Arc<dyn VideoDecoderFactory>, parameter_sets: VideoParameterSets) -> Self {
        Self {
            factory,
            parameter_sets: Some(parameter_sets),
        }
    }

    /// Create a provider that learns SPS/PPS from the first complete AVC
    /// access unit. RDPGFX does not expose those parameter sets in the
    /// pre-activation capability exchange, so this is the production
    /// composition-root path. If the first unit omits either set, decoding
    /// remains fail-closed and the caller can keep the legacy graphics path.
    pub fn from_factory(factory: Arc<dyn VideoDecoderFactory>) -> Self {
        Self {
            factory,
            parameter_sets: None,
        }
    }
}

impl EgfxDecoderProvider for Avc420DecoderProvider {
    fn create_decoder(
        &self,
        session_id: SessionId,
        coded_size: PixelSize,
    ) -> Option<Box<dyn H264Decoder>> {
        let decoder = match self.parameter_sets.as_ref() {
            Some(parameter_sets) => EgfxH264Decoder::try_new(
                self.factory.as_ref(),
                session_id,
                1,
                coded_size,
                parameter_sets.sps().to_vec().into_boxed_slice(),
                parameter_sets.pps().to_vec().into_boxed_slice(),
            )
            .ok(),
            None => {
                EgfxH264Decoder::try_new_lazy(Arc::clone(&self.factory), session_id, 1, coded_size)
                    .ok()
            }
        }?;
        Some(Box::new(decoder) as Box<dyn H264Decoder>)
    }
}

/// AVC420 + AVC444 provider backed by one exact planar H.264 factory.
///
/// AVC444 subframes are decoded sequentially by one H.264 stream context and
/// then combined by the bounded V1/V2 reconstruction layer. This type is a
/// construction seam only: callers still need an independent server/profile
/// and live-interoperability gate before injecting it into production roots.
pub struct Avc444DecoderProvider {
    factory: Arc<dyn VideoDecoderFactory>,
}

impl Avc444DecoderProvider {
    pub fn new(factory: Arc<dyn VideoDecoderFactory>) -> Self {
        Self { factory }
    }
}

#[allow(private_interfaces)]
impl EgfxDecoderProvider for Avc444DecoderProvider {
    fn create_decoder(
        &self,
        session_id: SessionId,
        coded_size: PixelSize,
    ) -> Option<Box<dyn H264Decoder>> {
        Avc420DecoderProvider::from_factory(Arc::clone(&self.factory))
            .create_decoder(session_id, coded_size)
    }

    fn create_avc444_decoder(
        &self,
        session_id: SessionId,
        coded_size: PixelSize,
    ) -> Option<Box<dyn Avc444Decoder>> {
        Avc444VideoDecoder::try_new(Arc::clone(&self.factory), session_id, coded_size)
            .ok()
            .map(|decoder| Box::new(decoder) as Box<dyn Avc444Decoder>)
    }
}

#[cfg(test)]
pub(crate) struct NoopEgfxHandler;

#[cfg(test)]
impl GraphicsPipelineHandler for NoopEgfxHandler {}

const MAX_PENDING_SURFACE_UPDATES: usize = 256;
const MAX_PENDING_PIXEL_BYTES: usize = 64 * 1024 * 1024;
const MAX_AVC444_BITMAP_DATA: usize = 64 * 1024 * 1024;
const BYTES_PER_PIXEL: usize = 4;
const AVC_TIMEBASE: NonZeroU32 = match NonZeroU32::new(90_000) {
    Some(value) => value,
    None => unreachable!(),
};

#[derive(Clone, Copy, Debug)]
struct EgfxSurface {
    width: u32,
    height: u32,
    origin_x: u32,
    origin_y: u32,
    mapped: bool,
}

#[derive(Debug)]
struct EgfxSurfaceState {
    session_id: SessionId,
    generation: u64,
    output_size: Option<PixelSize>,
    revision: u64,
    surfaces: BTreeMap<u16, EgfxSurface>,
    backings: backing::Backings,
    coverage: Option<EgfxCoverage>,
    baseline_established: bool,
    baseline_published: bool,
    pending_patches: Vec<PixelPatch>,
    pending_revision: Option<u64>,
    pending_overflowed: bool,
    pending_reference_update: bool,
    updates: VecDeque<SurfaceUpdate>,
    rejected_updates: u64,
    unhandled_codec_count: u64,
    last_unhandled_codec: Option<u16>,
    failure_reason: Option<RdpEgfxFailure>,
    disabled: bool,
}

#[derive(Debug)]
struct EgfxCoverage {
    size: PixelSize,
    words: Box<[u64]>,
    covered_pixels: u64,
}

impl EgfxCoverage {
    fn try_new(size: PixelSize) -> Option<Self> {
        let total_pixels = u64::from(size.width).checked_mul(u64::from(size.height))?;
        let word_count = total_pixels.checked_add(63)?.checked_div(64)?;
        let word_count = usize::try_from(word_count).ok()?;
        let mut words = Vec::new();
        words.try_reserve_exact(word_count).ok()?;
        words.resize(word_count, 0);
        Some(Self {
            size,
            words: words.into_boxed_slice(),
            covered_pixels: 0,
        })
    }

    fn record(&mut self, rect: PixelRect) -> bool {
        let Some((_, end)) = rect.checked_bounds() else {
            return false;
        };
        if end.x > self.size.width || end.y > self.size.height {
            return false;
        }
        for y in rect.y..end.y {
            let Some(start) = u64::from(y)
                .checked_mul(u64::from(self.size.width))
                .and_then(|row| row.checked_add(u64::from(rect.x)))
            else {
                return false;
            };
            let Some(end_x) = start.checked_add(u64::from(rect.width)) else {
                return false;
            };
            let mut cursor = start;
            while cursor < end_x {
                let Some(word_index) = usize::try_from(cursor / 64).ok() else {
                    return false;
                };
                let bit_offset = (cursor % 64) as u32;
                let bit_count = (end_x - cursor).min(u64::from(64 - bit_offset));
                let mask = if bit_count == 64 {
                    u64::MAX
                } else {
                    ((1_u64 << bit_count) - 1) << bit_offset
                };
                let Some(word) = self.words.get_mut(word_index) else {
                    return false;
                };
                let new_bits = mask & !*word;
                *word |= mask;
                self.covered_pixels = self
                    .covered_pixels
                    .saturating_add(u64::from(new_bits.count_ones()));
                cursor += bit_count;
            }
        }
        self.covered_pixels == u64::from(self.size.width) * u64::from(self.size.height)
    }

    fn clear(&mut self) {
        self.words.fill(0);
        self.covered_pixels = 0;
    }
}

/// Adapts the protocol-neutral YUV420 decoder contract to the pinned IronRDP
/// EGFX callback contract. The default RDP factory remains legacy-only; a
/// platform composition root may construct this adapter only after its exact
/// backend/profile/package checks pass. Server confirmation and the resulting
/// frame/recovery evidence still remain observable session gates.
#[allow(dead_code)]
pub(crate) struct EgfxH264Decoder {
    factory: Option<Arc<dyn VideoDecoderFactory>>,
    decoder: Option<Box<dyn VideoDecoder>>,
    stream: Option<VideoStreamConfig>,
    session_id: SessionId,
    coded_size: PixelSize,
    generation: u64,
    next_timestamp: u64,
    failed: bool,
}

#[allow(dead_code)]
impl EgfxH264Decoder {
    pub(crate) fn try_new(
        factory: &dyn VideoDecoderFactory,
        session_id: SessionId,
        generation: u64,
        coded_size: PixelSize,
        sps: Box<[u8]>,
        pps: Box<[u8]>,
    ) -> DecoderResult<Self> {
        let stream = avc420_stream_config(session_id, generation, coded_size, sps, pps)?;
        let input = stream.as_input();
        let query = VideoDecodeQuery {
            codec: input.codec,
            profile: input.profile,
            chroma: input.chroma,
            bit_depth: input.bit_depth,
            coded_size: input.coded_size,
            frame_rate: None,
            preferred_outputs: vec![VideoPixelFormat::Yuv420P8].into_boxed_slice(),
        };
        if factory.availability() != VideoBackendAvailability::DecoderReady
            || !factory.query(&query).is_exact()
        {
            return Err(DecoderError::msg(
                "AVC420 decoder factory does not provide an exact capability",
            ));
        }
        let decoder = factory
            .create(&stream)
            .map_err(|_| DecoderError::msg("AVC420 decoder creation failed"))?;
        Ok(Self {
            factory: None,
            decoder: Some(decoder),
            session_id,
            coded_size,
            generation,
            stream: Some(stream),
            next_timestamp: 0,
            failed: false,
        })
    }

    pub(crate) fn try_new_lazy(
        factory: Arc<dyn VideoDecoderFactory>,
        session_id: SessionId,
        generation: u64,
        coded_size: PixelSize,
    ) -> DecoderResult<Self> {
        if generation == 0 {
            return Err(DecoderError::msg("AVC420 generation must be non-zero"));
        }
        let query = VideoDecodeQuery {
            codec: VideoCodec::H264,
            profile: VideoProfile::H264Avc420,
            chroma: ChromaFormat::Yuv420,
            bit_depth: 8,
            coded_size,
            frame_rate: None,
            preferred_outputs: vec![VideoPixelFormat::Yuv420P8].into_boxed_slice(),
        };
        if factory.availability() != VideoBackendAvailability::DecoderReady
            || !factory.query(&query).is_exact()
        {
            return Err(DecoderError::msg(
                "AVC420 decoder factory does not provide an exact capability",
            ));
        }
        Ok(Self {
            factory: Some(factory),
            decoder: None,
            session_id,
            coded_size,
            generation,
            stream: None,
            next_timestamp: 0,
            failed: false,
        })
    }

    #[cfg(test)]
    fn from_decoder(stream: VideoStreamConfig, decoder: Box<dyn VideoDecoder>) -> Self {
        Self {
            factory: None,
            decoder: Some(decoder),
            session_id: stream.as_input().identity.session_id,
            coded_size: stream.as_input().coded_size,
            generation: stream.as_input().generation,
            stream: Some(stream),
            next_timestamp: 0,
            failed: false,
        }
    }

    fn current_generation(&self) -> u64 {
        self.generation
    }

    fn ensure_decoder(&mut self, data: &[u8]) -> DecoderResult<()> {
        if self.decoder.is_some() {
            return Ok(());
        }
        let factory = self
            .factory
            .as_ref()
            .ok_or_else(|| DecoderError::msg("AVC420 decoder factory is unavailable"))?;
        let (sps, pps) = extract_avc_parameter_sets(data)
            .ok_or_else(|| DecoderError::msg("first AVC420 access unit lacks SPS/PPS"))?;
        let stream =
            avc420_stream_config(self.session_id, self.generation, self.coded_size, sps, pps)?;
        let input = stream.as_input();
        let query = VideoDecodeQuery {
            codec: input.codec,
            profile: input.profile,
            chroma: input.chroma,
            bit_depth: input.bit_depth,
            coded_size: input.coded_size,
            frame_rate: None,
            preferred_outputs: vec![VideoPixelFormat::Yuv420P8].into_boxed_slice(),
        };
        if factory.availability() != VideoBackendAvailability::DecoderReady
            || !factory.query(&query).is_exact()
        {
            return Err(DecoderError::msg(
                "AVC420 decoder factory does not provide an exact capability",
            ));
        }
        let decoder = factory
            .create(&stream)
            .map_err(|_| DecoderError::msg("AVC420 decoder creation failed"))?;
        self.stream = Some(stream);
        self.decoder = Some(decoder);
        self.factory = None;
        Ok(())
    }

    fn next_timestamp(&mut self) -> DecoderResult<VideoTimestamp> {
        let ticks = self.next_timestamp;
        self.next_timestamp = self
            .next_timestamp
            .checked_add(1)
            .ok_or_else(|| DecoderError::msg("AVC420 timestamp overflow"))?;
        Ok(VideoTimestamp {
            ticks,
            timescale: AVC_TIMEBASE,
        })
    }
}

impl H264Decoder for EgfxH264Decoder {
    fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
        let frame = self.decode_yuv420_frame(data)?;
        decoded_yuv420_to_rgba(&frame)
    }

    fn reset(&mut self) {
        let _ = self.reset_checked();
    }

    fn is_healthy(&self) -> bool {
        !self.failed
    }
}

impl EgfxH264Decoder {
    pub(crate) fn reset_checked(&mut self) -> DecoderResult<()> {
        let result = (|| {
            let generation = self
                .generation
                .checked_add(1)
                .ok_or_else(|| DecoderError::msg("AVC420 generation overflow"))?;
            if let Some(decoder) = self.decoder.as_mut() {
                decoder
                    .reset(generation)
                    .map_err(|_| DecoderError::msg("AVC420 decoder reset failed"))?;
                let stream = self.stream.as_ref().ok_or_else(|| {
                    DecoderError::msg("AVC420 stream configuration is unavailable")
                })?;
                let mut input = stream.as_input().clone();
                input.generation = generation;
                self.stream = Some(
                    VideoStreamConfig::try_new(input)
                        .map_err(|_| DecoderError::msg("AVC420 stream reset is invalid"))?,
                );
            }
            self.generation = generation;
            self.next_timestamp = 0;
            Ok(())
        })();
        if result.is_err() {
            // IronRDP's H264Decoder::reset has no error return.  Retain the
            // failure locally so every later decode is rejected and the
            // owning EGFX adapter can disable the optional stream rather than
            // publishing a reset with an unusable decoder.
            self.failed = true;
        }
        result
    }

    /// Decode an AVC access unit while retaining its protocol-neutral YUV420
    /// planes for AVC444's auxiliary-view reconstruction. The public IronRDP
    /// trait still receives the RGBA conversion above.
    pub(crate) fn decode_yuv420_frame(
        &mut self,
        data: &[u8],
    ) -> DecoderResult<frd_media_api::DecodedVideoFrame> {
        if self.failed {
            return Err(DecoderError::msg("AVC420 decoder is in a failed state"));
        }
        // MS-RDPEGFX specifies the AVC420 payload as an Annex-B byte stream,
        // while the pinned IronRDP decoder trait and media backend consume
        // four-byte length-prefixed NAL units. Normalize either accepted wire
        // representation once at this boundary; the decoder and all metadata
        // extraction below then operate on one exact internal format.
        let normalized = normalize_avc_access_unit(data)?;
        self.ensure_decoder(&normalized)?;
        let (identity, coded_size) = {
            let stream = self
                .stream
                .as_ref()
                .ok_or_else(|| DecoderError::msg("AVC420 stream configuration is unavailable"))?;
            (stream.as_input().identity, stream.as_input().coded_size)
        };
        let timestamp = self.next_timestamp()?;
        let random_access = avc_contains_idr(&normalized);
        let access_unit = EncodedVideoAccessUnit::try_new(
            identity,
            self.current_generation(),
            timestamp,
            random_access,
            normalized,
        )
        .map_err(|_| DecoderError::msg("AVC420 access unit exceeds the media contract"))?;

        let outcome = self
            .decoder
            .as_mut()
            .ok_or_else(|| DecoderError::msg("AVC420 decoder is unavailable"))?
            .submit(access_unit)
            .map_err(|_| DecoderError::msg("AVC420 decoder submission failed"))?;
        let frame = match outcome {
            DecodeOutcome::NeedMoreData => {
                return Err(DecoderError::msg(
                    "AVC420 decoder did not produce a frame for this EGFX update",
                ));
            }
            DecodeOutcome::Frames(frames) if frames.len() == 1 => {
                frames.into_vec().pop().expect("length checked above")
            }
            DecodeOutcome::Frames(_) => {
                return Err(DecoderError::msg(
                    "AVC420 decoder produced an ambiguous frame count",
                ));
            }
        };
        if frame.as_input().identity != identity
            || frame.as_input().generation != self.current_generation()
            || frame.as_input().coded_size != coded_size
        {
            self.failed = true;
            return Err(DecoderError::msg(
                "AVC420 decoder returned a stale stream or generation",
            ));
        }
        Ok(frame)
    }
}

struct Avc444SurfaceDecoderState {
    size: PixelSize,
    reconstructor: Yuv444Reconstructor,
}

struct Avc444VideoDecoder {
    decoder: EgfxH264Decoder,
    coded_size: PixelSize,
    surfaces: BTreeMap<u16, Avc444SurfaceDecoderState>,
}

impl Avc444VideoDecoder {
    fn try_new(
        factory: Arc<dyn VideoDecoderFactory>,
        session_id: SessionId,
        coded_size: PixelSize,
    ) -> DecoderResult<Self> {
        let decoder = EgfxH264Decoder::try_new_lazy(factory, session_id, 1, coded_size)?;
        Ok(Self {
            decoder,
            coded_size,
            surfaces: BTreeMap::new(),
        })
    }

    fn decode_subframe(
        &mut self,
        data: &[u8],
        expected_size: PixelSize,
    ) -> DecoderResult<frd_media_api::DecodedVideoFrame> {
        let frame = self.decoder.decode_yuv420_frame(data)?;
        let input = frame.as_input();
        if input.format != VideoPixelFormat::Yuv420P8 || input.coded_size != expected_size {
            return Err(DecoderError::msg(
                "AVC444 subframe is not an exact YUV420 coded surface",
            ));
        }
        Ok(frame)
    }
}

impl Avc444Decoder for Avc444VideoDecoder {
    fn reset(&mut self, width: u32, height: u32) -> DecoderResult<()> {
        let size = PixelSize::new(width, height)
            .ok_or_else(|| DecoderError::msg("AVC444 reset dimensions are invalid"))?;
        if size != self.coded_size {
            return Err(DecoderError::msg(
                "AVC444 reset dimensions differ from the negotiated coded size",
            ));
        }
        self.decoder.reset_checked()?;
        self.surfaces.clear();
        Ok(())
    }

    fn on_surface_created(&mut self, surface: &Surface) -> DecoderResult<()> {
        let size = PixelSize::new(u32::from(surface.width), u32::from(surface.height))
            .ok_or_else(|| DecoderError::msg("AVC444 surface dimensions are invalid"))?;
        if size != self.coded_size {
            return Err(DecoderError::msg(
                "AVC444 offscreen surface size is not the negotiated coded size",
            ));
        }
        let reconstructor = Yuv444Reconstructor::try_new(
            usize::try_from(size.width).map_err(|_| DecoderError::msg("AVC444 width overflow"))?,
            usize::try_from(size.height)
                .map_err(|_| DecoderError::msg("AVC444 height overflow"))?,
        )
        .map_err(|_| DecoderError::msg("AVC444 surface allocation exceeds the budget"))?;
        self.surfaces.insert(
            surface.id,
            Avc444SurfaceDecoderState {
                size,
                reconstructor,
            },
        );
        Ok(())
    }

    fn on_surface_deleted(&mut self, surface_id: u16) -> DecoderResult<()> {
        self.surfaces.remove(&surface_id);
        Ok(())
    }

    fn decode(
        &mut self,
        codec_id: Codec1Type,
        surface_id: u16,
        bitmap: &ValidatedAvc444Bitmap<'_>,
        destination: &ExclusiveRectangle,
    ) -> DecoderResult<DecodedFrame> {
        let expected_size = self
            .surfaces
            .get(&surface_id)
            .ok_or_else(|| DecoderError::msg("AVC444 surface is unknown"))?;
        let expected_size = expected_size.size;
        let stream1 = self.decode_subframe(bitmap.stream1, expected_size)?;
        let stream2 = match bitmap.encoding {
            ValidatedAvc444Encoding::LumaAndChroma => {
                let data = bitmap
                    .stream2
                    .ok_or_else(|| DecoderError::msg("AVC444 stream2 is missing"))?;
                Some(self.decode_subframe(data, expected_size)?)
            }
            ValidatedAvc444Encoding::Luma | ValidatedAvc444Encoding::Chroma => None,
        };
        // CHROMA-only wire PDUs carry their auxiliary YUV420 subframe in
        // stream1.  stream2 exists only for the combined LUMA_AND_CHROMA
        // encoding; treating stream1 as luma here loses the stream1 region
        // mapping and makes every chroma-only update fail with a missing
        // chroma frame.
        let (luma, luma_regions, chroma, chroma_regions) = match bitmap.encoding {
            ValidatedAvc444Encoding::LumaAndChroma => (
                Some(yuv420_frame(&stream1)?),
                bitmap.stream1_regions.as_ref(),
                Some(yuv420_frame(stream2.as_ref().ok_or_else(|| {
                    DecoderError::msg("AVC444 stream2 is missing")
                })?)?),
                bitmap.stream2_regions.as_deref().unwrap_or_default(),
            ),
            ValidatedAvc444Encoding::Luma => (
                Some(yuv420_frame(&stream1)?),
                bitmap.stream1_regions.as_ref(),
                None,
                &[] as &[ironrdp::pdu::geometry::InclusiveRectangle],
            ),
            ValidatedAvc444Encoding::Chroma => (
                None,
                &[] as &[ironrdp::pdu::geometry::InclusiveRectangle],
                Some(yuv420_frame(&stream1)?),
                bitmap.stream1_regions.as_ref(),
            ),
        };
        let mode = match (codec_id, bitmap.encoding) {
            (Codec1Type::Avc444, ValidatedAvc444Encoding::Luma) => Avc444ReconstructionMode::Luma,
            (Codec1Type::Avc444, ValidatedAvc444Encoding::Chroma) => {
                Avc444ReconstructionMode::Chroma(Avc444ChromaLayout::V1)
            }
            (Codec1Type::Avc444, ValidatedAvc444Encoding::LumaAndChroma) => {
                Avc444ReconstructionMode::LumaAndChroma(Avc444ChromaLayout::V1)
            }
            (Codec1Type::Avc444v2, ValidatedAvc444Encoding::Luma) => Avc444ReconstructionMode::Luma,
            (Codec1Type::Avc444v2, ValidatedAvc444Encoding::Chroma) => {
                Avc444ReconstructionMode::Chroma(Avc444ChromaLayout::V2)
            }
            (Codec1Type::Avc444v2, ValidatedAvc444Encoding::LumaAndChroma) => {
                Avc444ReconstructionMode::LumaAndChroma(Avc444ChromaLayout::V2)
            }
            _ => return Err(DecoderError::msg("unexpected AVC444 codec id")),
        };
        let surface = self
            .surfaces
            .get_mut(&surface_id)
            .ok_or_else(|| DecoderError::msg("AVC444 surface is unknown"))?;
        surface
            .reconstructor
            .apply(
                mode,
                luma.as_ref(),
                luma_regions,
                chroma.as_ref(),
                chroma_regions,
            )
            .map_err(|_| DecoderError::msg("AVC444 stream reconstruction failed"))?;
        crop_yuv444_to_rgba(surface.reconstructor.frame(), destination)
    }
}

fn yuv420_frame<'a>(frame: &'a frd_media_api::DecodedVideoFrame) -> DecoderResult<Yuv420Frame<'a>> {
    let input = frame.as_input();
    if input.format != VideoPixelFormat::Yuv420P8 || input.planes.len() != 3 {
        return Err(DecoderError::msg(
            "AVC444 decoder returned a non-YUV420 frame",
        ));
    }
    let width = usize::try_from(input.coded_size.width)
        .map_err(|_| DecoderError::msg("AVC444 frame width overflow"))?;
    let height = usize::try_from(input.coded_size.height)
        .map_err(|_| DecoderError::msg("AVC444 frame height overflow"))?;
    let y = &input.planes[0];
    let u = &input.planes[1];
    let v = &input.planes[2];
    Yuv420Frame::try_new(
        width,
        height,
        Yuv420Plane::new(
            y.bytes(),
            usize::try_from(y.stride_bytes())
                .map_err(|_| DecoderError::msg("AVC444 luma stride overflow"))?,
            usize::try_from(y.width())
                .map_err(|_| DecoderError::msg("AVC444 luma width overflow"))?,
            usize::try_from(y.height())
                .map_err(|_| DecoderError::msg("AVC444 luma height overflow"))?,
        )
        .map_err(|_| DecoderError::msg("AVC444 luma plane is invalid"))?,
        Yuv420Plane::new(
            u.bytes(),
            usize::try_from(u.stride_bytes())
                .map_err(|_| DecoderError::msg("AVC444 U stride overflow"))?,
            usize::try_from(u.width()).map_err(|_| DecoderError::msg("AVC444 U width overflow"))?,
            usize::try_from(u.height())
                .map_err(|_| DecoderError::msg("AVC444 U height overflow"))?,
        )
        .map_err(|_| DecoderError::msg("AVC444 U plane is invalid"))?,
        Yuv420Plane::new(
            v.bytes(),
            usize::try_from(v.stride_bytes())
                .map_err(|_| DecoderError::msg("AVC444 V stride overflow"))?,
            usize::try_from(v.width()).map_err(|_| DecoderError::msg("AVC444 V width overflow"))?,
            usize::try_from(v.height())
                .map_err(|_| DecoderError::msg("AVC444 V height overflow"))?,
        )
        .map_err(|_| DecoderError::msg("AVC444 V plane is invalid"))?,
    )
    .map_err(|_| DecoderError::msg("AVC444 YUV420 dimensions are invalid"))
}

fn crop_yuv444_to_rgba(
    frame: &Yuv444Frame,
    destination: &ExclusiveRectangle,
) -> DecoderResult<DecodedFrame> {
    if destination.left > destination.right || destination.top > destination.bottom {
        return Err(DecoderError::msg("AVC444 destination rectangle is invalid"));
    }
    let left = usize::from(destination.left);
    let top = usize::from(destination.top);
    let width = usize::from(destination.right - destination.left);
    let height = usize::from(destination.bottom - destination.top);
    let right = left
        .checked_add(width)
        .ok_or_else(|| DecoderError::msg("AVC444 destination width overflow"))?;
    let bottom = top
        .checked_add(height)
        .ok_or_else(|| DecoderError::msg("AVC444 destination height overflow"))?;
    if width == 0 || height == 0 || right > frame.width() || bottom > frame.height() {
        return Err(DecoderError::msg(
            "AVC444 destination exceeds decoded frame",
        ));
    }
    let [y_plane, u_plane, v_plane] = frame.planes();
    let frame_width = frame.width();
    let plane_offset = top
        .checked_mul(frame_width)
        .and_then(|row| row.checked_add(left))
        .ok_or_else(|| DecoderError::msg("AVC444 destination offset overflow"))?;
    let required_plane_len = height
        .checked_sub(1)
        .and_then(|rows| rows.checked_mul(frame_width))
        .and_then(|rows| rows.checked_add(width))
        .ok_or_else(|| DecoderError::msg("AVC444 destination plane length overflow"))?;
    let plane_end = plane_offset
        .checked_add(required_plane_len)
        .ok_or_else(|| DecoderError::msg("AVC444 destination plane end overflow"))?;
    if y_plane.len() < plane_end || u_plane.len() < plane_end || v_plane.len() < plane_end {
        return Err(DecoderError::msg("AVC444 destination plane is truncated"));
    }
    let output_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(BYTES_PER_PIXEL))
        .ok_or_else(|| DecoderError::msg("AVC444 RGBA frame is over budget"))?;
    let mut output = vec![0_u8; output_len];
    convert_yuv444_to_rgba(
        width,
        height,
        &y_plane[plane_offset..],
        frame_width,
        &u_plane[plane_offset..],
        frame_width,
        &v_plane[plane_offset..],
        frame_width,
        &mut output,
    )
    .map_err(|_| DecoderError::msg("AVC444 YUV444 plane conversion failed"))?;
    Ok(DecodedFrame::new(
        output,
        u32::try_from(width).map_err(|_| DecoderError::msg("AVC444 width overflow"))?,
        u32::try_from(height).map_err(|_| DecoderError::msg("AVC444 height overflow"))?,
    ))
}

fn crop_rgba_frame_region(
    frame: &DecodedFrame,
    left: u32,
    top: u32,
    width: u32,
    height: u32,
) -> DecoderResult<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(DecoderError::msg("AVC444 region is empty"));
    }
    let right = left
        .checked_add(width)
        .ok_or_else(|| DecoderError::msg("AVC444 region right edge overflows"))?;
    let bottom = top
        .checked_add(height)
        .ok_or_else(|| DecoderError::msg("AVC444 region bottom edge overflows"))?;
    if right > frame.width() || bottom > frame.height() {
        return Err(DecoderError::msg("AVC444 region exceeds decoded frame"));
    }
    let frame_width = usize::try_from(frame.width())
        .map_err(|_| DecoderError::msg("AVC444 frame width overflow"))?;
    let left =
        usize::try_from(left).map_err(|_| DecoderError::msg("AVC444 region left overflow"))?;
    let top = usize::try_from(top).map_err(|_| DecoderError::msg("AVC444 region top overflow"))?;
    let width =
        usize::try_from(width).map_err(|_| DecoderError::msg("AVC444 region width overflow"))?;
    let height =
        usize::try_from(height).map_err(|_| DecoderError::msg("AVC444 region height overflow"))?;
    let row_bytes = width
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or_else(|| DecoderError::msg("AVC444 region row is over budget"))?;
    let output_len = row_bytes
        .checked_mul(height)
        .ok_or_else(|| DecoderError::msg("AVC444 region is over budget"))?;
    if output_len > MAX_AVC444_BITMAP_DATA {
        return Err(DecoderError::msg("AVC444 region is over budget"));
    }
    let source = frame.data();
    let frame_row_bytes = frame_width
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or_else(|| DecoderError::msg("AVC444 frame row overflows"))?;
    let frame_height = usize::try_from(frame.height())
        .map_err(|_| DecoderError::msg("AVC444 frame height overflow"))?;
    let required_source_len = frame_row_bytes
        .checked_mul(frame_height)
        .ok_or_else(|| DecoderError::msg("AVC444 frame buffer overflows"))?;
    if source.len() < required_source_len {
        return Err(DecoderError::msg("AVC444 frame buffer is truncated"));
    }
    let mut output = vec![0_u8; output_len];
    for row in 0..height {
        let source_start = (top + row)
            .checked_mul(frame_row_bytes)
            .and_then(|offset| offset.checked_add(left.checked_mul(BYTES_PER_PIXEL)?))
            .ok_or_else(|| DecoderError::msg("AVC444 region source offset overflows"))?;
        let source_end = source_start
            .checked_add(row_bytes)
            .ok_or_else(|| DecoderError::msg("AVC444 region source end overflows"))?;
        let destination_start = row
            .checked_mul(row_bytes)
            .ok_or_else(|| DecoderError::msg("AVC444 region destination offset overflows"))?;
        output[destination_start..destination_start + row_bytes]
            .copy_from_slice(&source[source_start..source_end]);
    }
    Ok(output)
}

fn avc420_stream_config(
    session_id: SessionId,
    generation: u64,
    coded_size: PixelSize,
    sps: Box<[u8]>,
    pps: Box<[u8]>,
) -> DecoderResult<VideoStreamConfig> {
    if generation == 0 {
        return Err(DecoderError::msg("AVC420 generation must be non-zero"));
    }
    let visible_rect = PixelRect {
        x: 0,
        y: 0,
        width: coded_size.width,
        height: coded_size.height,
    };
    let parameter_sets = VideoParameterSets::try_new(None, sps, pps)
        .map_err(|_| DecoderError::msg("AVC420 parameter sets are invalid"))?;
    let input = VideoStreamConfigInput {
        identity: VideoStreamIdentity {
            session_id,
            stream_id: 1,
        },
        generation,
        codec: VideoCodec::H264,
        profile: VideoProfile::H264Avc420,
        chroma: ChromaFormat::Yuv420,
        bit_depth: 8,
        coded_size,
        visible_rect,
        time_base: VideoTimeBase::try_new(AVC_TIMEBASE.get())
            .map_err(|_| DecoderError::msg("AVC420 timebase is invalid"))?,
        bitstream_format: VideoBitstreamFormat::AvcLengthPrefixed,
        colorimetry: VideoColorimetry::Bt709,
        range: VideoRange::Limited,
        chroma_location: ChromaLocation::Left,
        parameter_sets,
    };
    VideoStreamConfig::try_new(input)
        .map_err(|_| DecoderError::msg("AVC420 stream configuration is invalid"))
}

fn valid_avc_length_prefixed(data: &[u8]) -> bool {
    let mut offset = 0usize;
    while offset < data.len() {
        let Some(length_end) = offset.checked_add(4) else {
            return false;
        };
        if length_end > data.len() {
            return false;
        }
        let length = u32::from_be_bytes(
            data[offset..length_end]
                .try_into()
                .expect("the four-byte length was checked"),
        ) as usize;
        if length == 0 {
            return false;
        }
        let Some(end) = length_end.checked_add(length) else {
            return false;
        };
        if end > data.len() {
            return false;
        }
        offset = end;
    }
    !data.is_empty() && offset == data.len()
}

fn normalize_avc_access_unit(data: &[u8]) -> DecoderResult<Box<[u8]>> {
    if valid_avc_length_prefixed(data) {
        return Ok(data.to_vec().into_boxed_slice());
    }

    let nals = parse_annex_b_nals(data)
        .ok_or_else(|| DecoderError::msg("AVC420 payload is not a valid AVC access unit"))?;
    let total = nals.iter().try_fold(0usize, |total, nal| {
        total
            .checked_add(4)
            .and_then(|value| value.checked_add(nal.len()))
            .ok_or_else(|| DecoderError::msg("AVC420 access unit size overflow"))
    })?;
    let mut normalized = Vec::with_capacity(total);
    for nal in nals {
        let length = u32::try_from(nal.len())
            .map_err(|_| DecoderError::msg("AVC420 NAL exceeds four-byte length prefix"))?;
        normalized.extend_from_slice(&length.to_be_bytes());
        normalized.extend_from_slice(nal);
    }
    Ok(normalized.into_boxed_slice())
}

fn parse_annex_b_nals(data: &[u8]) -> Option<Vec<&[u8]>> {
    fn start_code_len(data: &[u8], offset: usize) -> Option<usize> {
        if offset.checked_add(4)? <= data.len() && data[offset..offset + 4] == [0, 0, 0, 1] {
            Some(4)
        } else if offset.checked_add(3)? <= data.len() && data[offset..offset + 3] == [0, 0, 1] {
            Some(3)
        } else {
            None
        }
    }

    let mut first_start = None;
    for offset in 0..data.len() {
        if start_code_len(data, offset).is_some() {
            first_start = Some(offset);
            break;
        }
    }
    if let Some(first_start) = first_start {
        if data[..first_start].iter().any(|byte| *byte != 0) {
            return None;
        }
    }
    let mut start = first_start?;
    let mut nals = Vec::new();
    loop {
        let code_len = start_code_len(data, start)?;
        let nal_start = start.checked_add(code_len)?;
        if nal_start >= data.len() {
            return None;
        }
        let mut next_start = None;
        for offset in nal_start..data.len() {
            if start_code_len(data, offset).is_some() {
                next_start = Some(offset);
                break;
            }
        }
        let end = next_start.unwrap_or(data.len());
        let mut nal_end = end;
        while nal_end > nal_start && data[nal_end - 1] == 0 {
            nal_end -= 1;
        }
        if nal_end == nal_start {
            return None;
        }
        nals.push(&data[nal_start..nal_end]);
        let Some(next) = next_start else { break };
        start = next;
    }
    (!nals.is_empty()).then_some(nals)
}

/// Extract the parameter sets carried in an internally normalized AVC
/// length-prefixed access unit. Wire data is normalized by
/// `normalize_avc_access_unit` before this helper is called.
///
/// The EGFX capability exchange does not carry H.264 SPS/PPS. A lazy decoder
/// therefore accepts only a complete access unit that contains both sets and
/// preserves the NAL payloads for the exact media configuration. It never
/// guesses profile, dimensions, or parameter-set bytes.
fn extract_avc_parameter_sets(data: &[u8]) -> Option<(Box<[u8]>, Box<[u8]>)> {
    let mut offset = 0usize;
    let mut sps = None;
    let mut pps = None;
    while offset < data.len() {
        let length_end = offset.checked_add(4)?;
        if length_end > data.len() {
            return None;
        }
        let length = usize::try_from(u32::from_be_bytes(
            data[offset..length_end].try_into().ok()?,
        ))
        .ok()?;
        let start = length_end;
        let end = start.checked_add(length)?;
        if length == 0 || end > data.len() {
            return None;
        }
        let nal = &data[start..end];
        match nal.first().copied().map(|value| value & 0x1f) {
            Some(7) if sps.is_none() => sps = Some(nal.to_vec().into_boxed_slice()),
            Some(8) if pps.is_none() => pps = Some(nal.to_vec().into_boxed_slice()),
            _ => {}
        }
        offset = end;
    }
    Some((sps?, pps?))
}

fn avc_contains_idr(data: &[u8]) -> bool {
    let mut offset = 0usize;
    while offset + 4 <= data.len() {
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let start = offset + 4;
        let Some(end) = start.checked_add(length) else {
            return false;
        };
        if end > data.len() || length == 0 {
            return false;
        }
        if data[start] & 0x1f == 5 {
            return true;
        }
        offset = end;
    }
    false
}

/// The pinned IronRDP client deliberately forwards AVC444/AVC444v2 PDUs to its
/// handler because it does not implement the YUV420 + Chroma420 reconstruction.
/// Validate that wire envelope here before any future decoder is allowed to
/// consume it. This keeps malformed input fail-closed and records the exact
/// subframe mode without pretending that parsing is color reconstruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidatedAvc444Encoding {
    LumaAndChroma,
    Luma,
    Chroma,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct ValidatedAvc444Bitmap<'a> {
    pub encoding: ValidatedAvc444Encoding,
    pub stream1: &'a [u8],
    pub stream2: Option<&'a [u8]>,
    /// Parsed rectangles are owned because IronRDP owns them inside its decoded stream value.
    /// Keeping them here preserves the wire geometry for the future planar adapter without
    /// returning references to a temporary parser value.
    /// Regions normalized to inclusive bounds for the existing AVC444
    /// reconstruction kernels. The wire parser uses exclusive RDPGFX_RECT16
    /// bounds and conversion is performed once at this validation boundary.
    pub stream1_regions: Box<[InclusiveRectangle]>,
    pub stream2_regions: Option<Box<[InclusiveRectangle]>>,
    pub stream1_region_count: usize,
    pub stream2_region_count: usize,
}

impl<'a> ValidatedAvc444Bitmap<'a> {
    fn publication_regions(&self) -> Vec<InclusiveRectangle> {
        let mut regions = self.stream1_regions.to_vec();
        if matches!(self.encoding, ValidatedAvc444Encoding::LumaAndChroma) {
            if let Some(stream2_regions) = self.stream2_regions.as_deref() {
                for region in stream2_regions {
                    if !regions.contains(region) {
                        regions.push(region.clone());
                    }
                }
            }
        }
        regions
    }
}

fn normalize_avc444_wire_regions(
    regions: Vec<ExclusiveRectangle>,
) -> DecoderResult<Box<[InclusiveRectangle]>> {
    regions
        .into_iter()
        .map(|region| {
            if region.left >= region.right || region.top >= region.bottom {
                return Err(DecoderError::msg(
                    "AVC444 region has empty or inverted exclusive bounds",
                ));
            }
            Ok(InclusiveRectangle {
                left: region.left,
                top: region.top,
                right: region.right - 1,
                bottom: region.bottom - 1,
            })
        })
        .collect::<DecoderResult<Vec<_>>>()
        .map(Vec::into_boxed_slice)
}

pub(crate) fn validate_avc444_bitmap(data: &[u8]) -> DecoderResult<ValidatedAvc444Bitmap<'_>> {
    if data.is_empty() || data.len() > MAX_AVC444_BITMAP_DATA {
        return Err(DecoderError::msg(
            "AVC444 bitmap payload is empty or over budget",
        ));
    }
    if data.len() < 4 {
        return Err(DecoderError::msg("AVC444 bitmap envelope is truncated"));
    }
    let stream_info = u32::from_le_bytes(
        data[..4]
            .try_into()
            .expect("the four-byte AVC444 stream info was checked"),
    );
    let stream_len = usize::try_from(stream_info & 0x3fff_ffff)
        .expect("the 30-bit AVC444 stream length fits in usize");
    let encoding_raw = stream_info >> 30;
    // In LUMA and CHROMA mode, the declared stream1 length covers the whole
    // payload. IronRDP's decoder intentionally splits the source cursor at
    // that length, so enforce the outer envelope here instead of allowing
    // bytes after stream1 to be silently discarded.
    if encoding_raw != 0
        && stream_len != 0
        && stream_len
            .checked_add(4)
            .is_none_or(|expected| expected != data.len())
    {
        return Err(DecoderError::msg(
            "AVC444 single-stream envelope has trailing or missing bytes",
        ));
    }
    let mut cursor = ReadCursor::new(data);
    let stream = Avc444BitmapStream::decode(&mut cursor)
        .map_err(|_| DecoderError::msg("AVC444 bitmap envelope is malformed"))?;
    if !cursor.remaining().is_empty()
        || stream.stream1.data.is_empty()
        || normalize_avc_access_unit(stream.stream1.data).is_err()
    {
        return Err(DecoderError::msg(
            "AVC444 stream1 is not a complete AVC access unit",
        ));
    }
    let (encoding, stream2) = match stream.encoding {
        Avc444Encoding::LUMA_AND_CHROMA => {
            let Some(stream2) = stream.stream2 else {
                return Err(DecoderError::msg(
                    "AVC444 luma-and-chroma mode is missing stream2",
                ));
            };
            if stream2.data.is_empty() || normalize_avc_access_unit(stream2.data).is_err() {
                return Err(DecoderError::msg(
                    "AVC444 chroma stream is not a complete AVC access unit",
                ));
            }
            (ValidatedAvc444Encoding::LumaAndChroma, Some(stream2))
        }
        Avc444Encoding::LUMA => {
            if stream.stream2.is_some() {
                return Err(DecoderError::msg(
                    "AVC444 luma mode must not contain stream2",
                ));
            }
            (ValidatedAvc444Encoding::Luma, None)
        }
        Avc444Encoding::CHROMA => {
            if stream.stream2.is_some() {
                return Err(DecoderError::msg(
                    "AVC444 chroma mode must not contain stream2",
                ));
            }
            (ValidatedAvc444Encoding::Chroma, None)
        }
        _ => return Err(DecoderError::msg("AVC444 encoding value is reserved")),
    };
    let stream1_data = stream.stream1.data;
    let stream1_region_count = stream.stream1.rectangles.len();
    let stream1_regions = normalize_avc444_wire_regions(stream.stream1.rectangles)?;
    let stream2_data = stream2.as_ref().map(|stream| stream.data);
    let stream2_region_count = stream2.as_ref().map_or(0, |stream| stream.rectangles.len());
    let stream2_regions = stream2
        .map(|stream| normalize_avc444_wire_regions(stream.rectangles))
        .transpose()?;
    if stream1_regions.is_empty()
        || stream2_regions
            .as_deref()
            .is_some_and(|regions| regions.is_empty())
    {
        return Err(DecoderError::msg(
            "AVC444 bitmap stream has no region rectangles",
        ));
    }
    Ok(ValidatedAvc444Bitmap {
        encoding,
        stream1: stream1_data,
        stream2: stream2_data,
        stream1_regions,
        stream2_regions,
        stream1_region_count,
        stream2_region_count,
    })
}

fn decoded_yuv420_to_rgba(frame: &frd_media_api::DecodedVideoFrame) -> DecoderResult<DecodedFrame> {
    let input = frame.as_input();
    if input.format != VideoPixelFormat::Yuv420P8 || input.planes.len() != 3 {
        return Err(DecoderError::msg(
            "AVC420 decoder returned a non-YUV420 frame",
        ));
    }
    let width = usize::try_from(input.coded_size.width)
        .map_err(|_| DecoderError::msg("AVC420 frame width is not representable"))?;
    let height = usize::try_from(input.coded_size.height)
        .map_err(|_| DecoderError::msg("AVC420 frame height is not representable"))?;
    let output_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(BYTES_PER_PIXEL))
        .ok_or_else(|| DecoderError::msg("AVC420 RGBA frame is over budget"))?;
    let mut output = vec![0_u8; output_len];
    let y_plane = &input.planes[0];
    let u_plane = &input.planes[1];
    let v_plane = &input.planes[2];
    let y_stride = usize::try_from(y_plane.stride_bytes())
        .map_err(|_| DecoderError::msg("AVC420 luma stride is not representable"))?;
    let u_stride = usize::try_from(u_plane.stride_bytes())
        .map_err(|_| DecoderError::msg("AVC420 chroma stride is not representable"))?;
    let v_stride = usize::try_from(v_plane.stride_bytes())
        .map_err(|_| DecoderError::msg("AVC420 chroma stride is not representable"))?;
    let y_bytes = y_plane.bytes();
    let u_bytes = u_plane.bytes();
    let v_bytes = v_plane.bytes();
    convert_yuv420_to_rgba(
        width,
        height,
        y_bytes,
        y_stride,
        u_bytes,
        u_stride,
        v_bytes,
        v_stride,
        &mut output,
    )
    .map_err(|_| DecoderError::msg("invalid AVC420 YUV plane layout"))?;
    Ok(DecodedFrame::new(
        output,
        input.coded_size.width,
        input.coded_size.height,
    ))
}

/// Generation-bound EGFX output queue.
///
/// IronRDP's handler callback receives decoded RGBA data, while the protocol
/// runtime requires generation-bound `SurfaceUpdate` values. This adapter
/// performs only bounded coordinate and buffer validation and queues the
/// resulting updates for the owning RDP session to consume. It never writes to
/// wgpu or calls a platform renderer.
#[derive(Clone)]
pub(crate) struct EgfxSurfacePublisher {
    state: Arc<Mutex<EgfxSurfaceState>>,
    clearcodec_decoder: Arc<Mutex<crate::clearcodec::NativeDecoder>>,
    avc444_decoder: Option<Arc<Mutex<Box<dyn Avc444Decoder>>>>,
    expected_coded_size: Option<PixelSize>,
}

impl fmt::Debug for EgfxSurfacePublisher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EgfxSurfacePublisher")
            .field("has_avc444_decoder", &self.avc444_decoder.is_some())
            .field("expected_coded_size", &self.expected_coded_size)
            .finish()
    }
}

#[allow(dead_code)]
impl EgfxSurfacePublisher {
    /// Create a publisher bound to the runtime's current generation. Every
    /// validated `ResetGraphics` advances to the next generation before its
    /// reset is queued, so the runtime can admit it atomically.
    pub(crate) fn try_new(session_id: SessionId, generation: u64) -> Option<Self> {
        if generation == 0 {
            return None;
        }
        let clearcodec_decoder = crate::clearcodec::native_decoder()?;
        Some(Self {
            state: Arc::new(Mutex::new(EgfxSurfaceState {
                session_id,
                generation,
                output_size: None,
                revision: 0,
                surfaces: BTreeMap::new(),
                backings: backing::Backings::new(backing::DEFAULT_BUDGET)?,
                coverage: None,
                baseline_established: false,
                baseline_published: false,
                pending_patches: Vec::new(),
                pending_revision: None,
                pending_overflowed: false,
                pending_reference_update: false,
                updates: VecDeque::new(),
                rejected_updates: 0,
                unhandled_codec_count: 0,
                last_unhandled_codec: None,
                failure_reason: None,
                disabled: false,
            })),
            clearcodec_decoder: Arc::new(Mutex::new(clearcodec_decoder)),
            avc444_decoder: None,
            expected_coded_size: None,
        })
    }

    /// Bind the publisher to the coded size used to construct the decoder.
    /// IronRDP's decoder reset callback does not carry dimensions, so a server
    /// reset to another size must disable EGFX before publishing a mismatched
    /// runtime surface. Callers without a decoder may leave this unset.
    pub(crate) fn with_expected_coded_size(mut self, size: PixelSize) -> Self {
        self.expected_coded_size = Some(size);
        self
    }

    /// Attach an AVC444 decoder to this publisher.  The caller is responsible
    /// for installing it only after the exact wire/profile/backend gates have
    /// passed; the default publisher remains AVC420-only.
    pub(crate) fn with_avc444_decoder(mut self, decoder: Box<dyn Avc444Decoder>) -> Self {
        self.avc444_decoder = Some(Arc::new(Mutex::new(decoder)));
        self
    }

    pub(crate) fn drain(&self) -> Vec<SurfaceUpdate> {
        let mut state = lock_state(&self.state);
        state.updates.drain(..).collect()
    }

    pub(crate) fn rejected_update_count(&self) -> u64 {
        lock_state(&self.state).rejected_updates
    }

    pub(crate) fn is_disabled(&self) -> bool {
        lock_state(&self.state).disabled
    }

    fn reject(state: &mut EgfxSurfaceState) {
        state.rejected_updates = state.rejected_updates.saturating_add(1);
    }

    fn push(state: &mut EgfxSurfaceState, update: SurfaceUpdate) {
        if state.disabled {
            return;
        }
        if state.updates.len() >= MAX_PENDING_SURFACE_UPDATES {
            Self::reject(state);
            return;
        }
        state.updates.push_back(update);
    }

    fn disable_locked(state: &mut EgfxSurfaceState) {
        state.disabled = true;
        state.backings.clear();
        state.updates.clear();
        state.pending_patches.clear();
        state.pending_revision = None;
        state.pending_overflowed = false;
        state.pending_reference_update = false;
        if let Some(coverage) = state.coverage.as_mut() {
            coverage.clear();
        }
        Self::reject(state);
    }

    pub(crate) fn disable(&self) {
        let mut state = lock_state(&self.state);
        Self::disable_locked(&mut state);
    }

    fn avc444_call<T>(
        &self,
        operation: impl FnOnce(&mut dyn Avc444Decoder) -> DecoderResult<T>,
    ) -> DecoderResult<T> {
        let decoder = self
            .avc444_decoder
            .as_ref()
            .ok_or_else(|| DecoderError::msg("AVC444 decoder is unavailable"))?;
        let mut decoder = decoder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        operation(decoder.as_mut())
    }

    fn reject_without_decoder(&self) {
        let mut state = lock_state(&self.state);
        Self::reject(&mut state);
    }

    fn fail_avc444(&self) {
        // Decoder failure is isolated to the optional EGFX path.  The caller's
        // legacy Bitmap/RemoteFX pipeline remains available because this only
        // disables this generation-bound publisher.
        self.disable();
    }

    fn checked_patch(
        state: &EgfxSurfaceState,
        surface: EgfxSurface,
        rectangle: &ExclusiveRectangle,
        data: &[u8],
        width: u16,
        height: u16,
    ) -> Option<PixelPatch> {
        let (rect, stride_bytes, expected_len) =
            Self::checked_patch_layout(state, surface, rectangle, width, height)?;
        if data.len() != expected_len {
            return None;
        }
        let mut bgrx = vec![0_u8; data.len()];
        convert_rgba_to_bgrx(data, &mut bgrx).ok()?;
        Some(PixelPatch {
            rect,
            stride_bytes,
            pixels: PixelBuffer::from_boxed_slice(bgrx.into_boxed_slice()),
        })
    }

    fn checked_patch_layout(
        state: &EgfxSurfaceState,
        surface: EgfxSurface,
        rectangle: &ExclusiveRectangle,
        width: u16,
        height: u16,
    ) -> Option<(PixelRect, u32, usize)> {
        if width == 0
            || height == 0
            || !surface.mapped
            || rectangle.left > rectangle.right
            || rectangle.top > rectangle.bottom
            || u32::from(rectangle.right) > surface.width
            || u32::from(rectangle.bottom) > surface.height
            || u32::from(rectangle.right - rectangle.left) != u32::from(width)
            || u32::from(rectangle.bottom - rectangle.top) != u32::from(height)
        {
            return None;
        }

        let x = surface.origin_x.checked_add(u32::from(rectangle.left))?;
        let y = surface.origin_y.checked_add(u32::from(rectangle.top))?;
        let width = u32::from(width);
        let height = u32::from(height);
        let output_size = state.output_size?;
        let end_x = x.checked_add(width)?;
        let end_y = y.checked_add(height)?;
        if end_x > output_size.width || end_y > output_size.height {
            return None;
        }
        let expected_len = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(4)?;
        let stride_bytes = width.checked_mul(BYTES_PER_PIXEL as u32)?;
        Some((
            PixelRect {
                x,
                y,
                width,
                height,
            },
            stride_bytes,
            expected_len,
        ))
    }
}

fn lock_state(state: &Mutex<EgfxSurfaceState>) -> std::sync::MutexGuard<'_, EgfxSurfaceState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl GraphicsPipelineHandler for EgfxSurfacePublisher {
    fn capabilities(&self) -> Vec<CapabilitySet> {
        // V10.7 advertises both AVC420 and AVC444.  It is safe only when the
        // optional decoder owns the complete AVC444 wire/reconstruction path;
        // otherwise keep the default AVC420 + V8 fallback contract.
        if self.avc444_decoder.is_some() {
            vec![
                CapabilitySet::V10_7 {
                    flags: CapabilitiesV107Flags::SMALL_CACHE,
                },
                CapabilitySet::V8_1 {
                    flags: CapabilitiesV81Flags::AVC420_ENABLED | CapabilitiesV81Flags::SMALL_CACHE,
                },
                CapabilitySet::V8 {
                    flags: CapabilitiesV8Flags::SMALL_CACHE,
                },
            ]
        } else {
            vec![
                CapabilitySet::V8_1 {
                    flags: CapabilitiesV81Flags::AVC420_ENABLED | CapabilitiesV81Flags::SMALL_CACHE,
                },
                CapabilitySet::V8 {
                    flags: CapabilitiesV8Flags::SMALL_CACHE,
                },
            ]
        }
    }

    fn on_reset_graphics(&mut self, width: u32, height: u32) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            Self::reject(&mut state);
            return;
        }
        // A reset is the generation boundary. Never advance the private generation unless the
        // corresponding Reset update can be queued for the runtime; otherwise later damage
        // would be permanently stale relative to the runtime's still-current generation.
        if state.updates.len() >= MAX_PENDING_SURFACE_UPDATES {
            Self::reject(&mut state);
            return;
        }
        let Some(generation) = state.generation.checked_add(1) else {
            Self::reject(&mut state);
            return;
        };
        let Some(size) = PixelSize::new(width, height) else {
            Self::reject(&mut state);
            return;
        };
        if self
            .expected_coded_size
            .is_some_and(|expected| expected != size)
        {
            // The pinned IronRDP H264Decoder::reset() API has no dimensions,
            // so recreating a decoder for this new coded size is impossible at
            // this boundary. Stop EGFX before changing the runtime surface;
            // the legacy Bitmap/RemoteFX stream remains available.
            state
                .failure_reason
                .get_or_insert(RdpEgfxFailure::PublisherCodedSizeMismatch);
            Self::disable_locked(&mut state);
            return;
        }
        if validate_surface_size(size).is_err() {
            Self::reject(&mut state);
            return;
        }
        let Some(coverage) = EgfxCoverage::try_new(size) else {
            Self::reject(&mut state);
            return;
        };
        if self.avc444_decoder.is_some()
            && self
                .avc444_call(|decoder| decoder.reset(width, height))
                .is_err()
        {
            Self::disable_locked(&mut state);
            return;
        }
        state.output_size = Some(size);
        state.revision = 0;
        state.surfaces.clear();
        state.backings.clear();
        state.coverage = Some(coverage);
        state.baseline_established = false;
        state.baseline_published = false;
        state.pending_patches.clear();
        state.pending_revision = None;
        state.pending_overflowed = false;
        state.pending_reference_update = false;
        state.generation = generation;
        let session_id = state.session_id;
        Self::push(
            &mut state,
            SurfaceUpdate::Reset {
                session_id,
                generation,
                size,
                format: PixelFormat::Bgrx8UnormSrgb,
            },
        );
    }

    fn on_surface_created(&mut self, surface: &Surface) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            Self::reject(&mut state);
            return;
        }
        if surface.width == 0 || surface.height == 0 {
            Self::reject(&mut state);
            return;
        }
        let size = PixelSize::new(u32::from(surface.width), u32::from(surface.height))
            .expect("checked dimensions");
        if state.backings.create(surface.id, size).is_err() {
            Self::disable_locked(&mut state);
            return;
        }
        state.surfaces.insert(
            surface.id,
            EgfxSurface {
                width: u32::from(surface.width),
                height: u32::from(surface.height),
                origin_x: 0,
                origin_y: 0,
                mapped: false,
            },
        );
        drop(state);
        if self.avc444_decoder.is_some()
            && self
                .avc444_call(|decoder| decoder.on_surface_created(surface))
                .is_err()
        {
            self.fail_avc444();
        }
    }

    fn on_surface_deleted(&mut self, surface_id: u16) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            Self::reject(&mut state);
            return;
        }
        state.surfaces.remove(&surface_id);
        state.backings.delete(surface_id);
        drop(state);
        if self.avc444_decoder.is_some()
            && self
                .avc444_call(|decoder| decoder.on_surface_deleted(surface_id))
                .is_err()
        {
            self.fail_avc444();
        }
    }

    fn on_surface_mapped(&mut self, surface_id: u16, origin_x: u32, origin_y: u32) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            Self::reject(&mut state);
            return;
        }
        let Some(surface) = state.surfaces.get_mut(&surface_id) else {
            Self::reject(&mut state);
            return;
        };
        surface.origin_x = origin_x;
        surface.origin_y = origin_y;
        surface.mapped = true;
        if state.backings.contains(surface_id) {
            let result = state
                .backings
                .valid_rectangles(surface_id, MAX_PENDING_SURFACE_UPDATES)
                .and_then(|rects| Self::publish_backing(&mut state, surface_id, &rects));
            if result.is_err() {
                Self::disable_locked(&mut state);
                return;
            }
        }
        drop(state);
        if self.avc444_decoder.is_some()
            && self
                .avc444_call(|decoder| decoder.on_surface_mapped(surface_id, origin_x, origin_y))
                .is_err()
        {
            self.fail_avc444();
        }
    }

    fn on_solid_fill(&mut self, pdu: &ironrdp_egfx::pdu::SolidFillPdu) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            return;
        }
        let result = (|| {
            Self::ensure_backing(&mut state, pdu.surface_id)?;
            let rects = pdu
                .rectangles
                .iter()
                .map(backing::rect)
                .collect::<backing::Result<Vec<_>>>()?;
            Self::publication_rectangles(&state, pdu.surface_id, &rects)?;
            state.backings.fill(
                pdu.surface_id,
                &rects,
                [pdu.fill_pixel.b, pdu.fill_pixel.g, pdu.fill_pixel.r, 255],
            )?;
            Self::publish_backing(&mut state, pdu.surface_id, &rects)
        })();
        if result.is_err() {
            Self::disable_locked(&mut state);
        }
    }
    fn on_surface_to_surface(&mut self, pdu: &ironrdp_egfx::pdu::SurfaceToSurfacePdu) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            return;
        }
        let result = (|| {
            Self::ensure_backing(&mut state, pdu.source_surface_id)?;
            Self::ensure_backing(&mut state, pdu.destination_surface_id)?;
            let r = backing::rect(&pdu.source_rectangle)?;
            let points: Vec<_> = pdu.destination_points.iter().map(|p| (p.x, p.y)).collect();
            let rects = points
                .iter()
                .map(|&(x, y)| PixelRect {
                    x: u32::from(x),
                    y: u32::from(y),
                    width: r.width,
                    height: r.height,
                })
                .collect::<Vec<_>>();
            Self::publication_rectangles(&state, pdu.destination_surface_id, &rects)?;
            let rects = state.backings.copy_surface(
                pdu.source_surface_id,
                r,
                pdu.destination_surface_id,
                &points,
            )?;
            Self::publish_backing(&mut state, pdu.destination_surface_id, &rects)
        })();
        if result.is_err() {
            Self::disable_locked(&mut state);
        }
    }
    fn on_surface_to_cache(&mut self, pdu: &ironrdp_egfx::pdu::SurfaceToCachePdu) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            return;
        }
        let result = (|| {
            Self::ensure_backing(&mut state, pdu.surface_id)?;
            state.backings.cache_surface(
                pdu.surface_id,
                backing::rect(&pdu.source_rectangle)?,
                pdu.cache_slot,
            )
        })();
        if result.is_err() {
            Self::disable_locked(&mut state);
        }
    }
    fn on_cache_to_surface(&mut self, pdu: &ironrdp_egfx::pdu::CacheToSurfacePdu) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            return;
        }
        let result = (|| {
            Self::ensure_backing(&mut state, pdu.surface_id)?;
            let points: Vec<_> = pdu.destination_points.iter().map(|p| (p.x, p.y)).collect();
            let rects = state
                .backings
                .copy_cache(pdu.cache_slot, pdu.surface_id, &points)?;
            Self::publish_backing(&mut state, pdu.surface_id, &rects)
        })();
        if result.is_err() {
            Self::disable_locked(&mut state);
        }
    }
    fn on_evict_cache_entry(&mut self, pdu: &ironrdp_egfx::pdu::EvictCacheEntryPdu) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            return;
        }
        if state.backings.evict(pdu.cache_slot).is_err() {
            Self::disable_locked(&mut state);
        }
    }

    fn on_bitmap_updated(&mut self, update: &BitmapUpdate) {
        self.queue_bitmap_update(
            update.surface_id,
            &update.destination_rectangle,
            update.codec_id,
            &update.data,
            update.width,
            update.height,
        );
    }

    fn on_frame_complete(&mut self, _frame_id: u32) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            Self::reject(&mut state);
            return;
        }
        let Some(revision) = state.pending_revision.take() else {
            return;
        };
        let contains_reference_update = std::mem::take(&mut state.pending_reference_update);
        // Damage and FrameBoundary are one publication unit. Keep both or publish neither;
        // a partially queued frame would leave the runtime with an uncommitted baseline.
        if state
            .updates
            .len()
            .checked_add(2)
            .is_none_or(|length| length > MAX_PENDING_SURFACE_UPDATES)
        {
            if contains_reference_update {
                state
                    .failure_reason
                    .get_or_insert(RdpEgfxFailure::PublisherQueueLimit);
                Self::disable_locked(&mut state);
                return;
            }
            Self::reject(&mut state);
            state.pending_patches.clear();
            state.pending_overflowed = false;
            state.pending_reference_update = false;
            if let Some(coverage) = state.coverage.as_mut() {
                coverage.clear();
            }
            state.baseline_established = false;
            return;
        }
        if state.pending_overflowed {
            if contains_reference_update {
                state
                    .failure_reason
                    .get_or_insert(RdpEgfxFailure::PublisherQueueLimit);
                Self::disable_locked(&mut state);
                return;
            }
            state.pending_patches.clear();
            state.pending_overflowed = false;
            state.pending_reference_update = false;
            if let Some(coverage) = state.coverage.as_mut() {
                coverage.clear();
            }
            state.baseline_established = false;
            return;
        }
        let completeness = if state.baseline_published {
            FrameCompleteness::Incremental
        } else if state.baseline_established {
            state.baseline_published = true;
            FrameCompleteness::FullBaseline
        } else {
            state.pending_patches.clear();
            if let Some(coverage) = state.coverage.as_mut() {
                coverage.clear();
            }
            return;
        };
        let patches = std::mem::take(&mut state.pending_patches);
        let session_id = state.session_id;
        let generation = state.generation;
        Self::push(
            &mut state,
            SurfaceUpdate::Damage {
                session_id,
                generation,
                revision,
                patches,
            },
        );
        Self::push(
            &mut state,
            SurfaceUpdate::FrameBoundary {
                session_id,
                generation,
                revision,
                completeness,
            },
        );
    }

    fn on_wire_to_surface2(&mut self, pdu: &ironrdp_egfx::pdu::WireToSurface2Pdu) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            return;
        }
        state.unhandled_codec_count = state.unhandled_codec_count.saturating_add(1);
        state.last_unhandled_codec = Some(u16::from(pdu.codec_id));
        state
            .failure_reason
            .get_or_insert(RdpEgfxFailure::UnsupportedCodec);
        Self::disable_locked(&mut state);
    }

    fn on_unhandled_pdu(&mut self, pdu: &GfxPdu) {
        let GfxPdu::WireToSurface1(wire) = pdu else {
            return;
        };
        if wire.codec_id == Codec1Type::ClearCodec {
            self.decode_clearcodec(wire);
            return;
        }
        if !matches!(wire.codec_id, Codec1Type::Avc444 | Codec1Type::Avc444v2) {
            let mut state = lock_state(&self.state);
            state.unhandled_codec_count = state.unhandled_codec_count.saturating_add(1);
            state.last_unhandled_codec = Some(u16::from(wire.codec_id));
            // 无法解码的更新使整个 generation 不再可信，清除之前排队的画面。
            state
                .failure_reason
                .get_or_insert(RdpEgfxFailure::UnsupportedCodec);
            if !state.disabled {
                Self::disable_locked(&mut state);
            }
            return;
        }
        let Some(decoder) = self.avc444_decoder.as_ref() else {
            self.reject_without_decoder();
            return;
        };
        let validated = match validate_avc444_bitmap(&wire.bitmap_data) {
            Ok(validated) => validated,
            Err(_) => {
                self.reject_without_decoder();
                return;
            }
        };
        let decoded = {
            let mut decoder = decoder
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            decoder.decode(
                wire.codec_id,
                wire.surface_id,
                &validated,
                &wire.destination_rectangle,
            )
        };
        let Ok(frame) = decoded else {
            self.fail_avc444();
            return;
        };
        let width = u32::from(wire.destination_rectangle.width());
        let height = u32::from(wire.destination_rectangle.height());
        if frame.width() != width || frame.height() != height {
            self.fail_avc444();
            return;
        }
        let destination_left = u32::from(wire.destination_rectangle.left);
        let destination_top = u32::from(wire.destination_rectangle.top);
        let destination_right = u32::from(wire.destination_rectangle.right);
        let destination_bottom = u32::from(wire.destination_rectangle.bottom);
        let mut updates = Vec::new();
        for region in validated.publication_regions() {
            let region_left = u32::from(region.left);
            let region_top = u32::from(region.top);
            let Some(region_right) = u32::from(region.right).checked_add(1) else {
                self.fail_avc444();
                return;
            };
            let Some(region_bottom) = u32::from(region.bottom).checked_add(1) else {
                self.fail_avc444();
                return;
            };
            if region_left < destination_left
                || region_top < destination_top
                || region_right > destination_right
                || region_bottom > destination_bottom
                || region_left >= region_right
                || region_top >= region_bottom
            {
                self.fail_avc444();
                return;
            }
            let crop_left = region_left - destination_left;
            let crop_top = region_top - destination_top;
            let crop_width = region_right - region_left;
            let crop_height = region_bottom - region_top;
            let data = match crop_rgba_frame_region(
                &frame,
                crop_left,
                crop_top,
                crop_width,
                crop_height,
            ) {
                Ok(data) => data,
                Err(_) => {
                    self.fail_avc444();
                    return;
                }
            };
            updates.push((
                ExclusiveRectangle {
                    left: region.left,
                    top: region.top,
                    right: match u16::try_from(region_right) {
                        Ok(value) => value,
                        Err(_) => {
                            self.fail_avc444();
                            return;
                        }
                    },
                    bottom: match u16::try_from(region_bottom) {
                        Ok(value) => value,
                        Err(_) => {
                            self.fail_avc444();
                            return;
                        }
                    },
                },
                data,
                crop_width,
                crop_height,
            ));
        }
        if updates.is_empty() {
            self.fail_avc444();
            return;
        }
        for (destination, data, width, height) in updates {
            self.queue_decoded_bitmap_update(wire.surface_id, &destination, &data, width, height);
        }
    }
}

impl EgfxSurfacePublisher {
    /// 固定分类仅含本地状态，不保留桌面载荷或上游错误文本。
    fn clearcodec_layout_failure(
        state: &EgfxSurfaceState,
        surface_id: u16,
        rectangle: &ExclusiveRectangle,
    ) -> RdpEgfxFailure {
        let Some(surface) = state.surfaces.get(&surface_id) else {
            return RdpEgfxFailure::PublisherMissingSurface;
        };
        if rectangle.left >= rectangle.right
            || rectangle.top >= rectangle.bottom
            || u32::from(rectangle.right) > surface.width
            || u32::from(rectangle.bottom) > surface.height
        {
            return RdpEgfxFailure::PublisherInvalidRectangle;
        }
        if !surface.mapped {
            return RdpEgfxFailure::Publisher;
        }
        if state.pending_revision.is_none() && state.revision == u64::MAX {
            return RdpEgfxFailure::PublisherRevisionOverflow;
        }
        if state.pending_overflowed
            || state.pending_patches.len() >= MAX_PENDING_SURFACE_UPDATES
            || state.updates.len() + 2 > MAX_PENDING_SURFACE_UPDATES
        {
            return RdpEgfxFailure::PublisherQueueLimit;
        }
        let Some(output) = state.output_size else {
            return RdpEgfxFailure::PublisherMissingOutput;
        };
        if surface
            .origin_x
            .checked_add(u32::from(rectangle.right))
            .is_none_or(|end| end > output.width)
            || surface
                .origin_y
                .checked_add(u32::from(rectangle.bottom))
                .is_none_or(|end| end > output.height)
        {
            return RdpEgfxFailure::PublisherOutputBounds;
        }
        RdpEgfxFailure::Publisher
    }

    /// ClearCodec 缓存和序号属于会话，普通 ResetGraphics 只改变发布 generation。
    /// 独立 BGRX 路径直接移动 opaque BGRA，不能走 AVC 的 RGBA 交换通道。
    fn decode_clearcodec(&mut self, wire: &ironrdp_egfx::pdu::WireToSurface1Pdu) {
        let mut state = lock_state(&self.state);
        if state.disabled {
            return;
        }
        let rectangle = &wire.destination_rectangle;
        // 2.2.4.1 的 compositePayload 是可选项。纯 CACHE_RESET 消息不绘图，
        // 仍验证 WireToSurface1 surface/rectangle 包络，但不要求映射或非零面积。
        if wire.bitmap_data.len() == 2 && wire.bitmap_data[0] == 4 {
            let valid = state.surfaces.get(&wire.surface_id).is_some_and(|surface| {
                rectangle.left <= rectangle.right
                    && rectangle.top <= rectangle.bottom
                    && u32::from(rectangle.right) <= surface.width
                    && u32::from(rectangle.bottom) <= surface.height
            });
            if valid {
                let result = self
                    .clearcodec_decoder
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .decode(
                        &wire.bitmap_data,
                        rectangle.right - rectangle.left,
                        rectangle.bottom - rectangle.top,
                    );
                if matches!(result, Ok(None)) {
                    return;
                }
            }
            state.failure_reason.get_or_insert(RdpEgfxFailure::Decoder);
            Self::disable_locked(&mut state);
            return;
        }
        let validated = backing::rect(rectangle).and_then(|rect| {
            Self::ensure_backing(&mut state, wire.surface_id)?;
            state.backings.check(wire.surface_id, rect)?;
            Self::publication_rectangles(&state, wire.surface_id, &[rect])?;
            Ok(rect)
        });
        let Ok(rect) = validated else {
            let reason = Self::clearcodec_layout_failure(&state, wire.surface_id, rectangle);
            state.failure_reason.get_or_insert(reason);
            Self::disable_locked(&mut state);
            return;
        };
        let decoded = self
            .clearcodec_decoder
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .decode(&wire.bitmap_data, rect.width as u16, rect.height as u16);
        let Ok(Some(pixels)) = decoded else {
            state.failure_reason.get_or_insert(RdpEgfxFailure::Decoder);
            Self::disable_locked(&mut state);
            return;
        };
        if state
            .backings
            .write(wire.surface_id, rect, &pixels)
            .is_err()
            || Self::publish_backing(&mut state, wire.surface_id, &[rect]).is_err()
        {
            state
                .failure_reason
                .get_or_insert(RdpEgfxFailure::Publisher);
            Self::disable_locked(&mut state);
        }
    }
}

impl EgfxSurfacePublisher {
    fn ensure_backing(state: &mut EgfxSurfaceState, id: u16) -> backing::Result<()> {
        if !state.backings.contains(id) {
            let s = state.surfaces.get(&id).ok_or(())?;
            let size = PixelSize::new(s.width, s.height).ok_or(())?;
            state.backings.create(id, size)?;
        }
        Ok(())
    }
    fn check_publication_bytes(
        state: &EgfxSurfaceState,
        new_bytes: usize,
        budget: usize,
    ) -> backing::Result<()> {
        let pending_bytes = state
            .pending_patches
            .iter()
            .try_fold(new_bytes, |sum, p| {
                sum.checked_add(p.pixels.as_bytes().len())
            })
            .ok_or(())?;
        let total_bytes = state
            .updates
            .iter()
            .try_fold(pending_bytes, |sum, u| match u {
                SurfaceUpdate::Damage { patches, .. } => patches
                    .iter()
                    .try_fold(sum, |n, p| n.checked_add(p.pixels.as_bytes().len())),
                _ => Some(sum),
            })
            .filter(|n| *n <= budget)
            .ok_or(())?;
        let _ = total_bytes;
        Ok(())
    }
    fn publication_rectangles(
        state: &EgfxSurfaceState,
        id: u16,
        rects: &[PixelRect],
    ) -> backing::Result<Vec<PixelRect>> {
        let s = state.surfaces.get(&id).ok_or(())?;
        if !s.mapped {
            return Ok(Vec::new());
        }
        let output = state.output_size.ok_or(())?;
        let maxw = output.width.saturating_sub(s.origin_x);
        let maxh = output.height.saturating_sub(s.origin_y);
        let clipped: Vec<_> = rects
            .iter()
            .filter_map(|r| {
                let endx = r.x.checked_add(r.width)?.min(maxw);
                let endy = r.y.checked_add(r.height)?.min(maxh);
                if endx <= r.x || endy <= r.y {
                    None
                } else {
                    Some(PixelRect {
                        x: r.x,
                        y: r.y,
                        width: endx - r.x,
                        height: endy - r.y,
                    })
                }
            })
            .collect();
        let new_bytes = clipped
            .iter()
            .try_fold(0usize, |sum, r| {
                sum.checked_add(r.width as usize * r.height as usize * 4)
            })
            .ok_or(())?;
        Self::check_publication_bytes(state, new_bytes, MAX_PENDING_PIXEL_BYTES)?;
        if !clipped.is_empty()
            && (state.pending_overflowed
                || state
                    .pending_patches
                    .len()
                    .checked_add(clipped.len())
                    .is_none_or(|n| n > MAX_PENDING_SURFACE_UPDATES)
                || state
                    .updates
                    .len()
                    .checked_add(2)
                    .is_none_or(|n| n > MAX_PENDING_SURFACE_UPDATES)
                || (state.pending_revision.is_none() && state.revision == u64::MAX))
        {
            return Err(());
        }
        Ok(clipped)
    }
    fn publish_backing(
        state: &mut EgfxSurfaceState,
        id: u16,
        rects: &[PixelRect],
    ) -> backing::Result<()> {
        let clipped = Self::publication_rectangles(state, id, rects)?;
        let surface = *state.surfaces.get(&id).ok_or(())?;
        let mut patches = Vec::with_capacity(clipped.len());
        for r in clipped {
            let pixels = state.backings.read(id, r)?;
            let x = surface.origin_x.checked_add(r.x).ok_or(())?;
            let y = surface.origin_y.checked_add(r.y).ok_or(())?;
            patches.push(PixelPatch {
                rect: PixelRect { x, y, ..r },
                stride_bytes: r.width * 4,
                pixels: PixelBuffer::from_boxed_slice(pixels.into_boxed_slice()),
            });
        }
        if !patches.is_empty() {
            state.pending_reference_update = true;
        }
        for patch in patches {
            Self::queue_patch(state, patch);
        }
        Ok(())
    }
    fn queue_bitmap_update(
        &mut self,
        surface_id: u16,
        destination_rectangle: &ExclusiveRectangle,
        codec_id: Codec1Type,
        data: &[u8],
        width: u16,
        height: u16,
    ) {
        if codec_id != Codec1Type::Avc420 {
            let mut state = lock_state(&self.state);
            if state.disabled {
                return;
            }
            state.unhandled_codec_count = state.unhandled_codec_count.saturating_add(1);
            state.last_unhandled_codec = Some(u16::from(codec_id));
            state
                .failure_reason
                .get_or_insert(RdpEgfxFailure::UnsupportedCodec);
            Self::disable_locked(&mut state);
            return;
        }
        if width == 0 || height == 0 {
            let mut state = lock_state(&self.state);
            Self::reject(&mut state);
            return;
        }
        self.queue_decoded_bitmap_update(
            surface_id,
            destination_rectangle,
            data,
            u32::from(width),
            u32::from(height),
        );
    }

    fn queue_decoded_bitmap_update(
        &mut self,
        surface_id: u16,
        destination_rectangle: &ExclusiveRectangle,
        data: &[u8],
        width: u32,
        height: u32,
    ) {
        let Ok(width) = u16::try_from(width) else {
            let mut state = lock_state(&self.state);
            Self::reject(&mut state);
            return;
        };
        let Ok(height) = u16::try_from(height) else {
            let mut state = lock_state(&self.state);
            Self::reject(&mut state);
            return;
        };
        if width == 0 || height == 0 {
            let mut state = lock_state(&self.state);
            Self::reject(&mut state);
            return;
        }
        let mut state = lock_state(&self.state);
        if state.disabled {
            Self::reject(&mut state);
            return;
        }
        let result = (|| {
            let rect = backing::rect(destination_rectangle)?;
            if rect.width != u32::from(width)
                || rect.height != u32::from(height)
                || data.len() != usize::from(width) * usize::from(height) * 4
            {
                return Err(());
            }
            Self::ensure_backing(&mut state, surface_id)?;
            state.backings.check(surface_id, rect)?;
            Self::publication_rectangles(&state, surface_id, &[rect])?;
            let mut pixels = vec![0; data.len()];
            convert_rgba_to_bgrx(data, &mut pixels).map_err(|_| ())?;
            state.backings.write(surface_id, rect, &pixels)?;
            Self::publish_backing(&mut state, surface_id, &[rect])
        })();
        if result.is_err() {
            Self::reject(&mut state);
        }
    }

    fn queue_patch(state: &mut EgfxSurfaceState, patch: PixelPatch) {
        if state.pending_revision.is_none() {
            let Some(revision) = state.revision.checked_add(1) else {
                Self::reject(state);
                return;
            };
            state.revision = revision;
            state.pending_revision = Some(revision);
        }
        if let Some(coverage) = state.coverage.as_mut() {
            if coverage.record(patch.rect) {
                state.baseline_established = true;
            }
        }
        if state.pending_patches.len() >= MAX_PENDING_SURFACE_UPDATES {
            Self::reject(state);
            state.pending_overflowed = true;
            return;
        }
        state.pending_patches.push(patch);
    }
}

/// A protocol-adapter-owned EGFX DVC processor.
///
/// The handler remains outside the RDP transport so the decoder and
/// `SurfaceUpdate` queue can be selected without adding platform branches to
/// the IronRDP session state machine.
#[allow(dead_code)]
pub(crate) struct EgfxAdapter {
    inner: GraphicsPipelineClient,
    surface_publisher: Option<EgfxSurfacePublisher>,
    /// 底层 IronRDP 图形客户端是否持有基础 H.264 解码器。
    ///
    /// `GraphicsPipelineClient::codec_capabilities()` 只反映服务器的
    /// `CapabilitiesConfirm`；即使调用方没有传入解码器，它仍可能报告 AVC 能力。
    /// 在 adapter 边界保留此事实，避免诊断信息把服务器证据误当成本地能力。
    h264_decoder_configured: bool,
    diagnostics: Mutex<RdpEgfxDiagnostics>,
    failed: Arc<AtomicBool>,
}

#[allow(dead_code)]
impl EgfxAdapter {
    pub(crate) fn new(
        decoder: Option<Box<dyn H264Decoder>>,
        handler: Box<dyn GraphicsPipelineHandler>,
    ) -> Self {
        let h264_decoder_configured = decoder.is_some();
        Self {
            inner: GraphicsPipelineClient::new(handler, decoder),
            surface_publisher: None,
            h264_decoder_configured,
            diagnostics: Mutex::new(RdpEgfxDiagnostics::default()),
            failed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn with_surface_publisher(
        decoder: Option<Box<dyn H264Decoder>>,
        publisher: EgfxSurfacePublisher,
    ) -> Self {
        let h264_decoder_configured = decoder.is_some();
        Self {
            inner: GraphicsPipelineClient::new(Box::new(publisher.clone()), decoder),
            surface_publisher: Some(publisher),
            h264_decoder_configured,
            diagnostics: Mutex::new(RdpEgfxDiagnostics::default()),
            failed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn diagnostics(&self) -> RdpEgfxDiagnostics {
        let mut diagnostics = *self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(publisher) = &self.surface_publisher {
            let state = lock_state(&publisher.state);
            diagnostics.unhandled_codec_count = state.unhandled_codec_count;
            diagnostics.last_unhandled_codec = state.last_unhandled_codec;
        }
        diagnostics
    }

    fn record_failure(&self, reason: RdpEgfxFailure) {
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        diagnostics.failure_count = diagnostics.failure_count.saturating_add(1);
        diagnostics.first_failure.get_or_insert(reason);
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.failed.load(Ordering::Acquire)
            && self.inner.is_active()
            && self
                .surface_publisher
                .as_ref()
                .is_none_or(|publisher| !publisher.is_disabled())
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub(crate) fn avc420_confirmed(&self) -> bool {
        self.h264_decoder_configured && self.is_active() && self.inner.codec_capabilities().avc420
    }

    pub(crate) fn avc444_confirmed(&self) -> bool {
        self.h264_decoder_configured && self.is_active() && self.inner.codec_capabilities().avc444
    }

    pub(crate) fn drain_surface_updates(&self) -> Vec<SurfaceUpdate> {
        self.surface_publisher
            .as_ref()
            .map(EgfxSurfacePublisher::drain)
            .unwrap_or_default()
    }

    /// A display reactivation commits a new runtime generation before the
    /// server has sent a fresh EGFX ResetGraphics. The pinned IronRDP client
    /// keeps its decoder and publisher private, so retaining them would make
    /// the next EGFX frame generation-stale. Disable this optional stream at
    /// the boundary; legacy Bitmap/RemoteFX continues on the new generation.
    pub(crate) fn disable_for_reactivation(&self) {
        self.record_failure(RdpEgfxFailure::Reactivation);
        self.failed.store(true, Ordering::Release);
        if let Some(publisher) = &self.surface_publisher {
            publisher.disable();
        }
    }
}

impl_as_any!(EgfxAdapter);

impl DvcProcessor for EgfxAdapter {
    fn channel_name(&self) -> &str {
        self.inner.channel_name()
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        let diagnostics = self
            .diagnostics
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        diagnostics.start_calls = diagnostics.start_calls.saturating_add(1);
        let result = self.inner.start(channel_id);
        match &result {
            Ok(messages) => {
                let diagnostics = self
                    .diagnostics
                    .get_mut()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                diagnostics.capability_messages_queued = diagnostics
                    .capability_messages_queued
                    .saturating_add(u64::try_from(messages.len()).unwrap_or(u64::MAX));
            }
            Err(_) => self.record_failure(RdpEgfxFailure::Start),
        }
        result
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        if self.failed.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }
        let result = self.inner.process(channel_id, payload);
        if let Some(capability) = self.inner.negotiated_capabilities() {
            let diagnostics = self
                .diagnostics
                .get_mut()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            diagnostics.typed_confirmation_ever = true;
            diagnostics.confirmed_version = Some(capability.version().0);
            diagnostics.confirmed_flags = match capability {
                CapabilitySet::V8 { flags } => Some(flags.bits()),
                CapabilitySet::V8_1 { flags } => Some(flags.bits()),
                CapabilitySet::V10 { flags } | CapabilitySet::V10_2 { flags } => Some(flags.bits()),
                CapabilitySet::V10_1 => None,
                CapabilitySet::V10_3 { flags } => Some(flags.bits()),
                CapabilitySet::V10_4 { flags }
                | CapabilitySet::V10_5 { flags }
                | CapabilitySet::V10_6 { flags }
                | CapabilitySet::V10_6Err { flags } => Some(flags.bits()),
                CapabilitySet::V10_7 { flags } => Some(flags.bits()),
            };
        }
        match result {
            Ok(messages) => {
                if self.inner.decoder_failed() {
                    self.record_failure(RdpEgfxFailure::Decoder);
                    self.failed.store(true, Ordering::Release);
                    if let Some(publisher) = &self.surface_publisher {
                        publisher.disable();
                    }
                } else if self
                    .surface_publisher
                    .as_ref()
                    .is_some_and(EgfxSurfacePublisher::is_disabled)
                {
                    let reason = self
                        .surface_publisher
                        .as_ref()
                        .and_then(|publisher| lock_state(&publisher.state).failure_reason)
                        .unwrap_or(RdpEgfxFailure::Publisher);
                    self.record_failure(reason);
                    self.failed.store(true, Ordering::Release);
                }
                Ok(messages)
            }
            Err(error) => {
                self.record_failure(RdpEgfxFailure::PayloadProcessing);
                self.failed.store(true, Ordering::Release);
                if let Some(publisher) = &self.surface_publisher {
                    publisher.disable();
                }
                // Keep the RDP session alive so the legacy bitmap/RemoteFX
                // path can continue. The malformed/unsupported EGFX stream is
                // permanently disabled for this generation; no partial
                // surface update is allowed to escape.
                let _ = error;
                Ok(Vec::new())
            }
        }
    }

    fn close(&mut self, channel_id: u32) {
        self.inner.close(channel_id);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        avc420_stream_config, validate_avc444_bitmap, Avc420DecoderProvider, Avc444Decoder,
        Avc444DecoderProvider, Avc444VideoDecoder, EgfxAdapter, EgfxDecoderProvider,
        EgfxH264Decoder, EgfxSurfacePublisher, ValidatedAvc444Encoding,
    };
    use crate::avc444::Yuv444Frame;
    use crate::yuv_convert::convert_yuv444_to_rgba;
    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_frame::{FrameCompleteness, PixelFormat, SurfaceUpdate};
    use frd_media_api::{
        DecodeOutcome, DecodedVideoFrame, DecodedVideoFrameInput, EncodedVideoAccessUnit,
        VideoBackendAvailability, VideoBackendId, VideoBackendKind, VideoCapabilityProvider,
        VideoDecodeCapability, VideoDecodeError, VideoDecodeQuery, VideoDecodeSupport,
        VideoDecoder, VideoDecoderFactory, VideoParameterSets, VideoPixelFormat, VideoPlane,
    };
    use ironrdp::core::encode_vec;
    use ironrdp::dvc::DvcProcessor;
    use ironrdp::graphics::zgfx::wrap_uncompressed;
    use ironrdp::pdu::geometry::ExclusiveRectangle;
    use ironrdp::pdu::{Encode, WriteCursor};
    use ironrdp_egfx::client::GraphicsPipelineHandler;
    use ironrdp_egfx::decode::{DecodedFrame, DecoderError, DecoderResult, H264Decoder};
    use ironrdp_egfx::pdu::{
        Avc420BitmapStream, Avc420Region, Avc444BitmapStream, CapabilitiesConfirmPdu,
        CapabilitiesV107Flags, CapabilitiesV81Flags, CapabilitySet, Codec1Type, CreateSurfacePdu,
        EndFramePdu, GfxPdu, MapSurfaceToOutputPdu, PixelFormat as EgfxPixelFormat,
        ResetGraphicsPdu, StartFramePdu, Timestamp, WireToSurface1Pdu,
    };
    use std::sync::Arc;

    #[test]
    fn diagnostics_preserve_confirmation_version_and_flags() {
        for (capability, avc420, expected_flags) in [
            (
                CapabilitySet::V8 {
                    flags: super::CapabilitiesV8Flags::SMALL_CACHE,
                },
                false,
                Some(2),
            ),
            (
                CapabilitySet::V8_1 {
                    flags: CapabilitiesV81Flags::SMALL_CACHE,
                },
                false,
                Some(2),
            ),
            (
                CapabilitySet::V8_1 {
                    flags: CapabilitiesV81Flags::AVC420_ENABLED,
                },
                true,
                Some(0x10),
            ),
            (CapabilitySet::V10_1, true, None),
        ] {
            let mut adapter =
                EgfxAdapter::new(Some(Box::new(FailingH264Decoder)), Box::new(TestHandler));
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu::from_typed(&capability)),
            );
            assert_eq!(adapter.avc420_confirmed(), avc420);
            let diagnostics = adapter.diagnostics();
            assert_eq!(diagnostics.confirmed_version, Some(capability.version().0));
            assert_eq!(diagnostics.confirmed_flags, expected_flags);
            adapter.process(7, &[0]).unwrap();
            assert_eq!(
                adapter.diagnostics().confirmed_version,
                diagnostics.confirmed_version
            );
            assert_eq!(
                adapter.diagnostics().confirmed_flags,
                diagnostics.confirmed_flags
            );
        }
    }

    #[test]
    fn unsupported_codec_disables_generation_once_and_discards_queued_updates() {
        for codec_id in [Codec1Type::RemoteFx, Codec1Type::Planar] {
            let publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
            let state = publisher.clone();
            let mut adapter = EgfxAdapter::with_surface_publisher(None, publisher);
            adapter.start(7).unwrap();
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu::from_typed(
                    &CapabilitySet::V10_7 {
                        flags: CapabilitiesV107Flags::SMALL_CACHE,
                    },
                )),
            );
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::ResetGraphics(ResetGraphicsPdu {
                    width: 2,
                    height: 2,
                    monitors: Vec::new(),
                }),
            );
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: 1,
                    width: 2,
                    height: 2,
                    pixel_format: EgfxPixelFormat::XRgb,
                }),
            );
            assert!(!super::lock_state(&state.state).updates.is_empty());
            let wire = GfxPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: 1,
                codec_id,
                pixel_format: EgfxPixelFormat::XRgb,
                destination_rectangle: ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
                bitmap_data: vec![0xA5],
            });
            process_gfx_pdu(&mut adapter, wire.clone());
            let diagnostics = adapter.diagnostics();
            assert_eq!(diagnostics.unhandled_codec_count, 1);
            assert_eq!(diagnostics.last_unhandled_codec, Some(u16::from(codec_id)));
            assert_eq!(diagnostics.failure_count, 1);
            assert_eq!(
                diagnostics.first_failure,
                Some(super::RdpEgfxFailure::UnsupportedCodec)
            );
            assert!(state.is_disabled());
            assert!(adapter.is_failed());
            assert!(!adapter.is_active());
            assert!(adapter.drain_surface_updates().is_empty());
            process_gfx_pdu(&mut adapter, wire);
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::ResetGraphics(ResetGraphicsPdu {
                    width: 2,
                    height: 2,
                    monitors: Vec::new(),
                }),
            );
            assert_eq!(adapter.diagnostics(), diagnostics);
            assert!(adapter.drain_surface_updates().is_empty());
        }
    }

    #[test]
    fn diagnostics_distinguish_unconfirmed_from_confirmed_then_failed() {
        for confirm_first in [false, true] {
            let mut adapter = EgfxAdapter::new(None, Box::new(TestHandler));
            assert_eq!(adapter.diagnostics(), super::RdpEgfxDiagnostics::default());
            let messages = adapter.start(7).unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(adapter.diagnostics().start_calls, 1);
            assert_eq!(adapter.diagnostics().capability_messages_queued, 1);
            assert!(!adapter.diagnostics().typed_confirmation_ever);
            if confirm_first {
                process_gfx_pdu(
                    &mut adapter,
                    GfxPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu::from_typed(
                        &CapabilitySet::V8 {
                            flags: super::CapabilitiesV8Flags::SMALL_CACHE,
                        },
                    )),
                );
                assert!(adapter.is_active());
            }
            // 非法 ZGFX 描述符；仅检查固定分类，不向诊断复制输入。
            assert!(adapter.process(7, &[0]).unwrap().is_empty());
            assert!(adapter.is_failed());
            assert!(!adapter.is_active());
            let diagnostics = adapter.diagnostics();
            assert_eq!(diagnostics.typed_confirmation_ever, confirm_first);
            assert_eq!(diagnostics.failure_count, 1);
            assert_eq!(
                diagnostics.first_failure,
                Some(super::RdpEgfxFailure::PayloadProcessing)
            );
            assert!(adapter.process(7, &[0]).unwrap().is_empty());
            assert_eq!(adapter.diagnostics(), diagnostics);
        }
    }

    #[test]
    fn egfx_adapter_is_disabled_without_a_decoder() {
        let adapter = EgfxAdapter::new(None, Box::new(TestHandler));

        assert!(!adapter.is_active());
        assert!(!adapter.avc420_confirmed());
        assert!(!adapter.avc444_confirmed());
    }

    #[test]
    fn capability_confirmation_does_not_confirm_without_a_base_decoder() {
        let publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1)
            .expect("generation")
            .with_avc444_decoder(Box::new(SolidAvc444Decoder));
        let mut adapter = EgfxAdapter::with_surface_publisher(None, publisher);

        adapter.start(7).expect("EGFX start succeeds");
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu::from_typed(
                &CapabilitySet::V10_7 {
                    flags: CapabilitiesV107Flags::SMALL_CACHE,
                },
            )),
        );

        assert!(adapter.is_active());
        assert!(
            !adapter.avc420_confirmed(),
            "server confirmation cannot provide a missing base H.264 decoder"
        );
        assert!(
            !adapter.avc444_confirmed(),
            "an AVC444 publisher cannot provide a missing base H.264 decoder"
        );
    }

    #[test]
    fn reactivation_disables_the_generation_bound_egfx_stream() {
        let publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
        let adapter = EgfxAdapter::with_surface_publisher(
            Some(Box::new(FailingH264Decoder)),
            publisher.clone(),
        );

        assert!(!adapter.is_failed());
        adapter.disable_for_reactivation();

        assert!(adapter.is_failed());
        assert!(!adapter.is_active());
        assert!(publisher.is_disabled());
        assert!(publisher.drain().is_empty());
        assert_eq!(
            adapter.diagnostics().first_failure,
            Some(super::RdpEgfxFailure::Reactivation)
        );
        assert_eq!(adapter.diagnostics().failure_count, 1);
    }

    #[test]
    fn surface_publisher_never_advertises_avc444() {
        let publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
        let capabilities = publisher.capabilities();

        assert!(capabilities.iter().any(|capability| matches!(
            capability,
            CapabilitySet::V8_1 { flags }
                if flags.contains(ironrdp_egfx::pdu::CapabilitiesV81Flags::AVC420_ENABLED)
        )));
        assert!(capabilities.iter().all(|capability| {
            !matches!(capability, CapabilitySet::V10_7 { .. })
                && !matches!(capability, CapabilitySet::V10_6 { .. })
                && !matches!(capability, CapabilitySet::V10_5 { .. })
                && !matches!(capability, CapabilitySet::V10_4 { .. })
                && !matches!(capability, CapabilitySet::V10_3 { .. })
                && !matches!(capability, CapabilitySet::V10_2 { .. })
                && !matches!(capability, CapabilitySet::V10_1)
        }));
    }

    #[test]
    fn avc444_validator_accepts_the_two_stream_luma_and_chroma_envelope() {
        let bitmap = encode_avc444_fixture(ironrdp_egfx::pdu::Encoding::LUMA_AND_CHROMA, true);

        let validated = validate_avc444_bitmap(&bitmap).expect("valid AVC444 envelope");
        assert_eq!(validated.encoding, ValidatedAvc444Encoding::LumaAndChroma);
        assert_eq!(validated.stream1, &[0, 0, 0, 2, 0x65, 0x88]);
        assert_eq!(validated.stream2, Some(&[0, 0, 0, 2, 0x65, 0x99][..]));
        assert_eq!(validated.stream1_regions.len(), 1);
        assert_eq!(
            validated
                .stream2_regions
                .as_deref()
                .map_or(0, |regions| regions.len()),
            1
        );
        assert_eq!(validated.stream1_region_count, 1);
        assert_eq!(validated.stream2_region_count, 1);
    }

    #[test]
    fn avc444_validator_accepts_luma_and_chroma_subframes_without_stream2() {
        for (encoding, expected) in [
            (
                ironrdp_egfx::pdu::Encoding::LUMA,
                ValidatedAvc444Encoding::Luma,
            ),
            (
                ironrdp_egfx::pdu::Encoding::CHROMA,
                ValidatedAvc444Encoding::Chroma,
            ),
        ] {
            let bitmap = encode_avc444_fixture(encoding, false);
            let validated = validate_avc444_bitmap(&bitmap).expect("valid AVC444 subframe");
            assert_eq!(validated.encoding, expected);
            assert!(validated.stream2.is_none());
            assert!(validated.stream2_regions.is_none());
            assert_eq!(validated.stream1_region_count, 1);
            assert_eq!(validated.stream2_region_count, 0);
        }
    }

    #[test]
    fn avc444_validator_rejects_reserved_encoding_and_trailing_bytes() {
        let mut reserved =
            encode_avc444_fixture(ironrdp_egfx::pdu::Encoding::LUMA_AND_CHROMA, true);
        reserved[3] |= 0xc0;
        assert!(validate_avc444_bitmap(&reserved).is_err());

        let mut trailing = encode_avc444_fixture(ironrdp_egfx::pdu::Encoding::LUMA, false);
        trailing.extend_from_slice(&[0xaa]);
        assert!(validate_avc444_bitmap(&trailing).is_err());

        let mut empty_region = encode_avc444_fixture(ironrdp_egfx::pdu::Encoding::LUMA, false);
        // Outer streamInfo (4), nRect (4), then RDPGFX_RECT16 left/top/right.
        empty_region[12..14].copy_from_slice(&0_u16.to_le_bytes());
        assert!(validate_avc444_bitmap(&empty_region).is_err());
    }

    #[test]
    fn surface_publisher_rejects_avc444_before_color_reconstruction_exists() {
        let mut publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
        let bitmap = encode_avc444_fixture(ironrdp_egfx::pdu::Encoding::LUMA_AND_CHROMA, true);
        for codec_id in [Codec1Type::Avc444, Codec1Type::Avc444v2] {
            publisher.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: 1,
                codec_id,
                pixel_format: EgfxPixelFormat::XRgb,
                destination_rectangle: ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
                bitmap_data: bitmap.clone(),
            }));
        }
        assert_eq!(publisher.rejected_update_count(), 2);
        assert!(publisher.drain().is_empty());
    }

    #[test]
    fn surface_publisher_dispatches_avc444_only_with_an_explicit_decoder() {
        let session_id = SessionId::allocate();
        let publisher = EgfxSurfacePublisher::try_new(session_id, 1)
            .unwrap()
            .with_avc444_decoder(Box::new(SolidAvc444Decoder));
        assert!(publisher
            .capabilities()
            .iter()
            .any(|capability| matches!(capability, CapabilitySet::V10_7 { .. })));

        let publisher_state = publisher.clone();
        let mut adapter = EgfxAdapter::with_surface_publisher(None, publisher);
        adapter.start(7).expect("EGFX start succeeds");
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu::from_typed(
                &CapabilitySet::V10_7 {
                    flags: CapabilitiesV107Flags::SMALL_CACHE,
                },
            )),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::ResetGraphics(ResetGraphicsPdu {
                width: 2,
                height: 2,
                monitors: Vec::new(),
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CreateSurface(CreateSurfacePdu {
                surface_id: 1,
                width: 2,
                height: 2,
                pixel_format: EgfxPixelFormat::XRgb,
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: 1,
                output_origin_x: 0,
                output_origin_y: 0,
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::StartFrame(StartFramePdu {
                timestamp: Timestamp {
                    milliseconds: 0,
                    seconds: 0,
                    minutes: 0,
                    hours: 0,
                },
                frame_id: 1,
            }),
        );
        let bitmap = encode_avc444_fixture(ironrdp_egfx::pdu::Encoding::LUMA, false);
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: 1,
                codec_id: Codec1Type::Avc444,
                pixel_format: EgfxPixelFormat::XRgb,
                destination_rectangle: ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
                bitmap_data: bitmap,
            }),
        );
        let responses = adapter
            .process(
                7,
                &wrap_uncompressed(
                    &encode_vec(&GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }))
                        .expect("encode EndFrame"),
                ),
            )
            .expect("EndFrame produces a frame acknowledgement");
        assert_eq!(responses.len(), 1);

        let updates = adapter.drain_surface_updates();
        assert_eq!(updates.len(), 3);
        assert!(matches!(
            updates[0],
            SurfaceUpdate::Reset { generation: 2, .. }
        ));
        assert!(matches!(
            &updates[1],
            SurfaceUpdate::Damage { patches, .. }
                if patches.len() == 1
                    && patches[0].pixels.as_bytes()
                        == [0x30, 0x20, 0x10, 0xff, 0x30, 0x20, 0x10, 0xff,
                            0x30, 0x20, 0x10, 0xff, 0x30, 0x20, 0x10, 0xff]
        ));
        assert!(matches!(
            updates[2],
            SurfaceUpdate::FrameBoundary {
                completeness: FrameCompleteness::FullBaseline,
                ..
            }
        ));
        assert_eq!(publisher_state.rejected_update_count(), 0);
    }

    #[test]
    fn avc444_publication_applies_substream_region_masks_to_nonzero_destination() {
        let session_id = SessionId::allocate();
        let mut publisher = EgfxSurfacePublisher::try_new(session_id, 1)
            .unwrap()
            .with_avc444_decoder(Box::new(SolidAvc444Decoder));
        publisher.on_reset_graphics(8, 8);
        super::lock_state(&publisher.state).surfaces.insert(
            1,
            super::EgfxSurface {
                width: 8,
                height: 8,
                origin_x: 0,
                origin_y: 0,
                mapped: true,
            },
        );

        let regions = [
            Avc420Region::new(3, 4, 3, 4, 22, 100),
            Avc420Region::new(5, 6, 5, 6, 22, 100),
        ];
        let stream_data = [0, 0, 0, 2, 0x65, 0x88];
        let stream = Avc420BitmapStream {
            rectangles: regions.iter().map(Avc420Region::to_rectangle).collect(),
            quant_qual_vals: regions.iter().map(Avc420Region::to_quant_quality).collect(),
            data: &stream_data,
        };
        let bitmap = Avc444BitmapStream {
            encoding: ironrdp_egfx::pdu::Encoding::LUMA,
            stream1: stream,
            stream2: None,
        };
        let mut bitmap_data = vec![0_u8; bitmap.size()];
        let mut cursor = WriteCursor::new(&mut bitmap_data);
        bitmap
            .encode(&mut cursor)
            .expect("AVC444 region fixture encodes");

        publisher.on_unhandled_pdu(&GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: 1,
            codec_id: Codec1Type::Avc444,
            pixel_format: EgfxPixelFormat::XRgb,
            destination_rectangle: ExclusiveRectangle {
                left: 2,
                top: 3,
                right: 6,
                bottom: 7,
            },
            bitmap_data,
        }));

        let state = super::lock_state(&publisher.state);
        assert!(!state.disabled);
        assert_eq!(state.pending_patches.len(), 2);
        assert_eq!(
            state.pending_patches[0].rect,
            PixelRect {
                x: 3,
                y: 4,
                width: 1,
                height: 1,
            }
        );
        assert_eq!(
            state.pending_patches[1].rect,
            PixelRect {
                x: 5,
                y: 6,
                width: 1,
                height: 1,
            }
        );
        assert!(state
            .pending_patches
            .iter()
            .all(|patch| patch.pixels.len() == 4));
    }

    #[test]
    fn publisher_disables_egfx_before_a_reset_changes_decoder_coded_size() {
        let session_id = SessionId::allocate();
        let expected = PixelSize::new(1280, 720).unwrap();
        let mut publisher = EgfxSurfacePublisher::try_new(session_id, 1)
            .unwrap()
            .with_expected_coded_size(expected);

        publisher.on_reset_graphics(1024, 768);

        let state = super::lock_state(&publisher.state);
        assert!(state.disabled);
        assert_eq!(state.generation, 1);
        assert!(state.updates.is_empty());
        drop(state);
        assert_eq!(publisher.rejected_update_count(), 1);
    }

    #[test]
    fn publisher_accepts_a_reset_matching_decoder_coded_size() {
        let session_id = SessionId::allocate();
        let expected = PixelSize::new(1280, 720).unwrap();
        let mut publisher = EgfxSurfacePublisher::try_new(session_id, 1)
            .unwrap()
            .with_expected_coded_size(expected);

        publisher.on_reset_graphics(1280, 720);

        let state = super::lock_state(&publisher.state);
        assert!(!state.disabled);
        assert_eq!(state.generation, 2);
        assert_eq!(state.output_size, Some(expected));
        drop(state);
        assert!(matches!(
            publisher.drain().as_slice(),
            [SurfaceUpdate::Reset { size, .. }] if *size == expected
        ));
    }

    #[test]
    fn avc444_crop_converts_only_the_requested_odd_rectangle() {
        let frame_width = 7;
        let frame_height = 5;
        let y = (0..frame_width * frame_height)
            .map(|index| (16 + index * 3) as u8)
            .collect::<Vec<_>>();
        let u = (0..frame_width * frame_height)
            .map(|index| (80 + index * 5) as u8)
            .collect::<Vec<_>>();
        let v = (0..frame_width * frame_height)
            .map(|index| (120 + index * 7) as u8)
            .collect::<Vec<_>>();
        let frame = Yuv444Frame::from_test_planes(
            frame_width,
            frame_height,
            y.clone(),
            u.clone(),
            v.clone(),
        )
        .expect("test YUV444 frame");
        let destination = ExclusiveRectangle {
            left: 1,
            top: 1,
            right: 6,
            bottom: 5,
        };

        let cropped = super::crop_yuv444_to_rgba(&frame, &destination)
            .expect("non-zero AVC444 crop converts");
        assert_eq!((cropped.width(), cropped.height()), (5, 4));

        let mut full = vec![0_u8; frame_width * frame_height * 4];
        convert_yuv444_to_rgba(
            frame_width,
            frame_height,
            &y,
            frame_width,
            &u,
            frame_width,
            &v,
            frame_width,
            &mut full,
        )
        .expect("full reference conversion");
        let mut expected = Vec::with_capacity(5 * 4 * 4);
        for row in 1..5 {
            let start = (row * frame_width + 1) * 4;
            expected.extend_from_slice(&full[start..start + 5 * 4]);
        }
        assert_eq!(cropped.data(), expected.as_slice());
    }

    #[test]
    fn avc444_crop_rejects_empty_and_out_of_bounds_rectangles() {
        let frame = Yuv444Frame::from_test_planes(3, 3, vec![16; 9], vec![128; 9], vec![128; 9])
            .expect("test YUV444 frame");
        for destination in [
            ExclusiveRectangle {
                left: 1,
                top: 1,
                right: 1,
                bottom: 2,
            },
            ExclusiveRectangle {
                left: 2,
                top: 2,
                right: 4,
                bottom: 3,
            },
        ] {
            assert!(
                super::crop_yuv444_to_rgba(&frame, &destination).is_err(),
                "invalid AVC444 rectangle must fail closed"
            );
        }
    }

    fn encode_avc444_fixture(
        encoding: ironrdp_egfx::pdu::Encoding,
        include_stream2: bool,
    ) -> Vec<u8> {
        let region = Avc420Region::full_frame(2, 2, 22);
        let stream1_data = [0, 0, 0, 2, 0x65, 0x88];
        let stream2_data = [0, 0, 0, 2, 0x65, 0x99];
        let stream1 = Avc420BitmapStream {
            rectangles: vec![region.to_rectangle()],
            quant_qual_vals: vec![region.to_quant_quality()],
            data: &stream1_data,
        };
        let stream2 = include_stream2.then(|| Avc420BitmapStream {
            rectangles: vec![region.to_rectangle()],
            quant_qual_vals: vec![region.to_quant_quality()],
            data: &stream2_data,
        });
        let stream = Avc444BitmapStream {
            encoding,
            stream1,
            stream2,
        };
        let mut encoded = vec![0_u8; stream.size()];
        let mut cursor = WriteCursor::new(&mut encoded);
        stream.encode(&mut cursor).expect("fixture encodes");
        encoded
    }

    #[test]
    fn surface_publisher_maps_mapped_avc420_damage_into_surface_updates() {
        let session_id = SessionId::allocate();
        let mut publisher = EgfxSurfacePublisher::try_new(session_id, 6).expect("generation");

        publisher.on_reset_graphics(4, 2);
        super::lock_state(&publisher.state).surfaces.insert(
            4,
            super::EgfxSurface {
                width: 4,
                height: 2,
                origin_x: 0,
                origin_y: 0,
                mapped: true,
            },
        );
        publisher.queue_bitmap_update(
            4,
            &ExclusiveRectangle {
                left: 0,
                top: 0,
                right: 4,
                bottom: 2,
            },
            Codec1Type::Avc420,
            &vec![1, 2, 3, 4]
                .into_iter()
                .cycle()
                .take(4 * 2 * 4)
                .collect::<Vec<_>>(),
            4,
            2,
        );
        publisher.on_frame_complete(11);

        let updates = publisher.drain();
        assert_eq!(
            updates.len(),
            3,
            "a complete startup frame emits one damage and one boundary"
        );
        assert!(matches!(
            updates.first(),
            Some(SurfaceUpdate::Reset {
                session_id: actual_session,
                generation: 7,
                size: frd_core::PixelSize {
                    width: 4,
                    height: 2
                },
                format: PixelFormat::Bgrx8UnormSrgb,
            }) if *actual_session == session_id
        ));
        match &updates[1] {
            SurfaceUpdate::Damage {
                session_id: actual_session,
                generation: 7,
                revision: 1,
                patches,
            } => {
                assert_eq!(*actual_session, session_id);
                assert_eq!(patches.len(), 1);
                assert_eq!(
                    patches[0].rect,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 4,
                        height: 2
                    }
                );
                assert_eq!(patches[0].stride_bytes, 16);
                assert_eq!(patches[0].pixels.len(), 32);
                assert_eq!(&patches[0].pixels.as_bytes()[..4], &[3, 2, 1, 0xff]);
            }
            other => panic!("unexpected damage update: {other:?}"),
        }
        assert!(matches!(
            updates[2],
            SurfaceUpdate::FrameBoundary {
                generation: 7,
                revision: 1,
                completeness: FrameCompleteness::FullBaseline,
                ..
            }
        ));
        assert_eq!(publisher.rejected_update_count(), 0);
    }

    #[test]
    fn surface_publisher_marks_the_first_complete_frame_as_full_baseline() {
        let session_id = SessionId::allocate();
        let mut publisher = EgfxSurfacePublisher::try_new(session_id, 1).unwrap();
        publisher.on_reset_graphics(4, 2);
        super::lock_state(&publisher.state).surfaces.insert(
            1,
            super::EgfxSurface {
                width: 4,
                height: 2,
                origin_x: 0,
                origin_y: 0,
                mapped: true,
            },
        );
        publisher.queue_bitmap_update(
            1,
            &ExclusiveRectangle {
                left: 0,
                top: 0,
                right: 2,
                bottom: 2,
            },
            Codec1Type::Avc420,
            &vec![0; 2 * 2 * 4],
            2,
            2,
        );
        publisher.queue_bitmap_update(
            1,
            &ExclusiveRectangle {
                left: 2,
                top: 0,
                right: 4,
                bottom: 2,
            },
            Codec1Type::Avc420,
            &vec![0; 2 * 2 * 4],
            2,
            2,
        );
        publisher.on_frame_complete(1);
        publisher.queue_bitmap_update(
            1,
            &ExclusiveRectangle {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            Codec1Type::Avc420,
            &vec![0; 4],
            1,
            1,
        );
        publisher.on_frame_complete(2);

        let updates = publisher.drain();
        assert!(matches!(
            updates.get(2),
            Some(SurfaceUpdate::FrameBoundary {
                generation: 2,
                revision: 1,
                completeness: FrameCompleteness::FullBaseline,
                ..
            })
        ));
        assert!(matches!(
            updates.get(1),
            Some(SurfaceUpdate::Damage { patches, revision: 1, .. }) if patches.len() == 2
        ));
        assert!(matches!(
            updates.get(4),
            Some(SurfaceUpdate::FrameBoundary {
                generation: 2,
                revision: 2,
                completeness: FrameCompleteness::Incremental,
                ..
            })
        ));
    }

    #[test]
    fn surface_publisher_rejects_unmapped_or_malformed_damage_without_emitting_it() {
        let mut publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
        publisher.on_reset_graphics(20, 20);
        super::lock_state(&publisher.state).surfaces.insert(
            9,
            super::EgfxSurface {
                width: 10,
                height: 10,
                origin_x: 0,
                origin_y: 0,
                mapped: false,
            },
        );
        publisher.queue_bitmap_update(
            9,
            &ExclusiveRectangle {
                left: 0,
                top: 0,
                right: 2,
                bottom: 2,
            },
            Codec1Type::Avc420,
            &vec![0; 2 * 2 * 4 - 1],
            2,
            2,
        );

        let updates = publisher.drain();
        assert_eq!(updates.len(), 1, "only Reset is valid before mapping");
        assert_eq!(publisher.rejected_update_count(), 1);
    }

    #[test]
    fn surface_publisher_does_not_advance_generation_when_reset_queue_is_full() {
        let mut publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
        for _ in 0..super::MAX_PENDING_SURFACE_UPDATES {
            publisher.on_reset_graphics(1, 1);
        }
        let generation_before_rejected_reset = super::lock_state(&publisher.state).generation;
        publisher.on_reset_graphics(1, 1);

        let state = super::lock_state(&publisher.state);
        assert_eq!(state.generation, generation_before_rejected_reset);
        assert_eq!(state.updates.len(), super::MAX_PENDING_SURFACE_UPDATES);
        assert_eq!(state.rejected_updates, 1);
    }

    #[test]
    fn h264_decoder_bridge_validates_avc_and_converts_yuv420_to_rgba() {
        let session_id = SessionId::allocate();
        let stream = avc420_stream_config(
            session_id,
            4,
            PixelSize::new(2, 2).unwrap(),
            vec![0x67, 0x42].into_boxed_slice(),
            vec![0x68, 0xce].into_boxed_slice(),
        )
        .unwrap();
        let frame = black_yuv420_frame(&stream);
        let mut decoder = EgfxH264Decoder::from_decoder(
            stream,
            Box::new(OneFrameDecoder {
                frame,
                resets: Vec::new(),
                fail_reset: false,
            }),
        );

        let decoded = decoder
            .decode(&[0, 0, 0, 2, 0x65, 0x88])
            .expect("one complete AVC access unit produces one frame");
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 2);
        assert_eq!(
            decoded.data(),
            &[0, 0, 0, 0xff, 0, 0, 0, 0xff, 0, 0, 0, 0xff, 0, 0, 0, 0xff]
        );

        assert!(decoder.decode(&[0, 0, 0, 4, 0x65]).is_err());
        decoder.reset();
        assert!(decoder.decode(&[0, 0, 0, 2, 0x65, 0x88]).is_ok());
    }

    #[test]
    fn h264_decoder_bridge_accepts_rdp_annex_b_access_units() {
        let session_id = SessionId::allocate();
        let stream = avc420_stream_config(
            session_id,
            4,
            PixelSize::new(2, 2).unwrap(),
            vec![0x67, 0x42].into_boxed_slice(),
            vec![0x68, 0xce].into_boxed_slice(),
        )
        .unwrap();
        let frame = black_yuv420_frame(&stream);
        let mut decoder = EgfxH264Decoder::from_decoder(
            stream,
            Box::new(OneFrameDecoder {
                frame,
                resets: Vec::new(),
                fail_reset: false,
            }),
        );

        let decoded = decoder
            .decode(&[0, 0, 0, 1, 0x65, 0x88])
            .expect("RDP AVC420 Annex-B access unit is normalized at the adapter boundary");
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
    }

    #[test]
    fn h264_decoder_bridge_requires_an_exact_factory_capability() {
        let factory = ExactFactory;
        assert!(EgfxH264Decoder::try_new(
            &factory,
            SessionId::allocate(),
            1,
            PixelSize::new(2, 2).unwrap(),
            vec![0x67, 0x42].into_boxed_slice(),
            vec![0x68, 0xce].into_boxed_slice(),
        )
        .is_ok());
    }

    #[test]
    fn h264_decoder_reset_failure_is_retained_as_a_fail_closed_state() {
        let stream = avc420_stream_config(
            SessionId::allocate(),
            1,
            PixelSize::new(2, 2).unwrap(),
            vec![0x67, 0x42].into_boxed_slice(),
            vec![0x68, 0xce].into_boxed_slice(),
        )
        .unwrap();
        let frame = black_yuv420_frame(&stream);
        let mut decoder = EgfxH264Decoder::from_decoder(
            stream,
            Box::new(OneFrameDecoder {
                frame,
                resets: Vec::new(),
                fail_reset: true,
            }),
        );

        assert!(decoder.reset_checked().is_err());
        assert!(decoder.failed);
        assert!(decoder.decode(&[0, 0, 0, 2, 0x65, 0x88]).is_err());
    }

    #[test]
    fn egfx_adapter_drops_reset_when_h264_decoder_reset_fails() {
        let session_id = SessionId::allocate();
        let stream = avc420_stream_config(
            session_id,
            1,
            PixelSize::new(2, 2).unwrap(),
            vec![0x67, 0x42].into_boxed_slice(),
            vec![0x68, 0xce].into_boxed_slice(),
        )
        .unwrap();
        let decoder = EgfxH264Decoder::from_decoder(
            stream.clone(),
            Box::new(OneFrameDecoder {
                frame: black_yuv420_frame(&stream),
                resets: Vec::new(),
                fail_reset: true,
            }),
        );
        let publisher = EgfxSurfacePublisher::try_new(session_id, 1).expect("generation");
        let mut adapter = EgfxAdapter::with_surface_publisher(Some(Box::new(decoder)), publisher);

        adapter.start(7).expect("EGFX start succeeds");
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu::from_typed(&CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED,
            })),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::ResetGraphics(ResetGraphicsPdu {
                width: 2,
                height: 2,
                monitors: Vec::new(),
            }),
        );

        assert!(adapter.is_failed());
        assert!(adapter
            .surface_publisher
            .as_ref()
            .is_some_and(EgfxSurfacePublisher::is_disabled));
        assert!(adapter.drain_surface_updates().is_empty());
    }

    #[test]
    fn avc420_provider_requires_parameter_sets_and_builds_the_exact_bridge() {
        let parameter_sets = VideoParameterSets::try_new(
            None,
            vec![0x67, 0x42].into_boxed_slice(),
            vec![0x68, 0xce].into_boxed_slice(),
        )
        .expect("test parameter sets");
        let provider = Avc420DecoderProvider::new(Arc::new(ExactFactory), parameter_sets);
        assert!(provider
            .create_decoder(SessionId::allocate(), PixelSize::new(2, 2).unwrap())
            .is_some());
    }

    #[test]
    fn lazy_avc420_provider_extracts_parameter_sets_from_the_first_access_unit() {
        let provider = Avc420DecoderProvider::from_factory(Arc::new(ExactFactory));
        let mut decoder = provider
            .create_decoder(SessionId::allocate(), PixelSize::new(2, 2).unwrap())
            .expect("lazy provider creates a decoder shell");

        let without_parameter_sets = [0, 0, 0, 2, 0x65, 0x88];
        assert!(decoder.decode(&without_parameter_sets).is_err());

        let first_access_unit = [
            0, 0, 0, 2, 0x67, 0x42, // SPS
            0, 0, 0, 2, 0x68, 0xce, // PPS
            0, 0, 0, 2, 0x65, 0x88, // IDR
        ];
        let frame = decoder
            .decode(&first_access_unit)
            .expect("SPS/PPS-bearing access unit configures the exact decoder");
        assert_eq!((frame.width(), frame.height()), (2, 2));
    }

    #[test]
    fn avc444_provider_reconstructs_luma_and_chroma_with_one_stream_decoder() {
        let session_id = SessionId::allocate();
        let provider = Avc444DecoderProvider::new(Arc::new(ExactFactory));
        let mut decoder = Avc444VideoDecoder::try_new(
            Arc::new(ExactFactory),
            session_id,
            PixelSize::new(2, 2).unwrap(),
        )
        .expect("exact AVC444 provider creates a decoder");
        decoder.reset(2, 2).expect("reset accepts coded size");
        decoder.surfaces.insert(
            1,
            super::Avc444SurfaceDecoderState {
                size: PixelSize::new(2, 2).unwrap(),
                reconstructor: super::Yuv444Reconstructor::try_new(2, 2).unwrap(),
            },
        );

        let first_access_unit = [
            0, 0, 0, 2, 0x67, 0x42, // SPS
            0, 0, 0, 2, 0x68, 0xce, // PPS
            0, 0, 0, 2, 0x65, 0x88, // IDR
        ];
        let region = Avc420Region::full_frame(2, 2, 22);
        let stream = Avc420BitmapStream {
            rectangles: vec![region.to_rectangle()],
            quant_qual_vals: vec![region.to_quant_quality()],
            data: &first_access_unit,
        };
        let bitmap = Avc444BitmapStream {
            encoding: ironrdp_egfx::pdu::Encoding::LUMA_AND_CHROMA,
            stream1: stream,
            stream2: Some(Avc420BitmapStream {
                rectangles: vec![region.to_rectangle()],
                quant_qual_vals: vec![region.to_quant_quality()],
                data: &[0, 0, 0, 2, 0x65, 0x99],
            }),
        };
        let mut encoded = vec![0_u8; bitmap.size()];
        let mut cursor = WriteCursor::new(&mut encoded);
        bitmap.encode(&mut cursor).expect("AVC444 fixture encodes");
        let validated = validate_avc444_bitmap(&encoded).expect("AVC444 fixture validates");
        assert_eq!(validated.stream1_regions[0].right, 1);
        assert_eq!(validated.stream1_regions[0].bottom, 1);
        assert_eq!(
            validated
                .stream2_regions
                .as_deref()
                .expect("luma and chroma has stream2")[0]
                .right,
            1
        );
        let frame = decoder
            .decode(
                Codec1Type::Avc444,
                1,
                &validated,
                &ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
            )
            .expect("AVC444 luma and chroma decode");
        assert_eq!((frame.width(), frame.height()), (2, 2));
        assert_eq!(
            frame.data(),
            &[0, 0, 0, 0xff, 0, 0, 0, 0xff, 0, 24, 0, 0xff, 0, 24, 0, 0xff,]
        );

        // A subsequent CHROMA-only PDU places its auxiliary subframe and
        // rectangles in stream1. It must reuse the luma reference established
        // above and update the existing surface instead of being rejected as
        // a missing stream2.
        let chroma_bitmap = Avc444BitmapStream {
            encoding: ironrdp_egfx::pdu::Encoding::CHROMA,
            stream1: Avc420BitmapStream {
                rectangles: vec![region.to_rectangle()],
                quant_qual_vals: vec![region.to_quant_quality()],
                data: &[0, 0, 0, 2, 0x65, 0x99],
            },
            stream2: None,
        };
        let mut encoded_chroma = vec![0_u8; chroma_bitmap.size()];
        let mut chroma_cursor = WriteCursor::new(&mut encoded_chroma);
        chroma_bitmap
            .encode(&mut chroma_cursor)
            .expect("AVC444 chroma-only fixture encodes");
        let validated_chroma =
            validate_avc444_bitmap(&encoded_chroma).expect("AVC444 chroma-only fixture validates");
        let chroma_frame = decoder
            .decode(
                Codec1Type::Avc444,
                1,
                &validated_chroma,
                &ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
            )
            .expect("AVC444 chroma-only update reuses the luma reference");
        assert_eq!((chroma_frame.width(), chroma_frame.height()), (2, 2));
        let chroma_v2_frame = decoder
            .decode(
                Codec1Type::Avc444v2,
                1,
                &validated_chroma,
                &ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
            )
            .expect("AVC444v2 chroma-only update reuses the luma reference");
        assert_eq!((chroma_v2_frame.width(), chroma_v2_frame.height()), (2, 2));

        assert!(provider
            .create_avc444_decoder(session_id, PixelSize::new(2, 2).unwrap())
            .is_some());
    }

    #[test]
    fn egfx_adapter_processes_an_avc420_pdu_frame_into_surface_updates() {
        let session_id = SessionId::allocate();
        let publisher = EgfxSurfacePublisher::try_new(session_id, 1).expect("generation");
        let parameter_sets = VideoParameterSets::try_new(
            None,
            vec![0x67, 0x42].into_boxed_slice(),
            vec![0x68, 0xce].into_boxed_slice(),
        )
        .expect("test parameter sets");
        let provider = Avc420DecoderProvider::new(Arc::new(ExactFactory), parameter_sets);
        let decoder = provider
            .create_decoder(session_id, PixelSize::new(2, 2).unwrap())
            .expect("exact AVC420 provider creates the bridge");
        let mut adapter = EgfxAdapter::with_surface_publisher(Some(decoder), publisher);

        adapter
            .start(7)
            .expect("EGFX start emits a capability advertisement");
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu::from_typed(&CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED,
            })),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::ResetGraphics(ResetGraphicsPdu {
                width: 2,
                height: 2,
                monitors: Vec::new(),
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CreateSurface(CreateSurfacePdu {
                surface_id: 1,
                width: 2,
                height: 2,
                pixel_format: EgfxPixelFormat::XRgb,
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: 1,
                output_origin_x: 0,
                output_origin_y: 0,
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::StartFrame(StartFramePdu {
                timestamp: Timestamp {
                    milliseconds: 0,
                    seconds: 0,
                    minutes: 0,
                    hours: 0,
                },
                frame_id: 9,
            }),
        );

        let avc_payload = vec![0, 0, 0, 2, 0x65, 0x88];
        let bitmap_data = ironrdp_egfx::pdu::encode_avc420_bitmap_stream(
            &[Avc420Region::full_frame(2, 2, 22)],
            &avc_payload,
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: 1,
                codec_id: Codec1Type::Avc420,
                pixel_format: EgfxPixelFormat::XRgb,
                destination_rectangle: ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
                bitmap_data,
            }),
        );
        let responses = adapter
            .process(
                7,
                &wrap_uncompressed(
                    &encode_vec(&GfxPdu::EndFrame(EndFramePdu { frame_id: 9 }))
                        .expect("encode EndFrame"),
                ),
            )
            .expect("EndFrame produces a frame acknowledgement");
        assert_eq!(responses.len(), 1);

        let updates = adapter.drain_surface_updates();
        assert_eq!(updates.len(), 3);
        assert!(matches!(
            updates[0],
            SurfaceUpdate::Reset { generation: 2, .. }
        ));
        assert!(matches!(
            &updates[1],
            SurfaceUpdate::Damage {
                generation: 2,
                revision: 1,
                patches,
                ..
            } if patches.len() == 1 && patches[0].pixels.as_bytes() == [
                0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff,
                0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff,
            ]
        ));
        assert!(matches!(
            updates[2],
            SurfaceUpdate::FrameBoundary {
                generation: 2,
                revision: 1,
                completeness: FrameCompleteness::FullBaseline,
                ..
            }
        ));
    }

    #[test]
    fn egfx_decoder_failure_disables_the_stream_and_keeps_legacy_processing_alive() {
        let session_id = SessionId::allocate();
        let publisher = EgfxSurfacePublisher::try_new(session_id, 1).expect("generation");
        let mut adapter =
            EgfxAdapter::with_surface_publisher(Some(Box::new(FailingH264Decoder)), publisher);

        adapter.start(7).expect("EGFX start succeeds");
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu::from_typed(&CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED,
            })),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::ResetGraphics(ResetGraphicsPdu {
                width: 2,
                height: 2,
                monitors: Vec::new(),
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CreateSurface(CreateSurfacePdu {
                surface_id: 1,
                width: 2,
                height: 2,
                pixel_format: EgfxPixelFormat::XRgb,
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: 1,
                output_origin_x: 0,
                output_origin_y: 0,
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: 1,
                codec_id: Codec1Type::Avc420,
                pixel_format: EgfxPixelFormat::XRgb,
                destination_rectangle: ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
                bitmap_data: ironrdp_egfx::pdu::encode_avc420_bitmap_stream(
                    &[Avc420Region::full_frame(2, 2, 22)],
                    &[0, 0, 0, 2, 0x65, 0x88],
                ),
            }),
        );

        assert!(adapter.is_failed());
        assert!(adapter.drain_surface_updates().is_empty());

        // A failed EGFX stream is ignored thereafter; the surrounding ActiveStage
        // can continue handling legacy Bitmap/RemoteFX PDUs.
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::ResetGraphics(ResetGraphicsPdu {
                width: 2,
                height: 2,
                monitors: Vec::new(),
            }),
        );
        assert!(adapter.drain_surface_updates().is_empty());
    }

    fn clear_adapter(width: u32, height: u32) -> EgfxAdapter {
        let publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
        let mut adapter = EgfxAdapter::with_surface_publisher(None, publisher);
        adapter.start(7).unwrap();
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu::from_typed(
                &CapabilitySet::V10_7 {
                    flags: CapabilitiesV107Flags::SMALL_CACHE,
                },
            )),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::ResetGraphics(ResetGraphicsPdu {
                width,
                height,
                monitors: Vec::new(),
            }),
        );
        adapter
    }
    fn clear_surface(adapter: &mut EgfxAdapter, id: u16, w: u16, h: u16, x: u32, y: u32) {
        process_gfx_pdu(
            adapter,
            GfxPdu::CreateSurface(CreateSurfacePdu {
                surface_id: id,
                width: w,
                height: h,
                pixel_format: EgfxPixelFormat::XRgb,
            }),
        );
        process_gfx_pdu(
            adapter,
            GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: id,
                output_origin_x: x,
                output_origin_y: y,
            }),
        );
    }
    fn clear_wire(
        id: u16,
        seq: u8,
        rect: ExclusiveRectangle,
        color: [u8; 3],
        glyph: bool,
    ) -> GfxPdu {
        let mut data = vec![if glyph { 1 } else { 0 }, seq];
        if glyph {
            data.extend(42u16.to_le_bytes());
        }
        data.extend(4u32.to_le_bytes());
        data.extend(0u32.to_le_bytes());
        data.extend(0u32.to_le_bytes());
        data.extend(color);
        data.push(((rect.right - rect.left) * (rect.bottom - rect.top)) as u8);
        GfxPdu::WireToSurface1(WireToSurface1Pdu {
            surface_id: id,
            codec_id: Codec1Type::ClearCodec,
            pixel_format: EgfxPixelFormat::XRgb,
            destination_rectangle: rect,
            bitmap_data: data,
        })
    }
    #[test]
    fn clearcodec_gfx_pdu_shares_sequence_and_glyph_across_surfaces_and_reset() {
        let mut adapter = clear_adapter(4, 2);
        let rect = ExclusiveRectangle {
            left: 0,
            top: 0,
            right: 2,
            bottom: 2,
        };
        clear_surface(&mut adapter, 1, 2, 2, 0, 0);
        clear_surface(&mut adapter, 2, 2, 2, 2, 0);
        process_gfx_pdu(
            &mut adapter,
            clear_wire(1, 0, rect.clone(), [11, 22, 33], true),
        );
        process_gfx_pdu(
            &mut adapter,
            clear_wire(2, 1, rect.clone(), [44, 55, 66], false),
        );
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }));
        assert!(!adapter.is_failed());
        let updates = adapter.drain_surface_updates();
        assert!(
            matches!(&updates[1],SurfaceUpdate::Damage{generation:2,patches,..} if patches.len()==2 && patches[0].pixels.as_bytes()==[11,22,33,255].repeat(4) && patches[1].rect.x==2 && patches[1].pixels.as_bytes()==[44,55,66,255].repeat(4))
        );
        assert!(matches!(
            updates[2],
            SurfaceUpdate::FrameBoundary {
                completeness: FrameCompleteness::FullBaseline,
                ..
            }
        ));
        // 普通图形reset产生新generation，但不能从0重启会话级序号或丢失glyph。
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::ResetGraphics(ResetGraphicsPdu {
                width: 2,
                height: 2,
                monitors: Vec::new(),
            }),
        );
        clear_surface(&mut adapter, 3, 2, 2, 0, 0);
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: 3,
                codec_id: Codec1Type::ClearCodec,
                pixel_format: EgfxPixelFormat::XRgb,
                destination_rectangle: rect,
                bitmap_data: vec![3, 2, 42, 0],
            }),
        );
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 2 }));
        let updates = adapter.drain_surface_updates();
        assert!(!adapter.is_failed());
        assert!(
            matches!(&updates[1],SurfaceUpdate::Damage{generation:3,patches,..} if patches[0].pixels.as_bytes()==[11,22,33,255].repeat(4))
        );
    }
    #[test]
    fn clearcodec_gfx_pdu_preserves_crop_coordinates_and_bgr_channels() {
        let mut adapter = clear_adapter(4, 3);
        clear_surface(&mut adapter, 1, 4, 3, 0, 0);
        process_gfx_pdu(
            &mut adapter,
            clear_wire(
                1,
                0,
                ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 4,
                    bottom: 3,
                },
                [0, 0, 0],
                false,
            ),
        );
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }));
        adapter.drain_surface_updates();
        process_gfx_pdu(
            &mut adapter,
            clear_wire(
                1,
                1,
                ExclusiveRectangle {
                    left: 1,
                    top: 1,
                    right: 3,
                    bottom: 3,
                },
                [1, 17, 239],
                false,
            ),
        );
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 2 }));
        let updates = adapter.drain_surface_updates();
        assert!(
            matches!(&updates[0],SurfaceUpdate::Damage{patches,..} if patches[0].rect==PixelRect{x:1,y:1,width:2,height:2} && patches[0].stride_bytes==8 && patches[0].pixels.as_bytes()==[1,17,239,255].repeat(4))
        );
        assert!(matches!(
            updates[1],
            SurfaceUpdate::FrameBoundary {
                completeness: FrameCompleteness::Incremental,
                ..
            }
        ));
    }
    #[test]
    fn clearcodec_failure_drops_prior_queued_pixels_and_disables_stream() {
        for kind in 0..3 {
            let mut adapter = clear_adapter(2, 2);
            clear_surface(&mut adapter, 1, 2, 2, 0, 0);
            let rect = ExclusiveRectangle {
                left: 0,
                top: 0,
                right: 2,
                bottom: 2,
            };
            process_gfx_pdu(
                &mut adapter,
                clear_wire(1, 0, rect.clone(), [1, 2, 3], false),
            );
            let mut wire = clear_wire(1, if kind == 0 { 0 } else { 1 }, rect, [7, 8, 9], false);
            if let GfxPdu::WireToSurface1(ref mut p) = wire {
                if kind == 1 {
                    p.bitmap_data.push(99);
                } else if kind == 2 {
                    p.destination_rectangle.right = 3;
                }
            }
            process_gfx_pdu(&mut adapter, wire);
            assert!(adapter.is_failed());
            assert!(adapter.drain_surface_updates().is_empty());
        }
    }

    #[test]
    fn clearcodec_cache_reset_control_advances_sequence_without_pixels_or_mapping() {
        let mut adapter = clear_adapter(2, 2);
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CreateSurface(CreateSurfacePdu {
                surface_id: 1,
                width: 2,
                height: 2,
                pixel_format: EgfxPixelFormat::XRgb,
            }),
        );
        adapter.drain_surface_updates();
        for (seq, right) in [(0, 0), (1, 2)] {
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::WireToSurface1(WireToSurface1Pdu {
                    surface_id: 1,
                    codec_id: Codec1Type::ClearCodec,
                    pixel_format: EgfxPixelFormat::XRgb,
                    destination_rectangle: ExclusiveRectangle {
                        left: 0,
                        top: 0,
                        right,
                        bottom: right,
                    },
                    bitmap_data: vec![4, seq],
                }),
            );
            assert!(!adapter.is_failed());
            assert!(adapter.drain_surface_updates().is_empty());
        }
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: 1,
                output_origin_x: 0,
                output_origin_y: 0,
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            clear_wire(
                1,
                2,
                ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
                [1, 2, 3],
                false,
            ),
        );
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }));
        assert!(!adapter.is_failed());
        assert_eq!(adapter.drain_surface_updates().len(), 2);
    }
    #[test]
    fn clearcodec_frame_publication_overflow_disables_instead_of_losing_reference() {
        let mut adapter = clear_adapter(2, 2);
        clear_surface(&mut adapter, 1, 2, 2, 0, 0);
        process_gfx_pdu(
            &mut adapter,
            clear_wire(
                1,
                0,
                ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
                [1, 2, 3],
                false,
            ),
        );
        let publisher = adapter.surface_publisher.as_ref().unwrap();
        {
            let mut state = super::lock_state(&publisher.state);
            let session_id = state.session_id;
            while state.updates.len() < super::MAX_PENDING_SURFACE_UPDATES {
                state.updates.push_back(SurfaceUpdate::Reset {
                    session_id,
                    generation: 2,
                    size: PixelSize::new(2, 2).unwrap(),
                    format: PixelFormat::Bgrx8UnormSrgb,
                });
            }
        }
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }));
        assert!(adapter.is_failed());
        assert!(adapter.drain_surface_updates().is_empty());
    }

    #[test]
    fn clearcodec_layout_failure_diagnostics_are_precise_without_payload() {
        for (kind, expected) in [
            (0, super::RdpEgfxFailure::PublisherMissingSurface),
            (2, super::RdpEgfxFailure::PublisherInvalidRectangle),
            (3, super::RdpEgfxFailure::PublisherMissingOutput),
            (5, super::RdpEgfxFailure::PublisherQueueLimit),
            (6, super::RdpEgfxFailure::PublisherRevisionOverflow),
        ] {
            let mut publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
            publisher.on_reset_graphics(2, 2);
            if kind != 0 {
                super::lock_state(&publisher.state).surfaces.insert(
                    1,
                    super::EgfxSurface {
                        width: 2,
                        height: 2,
                        origin_x: 0,
                        origin_y: 0,
                        mapped: false,
                    },
                );
                if kind != 1 {
                    publisher.on_surface_mapped(1, if kind == 4 { 1 } else { 0 }, 0);
                }
            }
            {
                let mut state = super::lock_state(&publisher.state);
                if kind == 3 {
                    state.output_size = None;
                }
                if kind == 5 {
                    state.pending_overflowed = true;
                }
                if kind == 6 {
                    state.revision = u64::MAX;
                }
            }
            let pdu = clear_wire(
                1,
                0,
                ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: if kind == 2 { 3 } else { 2 },
                    bottom: 2,
                },
                [1, 2, 3],
                false,
            );
            publisher.on_unhandled_pdu(&pdu);
            assert_eq!(
                super::lock_state(&publisher.state).failure_reason,
                Some(expected),
                "kind={kind}"
            );
            assert!(publisher.is_disabled());
            assert!(publisher.drain().is_empty());
        }
    }

    #[test]
    fn clearcodec_offscreen_decode_then_map_publishes_original_pixels() {
        let mut adapter = clear_adapter(2, 2);
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CreateSurface(CreateSurfacePdu {
                surface_id: 1,
                width: 2,
                height: 2,
                pixel_format: EgfxPixelFormat::XRgb,
            }),
        );
        adapter.drain_surface_updates();
        process_gfx_pdu(
            &mut adapter,
            clear_wire(
                1,
                0,
                ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 2,
                    bottom: 2,
                },
                [3, 19, 211],
                true,
            ),
        );
        assert!(!adapter.is_failed(), "offscreen write is legal");
        assert!(adapter.drain_surface_updates().is_empty());
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: 1,
                output_origin_x: 0,
                output_origin_y: 0,
            }),
        );
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }));
        let updates = adapter.drain_surface_updates();
        assert!(
            matches!(&updates[0],SurfaceUpdate::Damage{patches,..} if patches[0].pixels.as_bytes()==[3,19,211,255].repeat(4))
        );
    }

    #[test]
    fn offscreen_multiple_surfaces_remap_latest_backing_without_sequence_reset() {
        let mut adapter = clear_adapter(4, 2);
        let rect = ExclusiveRectangle {
            left: 0,
            top: 0,
            right: 2,
            bottom: 2,
        };
        for id in [1, 2] {
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: id,
                    width: 2,
                    height: 2,
                    pixel_format: EgfxPixelFormat::XRgb,
                }),
            );
        }
        process_gfx_pdu(
            &mut adapter,
            clear_wire(1, 0, rect.clone(), [1, 2, 3], false),
        );
        process_gfx_pdu(
            &mut adapter,
            clear_wire(2, 1, rect.clone(), [4, 5, 6], false),
        );
        for (id, x) in [(1, 0), (2, 2)] {
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: id,
                    output_origin_x: x,
                    output_origin_y: 0,
                }),
            );
        }
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }));
        adapter.drain_surface_updates();
        process_gfx_pdu(&mut adapter, clear_wire(1, 2, rect, [7, 8, 9], false));
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 2 }));
        adapter.drain_surface_updates();
        for (id, x) in [(1, 2), (2, 0)] {
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                    surface_id: id,
                    output_origin_x: x,
                    output_origin_y: 0,
                }),
            );
        }
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 3 }));
        assert!(!adapter.is_failed());
        let updates = adapter.drain_surface_updates();
        assert!(
            matches!(&updates[0],SurfaceUpdate::Damage{patches,..} if patches.len()==2&&patches[0].rect.x==2&&patches[0].pixels.as_bytes()==[7,8,9,255].repeat(4)&&patches[1].rect.x==0&&patches[1].pixels.as_bytes()==[4,5,6,255].repeat(4))
        );
    }
    #[test]
    fn gfx_surface_copy_cache_fill_and_evict_update_backing_before_map() {
        use ironrdp_egfx::pdu::{
            CacheToSurfacePdu, Color, EvictCacheEntryPdu, Point, SolidFillPdu, SurfaceToCachePdu,
            SurfaceToSurfacePdu,
        };
        let mut adapter = clear_adapter(2, 2);
        let rect = ExclusiveRectangle {
            left: 0,
            top: 0,
            right: 2,
            bottom: 2,
        };
        for id in [1, 2] {
            process_gfx_pdu(
                &mut adapter,
                GfxPdu::CreateSurface(CreateSurfacePdu {
                    surface_id: id,
                    width: 2,
                    height: 2,
                    pixel_format: EgfxPixelFormat::XRgb,
                }),
            );
        }
        process_gfx_pdu(
            &mut adapter,
            clear_wire(1, 0, rect.clone(), [9, 8, 7], false),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::SurfaceToSurface(SurfaceToSurfacePdu {
                source_surface_id: 1,
                destination_surface_id: 2,
                source_rectangle: rect.clone(),
                destination_points: vec![Point { x: 0, y: 0 }],
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::SurfaceToCache(SurfaceToCachePdu {
                surface_id: 2,
                cache_key: 55,
                cache_slot: 3,
                source_rectangle: rect.clone(),
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::SolidFill(SolidFillPdu {
                surface_id: 1,
                fill_pixel: Color {
                    b: 1,
                    g: 2,
                    r: 3,
                    xa: 0,
                },
                rectangles: vec![rect],
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::CacheToSurface(CacheToSurfacePdu {
                cache_slot: 3,
                surface_id: 1,
                destination_points: vec![Point { x: 0, y: 0 }],
            }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::EvictCacheEntry(EvictCacheEntryPdu { cache_slot: 3 }),
        );
        process_gfx_pdu(
            &mut adapter,
            GfxPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: 1,
                output_origin_x: 0,
                output_origin_y: 0,
            }),
        );
        process_gfx_pdu(&mut adapter, GfxPdu::EndFrame(EndFramePdu { frame_id: 1 }));
        assert!(!adapter.is_failed());
        let updates = adapter.drain_surface_updates();
        assert!(
            matches!(&updates[1],SurfaceUpdate::Damage{patches,..} if patches[0].pixels.as_bytes()==[9,8,7,255].repeat(4))
        );
    }
    #[test]
    fn publication_rejects_aggregate_bytes_before_backing_reads() {
        let publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
        let mut state = super::lock_state(&publisher.state);
        state.output_size = PixelSize::new(4096, 4096);
        state.surfaces.insert(
            1,
            super::EgfxSurface {
                width: 4096,
                height: 4096,
                origin_x: 0,
                origin_y: 0,
                mapped: true,
            },
        );
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: 4096,
            height: 4096,
        };
        assert!(EgfxSurfacePublisher::publication_rectangles(&state, 1, &[rect, rect]).is_err());
        assert!(!state.backings.contains(1));
    }

    #[test]
    fn pending_and_queued_damage_bytes_share_publication_budget() {
        fn patch() -> frd_frame::PixelPatch {
            frd_frame::PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                stride_bytes: 4,
                pixels: frd_frame::PixelBuffer::from_boxed_slice(
                    vec![1, 2, 3, 255].into_boxed_slice(),
                ),
            }
        }
        let publisher = EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap();
        let mut state = super::lock_state(&publisher.state);
        state.pending_patches.push(patch());
        let session_id = state.session_id;
        state.updates.push_back(SurfaceUpdate::Damage {
            session_id,
            generation: 1,
            revision: 1,
            patches: vec![patch()],
        });
        assert!(EgfxSurfacePublisher::check_publication_bytes(&state, 4, 8).is_err());
        assert!(EgfxSurfacePublisher::check_publication_bytes(&state, 4, 12).is_ok());
    }

    fn process_gfx_pdu(adapter: &mut EgfxAdapter, pdu: GfxPdu) {
        let payload = wrap_uncompressed(&encode_vec(&pdu).expect("encode EGFX PDU"));
        adapter
            .process(7, &payload)
            .expect("EGFX PDU must be accepted");
    }

    fn black_yuv420_frame(stream: &frd_media_api::VideoStreamConfig) -> DecodedVideoFrame {
        let input = stream.as_input();
        DecodedVideoFrame::try_new(DecodedVideoFrameInput {
            identity: input.identity,
            generation: input.generation,
            timestamp: frd_media_api::VideoTimestamp {
                ticks: 0,
                timescale: input.time_base.ticks_per_second(),
            },
            coded_size: input.coded_size,
            visible_rect: input.visible_rect,
            format: VideoPixelFormat::Yuv420P8,
            colorimetry: input.colorimetry,
            range: input.range,
            planes: vec![
                VideoPlane::try_new(2, 2, 2, vec![16, 16, 16, 16].into_boxed_slice()).unwrap(),
                VideoPlane::try_new(1, 1, 1, vec![128].into_boxed_slice()).unwrap(),
                VideoPlane::try_new(1, 1, 1, vec![128].into_boxed_slice()).unwrap(),
            ]
            .into_boxed_slice(),
        })
        .unwrap()
    }

    struct OneFrameDecoder {
        frame: DecodedVideoFrame,
        resets: Vec<u64>,
        fail_reset: bool,
    }

    struct FailingH264Decoder;

    impl H264Decoder for FailingH264Decoder {
        fn decode(&mut self, _data: &[u8]) -> DecoderResult<DecodedFrame> {
            Err(DecoderError::msg("test EGFX decoder failure"))
        }

        fn reset(&mut self) {}
    }

    impl VideoDecoder for OneFrameDecoder {
        fn submit(
            &mut self,
            _access_unit: EncodedVideoAccessUnit,
        ) -> Result<DecodeOutcome, VideoDecodeError> {
            Ok(DecodeOutcome::Frames(
                vec![self.frame.clone()].into_boxed_slice(),
            ))
        }

        fn flush(&mut self) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError> {
            Ok(vec![self.frame.clone()].into_boxed_slice())
        }

        fn reset(&mut self, generation: u64) -> Result<(), VideoDecodeError> {
            if self.fail_reset {
                return Err(VideoDecodeError::new(
                    frd_media_api::VideoDecodeErrorCode::DecodeFailedAfterFirstFrame,
                ));
            }
            self.resets.push(generation);
            let mut input = self.frame.as_input().clone();
            input.generation = generation;
            self.frame = DecodedVideoFrame::try_new(input).map_err(|_| {
                VideoDecodeError::new(
                    frd_media_api::VideoDecodeErrorCode::DecodedFrameLayoutInvalid,
                )
            })?;
            Ok(())
        }
    }

    struct ExactFactory;

    impl VideoCapabilityProvider for ExactFactory {
        fn backend_id(&self) -> VideoBackendId {
            VideoBackendId::new("test-avc420")
        }

        fn backend_kind(&self) -> VideoBackendKind {
            VideoBackendKind::Ffmpeg
        }

        fn availability(&self) -> VideoBackendAvailability {
            VideoBackendAvailability::DecoderReady
        }

        fn query(&self, query: &VideoDecodeQuery) -> VideoDecodeSupport {
            VideoDecodeSupport::SoftwareExact(VideoDecodeCapability {
                backend_id: self.backend_id(),
                codec: query.codec,
                profile: query.profile,
                chroma: query.chroma,
                bit_depth: query.bit_depth,
                max_coded_size: query.coded_size,
                output_formats: query.preferred_outputs.clone(),
                requires_bitstream_conversion: true,
            })
        }
    }

    impl VideoDecoderFactory for ExactFactory {
        fn create(
            &self,
            config: &frd_media_api::VideoStreamConfig,
        ) -> Result<Box<dyn VideoDecoder>, VideoDecodeError> {
            Ok(Box::new(OneFrameDecoder {
                frame: black_yuv420_frame(config),
                resets: Vec::new(),
                fail_reset: false,
            }))
        }
    }

    struct SolidAvc444Decoder;

    impl Avc444Decoder for SolidAvc444Decoder {
        fn reset(&mut self, _width: u32, _height: u32) -> DecoderResult<()> {
            Ok(())
        }

        fn decode(
            &mut self,
            _codec_id: Codec1Type,
            _surface_id: u16,
            bitmap: &super::ValidatedAvc444Bitmap<'_>,
            destination: &ExclusiveRectangle,
        ) -> DecoderResult<DecodedFrame> {
            assert_eq!(bitmap.encoding, super::ValidatedAvc444Encoding::Luma);
            let width = u32::from(destination.right - destination.left);
            let height = u32::from(destination.bottom - destination.top);
            Ok(DecodedFrame::new(
                vec![0x10, 0x20, 0x30, 0xff]
                    .into_iter()
                    .cycle()
                    .take(usize::try_from(width * height * 4).expect("test frame fits in usize"))
                    .collect(),
                width,
                height,
            ))
        }
    }

    struct TestHandler;

    impl GraphicsPipelineHandler for TestHandler {}
}
