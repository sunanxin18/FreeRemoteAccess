//! 合成 codec 合同与独立像素参考；仅显式验证模式使用。
use frd_core::{PixelRect, PixelSize, SessionId};
use frd_media_api::*;
use sha2::{Digest, Sha256};
pub(super) struct Case {
    pub(super) id: &'static str,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) visible_width: u32,
    pub(super) visible_height: u32,
    pub(super) hevc: bool,
    pub(super) y420: bool,
    pub(super) units: &'static [&'static [u8]],
    pub(super) hashes: &'static [&'static str],
    pub(super) references: &'static [&'static [u8]],
}
include!("codec_fixture_data.rs");

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Contract,
    Decode(VideoDecodeErrorCode),
    BeforeEos,
    Geometry,
    Pixels,
    #[cfg(all(
        target_env = "ohos",
        debug_assertions,
        feature = "normal-hap-codec-fixture"
    ))]
    StaleAccepted,
    #[cfg(all(
        target_env = "ohos",
        debug_assertions,
        feature = "normal-hap-codec-fixture"
    ))]
    MalformedAccepted,
    Deadline,
    Cancelled,
}
pub(super) type Result<T> = std::result::Result<T, Failure>;
impl From<VideoDecodeError> for Failure {
    fn from(error: VideoDecodeError) -> Self {
        Self::Decode(error.code())
    }
}
// 仅为固定 AnnexB 合成输入提取配置/生成长度前缀输入；实际转换由生产 loader 执行。
pub(super) fn nals(bytes: &[u8]) -> Result<Vec<&[u8]>> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let prefix = if bytes[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        starts.push((i, i + prefix));
        i += prefix;
    }
    if starts.first().map(|s| s.0) != Some(0) {
        return Err(Failure::Contract);
    }
    let mut result = Vec::new();
    for (index, (_, payload)) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(bytes.len(), |s| s.0);
        if *payload >= end {
            return Err(Failure::Contract);
        }
        result.push(&bytes[*payload..end]);
    }
    Ok(result)
}
pub(super) fn config(case: &Case, length: bool) -> Result<VideoStreamConfig> {
    let ns = nals(case.units[0])?;
    let parameter = |kind| {
        ns.iter()
            .find(|n| {
                if case.hevc {
                    (n[0] >> 1) & 0x3f == kind
                } else {
                    n[0] & 31 == kind
                }
            })
            .map(|n| n.to_vec().into_boxed_slice())
            .ok_or(Failure::Contract)
    };
    VideoStreamConfig::try_new(VideoStreamConfigInput {
        identity: VideoStreamIdentity {
            session_id: SessionId::allocate(),
            stream_id: 1,
        },
        generation: 1,
        codec: if case.hevc {
            VideoCodec::Hevc
        } else {
            VideoCodec::H264
        },
        profile: if case.hevc {
            VideoProfile::HevcMain4448
        } else if case.y420 {
            VideoProfile::H264Avc420
        } else {
            VideoProfile::H264Avc444
        },
        chroma: if case.y420 {
            ChromaFormat::Yuv420
        } else {
            ChromaFormat::Yuv444
        },
        bit_depth: 8,
        coded_size: PixelSize::new(case.width, case.height).ok_or(Failure::Contract)?,
        visible_rect: PixelRect {
            x: 0,
            y: 0,
            width: case.visible_width,
            height: case.visible_height,
        },
        time_base: VideoTimeBase::try_new(90000).map_err(|_| Failure::Contract)?,
        bitstream_format: if length {
            VideoBitstreamFormat::AvcLengthPrefixed
        } else {
            VideoBitstreamFormat::AnnexB
        },
        colorimetry: VideoColorimetry::Bt709,
        range: VideoRange::Limited,
        chroma_location: ChromaLocation::Left,
        parameter_sets: VideoParameterSets::try_new(
            if case.hevc {
                Some(parameter(32)?)
            } else {
                None
            },
            parameter(if case.hevc { 33 } else { 7 })?,
            parameter(if case.hevc { 34 } else { 8 })?,
        )
        .map_err(|_| Failure::Contract)?,
    })
    .map_err(|_| Failure::Contract)
}
pub(super) fn access(
    config: &VideoStreamConfig,
    bytes: &[u8],
    generation: u64,
    index: usize,
) -> Result<EncodedVideoAccessUnit> {
    let bytes = if config.as_input().bitstream_format == VideoBitstreamFormat::AvcLengthPrefixed {
        let mut output = Vec::new();
        for nal in nals(bytes)? {
            output.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            output.extend_from_slice(nal);
        }
        output
    } else {
        bytes.to_vec()
    };
    raw_access(config, bytes, generation, index)
}
pub(super) fn raw_access(
    config: &VideoStreamConfig,
    bytes: Vec<u8>,
    generation: u64,
    index: usize,
) -> Result<EncodedVideoAccessUnit> {
    EncodedVideoAccessUnit::try_new(
        config.as_input().identity,
        generation,
        VideoTimestamp {
            ticks: index as u64 * 3000,
            timescale: config.as_input().time_base.ticks_per_second(),
        },
        index == 0,
        bytes.into_boxed_slice(),
    )
    .map_err(|_| Failure::Contract)
}
pub(super) fn one_frame(outcome: DecodeOutcome) -> Result<DecodedVideoFrame> {
    match outcome {
        DecodeOutcome::Frames(frames) if frames.len() == 1 => Ok(frames.into_vec().remove(0)),
        _ => Err(Failure::BeforeEos),
    }
}
pub(super) fn visible_rows(frame: &DecodedVideoFrame) -> Result<Vec<&[u8]>> {
    let f = frame.as_input();
    let mut rows = Vec::new();
    for (i, plane) in f.planes.iter().enumerate() {
        let divisor = if i > 0 && f.format == VideoPixelFormat::Yuv420P8 {
            2
        } else {
            1
        };
        let x = f.visible_rect.x / divisor;
        let y = f.visible_rect.y / divisor;
        let width = f.visible_rect.width.div_ceil(divisor);
        let height = f.visible_rect.height.div_ceil(divisor);
        for row in y..y + height {
            let start = (row as usize)
                .checked_mul(plane.stride_bytes() as usize)
                .and_then(|s| s.checked_add(x as usize))
                .ok_or(Failure::Geometry)?;
            rows.push(
                plane
                    .bytes()
                    .get(start..start + width as usize)
                    .ok_or(Failure::Geometry)?,
            );
        }
    }
    Ok(rows)
}
pub(super) fn verify_metadata(
    case: &Case,
    config: &VideoStreamConfig,
    frame: &DecodedVideoFrame,
    index: usize,
    generation: u64,
) -> Result<()> {
    let f = frame.as_input();
    if f.identity != config.as_input().identity
        || f.generation != generation
        || f.timestamp.ticks != index as u64 * 3000
        || f.timestamp.timescale != config.as_input().time_base.ticks_per_second()
        || f.coded_size != config.as_input().coded_size
        || f.visible_rect != config.as_input().visible_rect
        || f.format
            != if case.y420 {
                VideoPixelFormat::Yuv420P8
            } else {
                VideoPixelFormat::Yuv444P8
            }
    {
        return Err(Failure::Geometry);
    }
    Ok(())
}
pub(super) fn verify_frame(
    case: &Case,
    config: &VideoStreamConfig,
    frame: &DecodedVideoFrame,
    index: usize,
    generation: u64,
) -> Result<()> {
    verify_metadata(case, config, frame, index, generation)?;
    let rows = visible_rows(frame)?;
    let mut hash = Sha256::new();
    let mut offset = 0;
    for row in rows {
        hash.update(row);
        if let Some(reference) = case.references.get(index) {
            if reference.get(offset..offset + row.len()) != Some(row) {
                return Err(Failure::Pixels);
            }
        }
        offset += row.len();
    }
    if case
        .references
        .get(index)
        .is_some_and(|r| offset != r.len())
        || format!("{:x}", hash.finalize()) != case.hashes[index]
    {
        return Err(Failure::Pixels);
    }
    Ok(())
}
