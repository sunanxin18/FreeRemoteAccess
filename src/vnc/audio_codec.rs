//! Apple Remote Desktop UDP audio codec contract.
//!
//! AVConference 运行时证据将 codec type 16 / RTP payload type 101 映射为
//! 48 kHz、每 AU 480 个样本且无 SBR 的 raw AAC-ELD。Mac→PC HPSS 流为双声道，
//! mode-4 `RemoteMicrophone` profile 为单声道。mode-4 有界实验中的认证 SRTCP
//! 只证明通用 AVConference 端点接收/报告，不证明 password-HPSS 产品路径或播放。
//! stock `ScreenSharing.framework` 已恢复的 `audioChatSupported` 明确要求 IDS 会话或
//! Apple-ID 邀请地址；该身份路径不属于产品范围。因此 HPSS viewer 拒绝所有
//! PC→Mac Audio Chat，Windows 麦克风保持禁用；下面的 mode-4 编码常量只用于离线
//! 协议回归。

use anyhow::{anyhow, ensure, Context, Result};
use fdk_aac::dec::{Decoder, Transport};
use fdk_aac_sys as fdk;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr;

use crate::vnc::srtp::parse_rtp_packet;

pub const ARD_AUDIO_RTP_PAYLOAD_TYPE: u8 = 101;
pub const ARD_AUDIO_SAMPLE_RATE_HZ: u32 = 48_000;
pub const ARD_AUDIO_CHANNEL_COUNT: usize = 2;
pub const ARD_AUDIO_PC_TO_MAC_CHANNEL_COUNT: usize = 1;
pub const ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT: usize = 480;
pub const ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT: usize =
    ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT * ARD_AUDIO_CHANNEL_COUNT;
pub const ARD_AUDIO_PC_TO_MAC_PCM_SAMPLES_PER_ACCESS_UNIT: usize =
    ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT * ARD_AUDIO_PC_TO_MAC_CHANNEL_COUNT;
#[cfg(test)]
const ARD_AUDIO_MAC_TO_PC_BIT_RATE: u32 = 320_000;
pub const ARD_AUDIO_PC_TO_MAC_BIT_RATE: u32 = 80_000;
const AAC_ELD_DECODER_OUTPUT_CAPACITY_SAMPLES: usize = 8_192;
const AAC_ENCODER_AUTOMATIC_MODULE_SELECTION: u32 = 0;
const AAC_ENCODER_CONSTANT_BIT_RATE_MODE: u32 = 0;
const AAC_ELD_SBR_DISABLED: u32 = 0;
const AAC_ENCODER_NO_ANCILLARY_BYTES: i32 = 0;
const AAC_ENCODER_BUFFER_COUNT: i32 = 1;
const MAX_CONCEALED_AUDIO_GAP_ACCESS_UNITS: usize = 100;
#[cfg(test)]
const RFC3640_SINGLE_AU_HEADERS_LENGTH_BITS: u16 = 16;
#[cfg(test)]
const RFC3640_AU_INDEX_BITS: u32 = 3;
#[cfg(test)]
const RFC3640_AU_SIZE_BITS: u32 = 13;
#[cfg(test)]
const RFC3640_MAX_ACCESS_UNIT_BYTES: usize = (1usize << RFC3640_AU_SIZE_BITS) - 1;
#[cfg(test)]
const RFC3640_SINGLE_AU_HEADER_BYTES: usize = 4;

/// DecoderSpecificInfo from the AVConference `VCAudioPayload` magic cookie.
pub const ARD_AAC_ELD_AUDIO_SPECIFIC_CONFIG: [u8; 4] = [0xf8, 0xe6, 0x50, 0x00];
pub const ARD_AAC_ELD_PC_TO_MAC_AUDIO_SPECIFIC_CONFIG: [u8; 4] = [0xf8, 0xe6, 0x30, 0x00];

/// AAC-ELD DTX access unit observed from the authenticated ARD audio stream.
#[cfg(test)]
const ARD_AAC_ELD_DTX_ACCESS_UNIT: [u8; 4] = [0x00, 0x68, 0x34, 0x00];

/// 将一个 AAC-ELD AU 封装为 AVConference bundling scheme 2 的 RFC 3640 payload。
///
/// 这只保留为非 mode-4 的离线测试 helper。`RemoteMicrophone` mode 4 对应 stream
/// mode 7 / bundling scheme 3，其 RTP payload 是 raw AU，生产发送路径不得调用本函数。
/// 单 AU 形态为
/// 16-bit AU-headers-length，随后是 13-bit AU-size 和值为零的 3-bit AU-index。
#[cfg(test)]
pub(crate) fn bundle_rfc3640_single_access_unit(access_unit: &[u8]) -> Result<Vec<u8>> {
    ensure!(!access_unit.is_empty(), "RFC 3640 AAC access unit 不能为空");
    ensure!(
        access_unit.len() <= RFC3640_MAX_ACCESS_UNIT_BYTES,
        "RFC 3640 AAC access unit 超过 13-bit AU-size: {}",
        access_unit.len()
    );
    let access_unit_size =
        u16::try_from(access_unit.len()).context("RFC 3640 AAC access unit 长度超过 u16")?;
    let size_and_index = access_unit_size << RFC3640_AU_INDEX_BITS;
    let mut payload = Vec::with_capacity(RFC3640_SINGLE_AU_HEADER_BYTES + access_unit.len());
    payload.extend_from_slice(&RFC3640_SINGLE_AU_HEADERS_LENGTH_BITS.to_be_bytes());
    payload.extend_from_slice(&size_and_index.to_be_bytes());
    payload.extend_from_slice(access_unit);
    Ok(payload)
}

pub struct AacEldDecoder {
    decoder: Decoder,
    expected_output_channels: usize,
    expected_output_sample_count: usize,
}

pub struct AacEldEncoder {
    handle: AacEldEncoderHandle,
    #[cfg(test)]
    audio_specific_config: Vec<u8>,
    max_output_bytes: usize,
    input_sample_count: usize,
}

struct AacEldEncoderHandle {
    raw: fdk::HANDLE_AACENCODER,
}

impl Drop for AacEldEncoderHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = fdk::aacEncClose(&mut self.raw);
        }
    }
}

fn check_encoder_status(status: fdk::AACENC_ERROR, operation: &str) -> Result<()> {
    ensure!(
        status == fdk::AACENC_ERROR_AACENC_OK,
        "{operation} 失败: FDK AAC 错误码 {status}"
    );
    Ok(())
}

impl AacEldEncoder {
    #[cfg(test)]
    pub fn new() -> Result<Self> {
        Self::new_with_contract(
            ARD_AUDIO_CHANNEL_COUNT,
            fdk::CHANNEL_MODE_MODE_2 as u32,
            ARD_AUDIO_MAC_TO_PC_BIT_RATE,
            &ARD_AAC_ELD_AUDIO_SPECIFIC_CONFIG,
        )
    }

    pub fn new_for_pc_to_mac() -> Result<Self> {
        Self::new_with_contract(
            ARD_AUDIO_PC_TO_MAC_CHANNEL_COUNT,
            fdk::CHANNEL_MODE_MODE_1 as u32,
            ARD_AUDIO_PC_TO_MAC_BIT_RATE,
            &ARD_AAC_ELD_PC_TO_MAC_AUDIO_SPECIFIC_CONFIG,
        )
    }

    fn new_with_contract(
        channel_count: usize,
        channel_mode: u32,
        target_bit_rate: u32,
        expected_audio_specific_config: &[u8],
    ) -> Result<Self> {
        let mut raw = ptr::null_mut();
        check_encoder_status(
            unsafe {
                fdk::aacEncOpen(
                    &mut raw,
                    AAC_ENCODER_AUTOMATIC_MODULE_SELECTION,
                    channel_count as u32,
                )
            },
            "创建 AAC-ELD 编码器",
        )?;
        let handle = AacEldEncoderHandle { raw };
        let parameters = [
            (
                fdk::AACENC_PARAM_AACENC_AOT,
                fdk::AUDIO_OBJECT_TYPE_AOT_ER_AAC_ELD as u32,
                "设置 AAC-ELD object type",
            ),
            (
                fdk::AACENC_PARAM_AACENC_BITRATE,
                target_bit_rate,
                "设置 AAC-ELD 比特率",
            ),
            (
                fdk::AACENC_PARAM_AACENC_BITRATEMODE,
                AAC_ENCODER_CONSTANT_BIT_RATE_MODE,
                "设置 AAC-ELD CBR 模式",
            ),
            (
                fdk::AACENC_PARAM_AACENC_SAMPLERATE,
                ARD_AUDIO_SAMPLE_RATE_HZ,
                "设置 AAC-ELD 采样率",
            ),
            (
                fdk::AACENC_PARAM_AACENC_SBR_MODE,
                AAC_ELD_SBR_DISABLED,
                "关闭 AAC-ELD SBR",
            ),
            (
                fdk::AACENC_PARAM_AACENC_GRANULE_LENGTH,
                ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32,
                "设置 AAC-ELD 480 样本帧长",
            ),
            (
                fdk::AACENC_PARAM_AACENC_CHANNELMODE,
                channel_mode,
                "设置 AAC-ELD 声道模式",
            ),
            (
                fdk::AACENC_PARAM_AACENC_TRANSMUX,
                fdk::TRANSPORT_TYPE_TT_MP4_RAW as u32,
                "设置 AAC-ELD raw transport",
            ),
        ];
        for (parameter, value, operation) in parameters {
            check_encoder_status(
                unsafe { fdk::aacEncoder_SetParam(handle.raw, parameter, value) },
                operation,
            )?;
        }
        check_encoder_status(
            unsafe {
                fdk::aacEncEncode(
                    handle.raw,
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null_mut(),
                )
            },
            "初始化 AAC-ELD 编码器",
        )?;
        let mut info = unsafe { zeroed::<fdk::AACENC_InfoStruct>() };
        check_encoder_status(
            unsafe { fdk::aacEncInfo(handle.raw, &mut info) },
            "读取 AAC-ELD 编码器配置",
        )?;
        ensure!(
            info.inputChannels as usize == channel_count,
            "AAC-ELD 编码器输入声道数不符合 ARD 协议: {}",
            info.inputChannels
        );
        ensure!(
            info.frameLength as usize == ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT,
            "AAC-ELD 编码器帧长不符合 ARD 协议: {}",
            info.frameLength
        );
        let configuration_length = info.confSize as usize;
        ensure!(
            configuration_length <= info.confBuf.len(),
            "AAC-ELD AudioSpecificConfig 长度越界: {configuration_length}"
        );
        let audio_specific_config = info.confBuf[..configuration_length].to_vec();
        ensure!(
            audio_specific_config == expected_audio_specific_config,
            "AAC-ELD AudioSpecificConfig 与 ARD 协议不一致"
        );
        ensure!(info.maxOutBufBytes != 0, "AAC-ELD 编码器报告空输出缓冲区");
        Ok(Self {
            handle,
            #[cfg(test)]
            audio_specific_config,
            max_output_bytes: info.maxOutBufBytes as usize,
            input_sample_count: ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT * channel_count,
        })
    }

    #[cfg(test)]
    pub fn audio_specific_config(&self) -> &[u8] {
        &self.audio_specific_config
    }

    pub fn encode_pcm_frame(&self, pcm: &[i16]) -> Result<Vec<u8>> {
        ensure!(
            pcm.len() == self.input_sample_count,
            "AAC-ELD 编码输入必须恰好为一帧: {} != {}",
            pcm.len(),
            self.input_sample_count
        );
        let mut output = vec![0u8; self.max_output_bytes];
        let mut input_buffer = pcm.as_ptr() as *mut c_void;
        let mut input_identifier = fdk::AACENC_BufferIdentifier_IN_AUDIO_DATA as i32;
        let mut input_size = i32::try_from(std::mem::size_of_val(pcm))
            .map_err(|_| anyhow!("AAC-ELD PCM 帧字节数超出 FDK 范围"))?;
        let mut input_element_size = size_of::<i16>() as i32;
        let input_descriptor = fdk::AACENC_BufDesc {
            numBufs: AAC_ENCODER_BUFFER_COUNT,
            bufs: &mut input_buffer,
            bufferIdentifiers: &mut input_identifier,
            bufSizes: &mut input_size,
            bufElSizes: &mut input_element_size,
        };
        let mut output_buffer = output.as_mut_ptr() as *mut c_void;
        let mut output_identifier = fdk::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as i32;
        let mut output_size =
            i32::try_from(output.len()).map_err(|_| anyhow!("AAC-ELD 输出缓冲区超出 FDK 范围"))?;
        let mut output_element_size = size_of::<u8>() as i32;
        let output_descriptor = fdk::AACENC_BufDesc {
            numBufs: AAC_ENCODER_BUFFER_COUNT,
            bufs: &mut output_buffer,
            bufferIdentifiers: &mut output_identifier,
            bufSizes: &mut output_size,
            bufElSizes: &mut output_element_size,
        };
        let input_arguments = fdk::AACENC_InArgs {
            numInSamples: pcm.len() as i32,
            numAncBytes: AAC_ENCODER_NO_ANCILLARY_BYTES,
        };
        let mut output_arguments = unsafe { zeroed::<fdk::AACENC_OutArgs>() };
        check_encoder_status(
            unsafe {
                fdk::aacEncEncode(
                    self.handle.raw,
                    &input_descriptor,
                    &output_descriptor,
                    &input_arguments,
                    &mut output_arguments,
                )
            },
            "编码 AAC-ELD PCM 帧",
        )?;
        ensure!(
            output_arguments.numInSamples as usize == pcm.len(),
            "AAC-ELD 编码器未完整消费 PCM 帧: {}/{}",
            output_arguments.numInSamples,
            pcm.len()
        );
        ensure!(
            output_arguments.numOutBytes > 0,
            "AAC-ELD 编码器没有产生 access unit"
        );
        let encoded_size = output_arguments.numOutBytes as usize;
        ensure!(
            encoded_size <= output.len(),
            "AAC-ELD 编码器输出越界: {} > {}",
            encoded_size,
            output.len()
        );
        output.truncate(encoded_size);
        Ok(output)
    }
}

impl AacEldDecoder {
    pub fn new() -> Result<Self> {
        Self::new_with_contract(&ARD_AAC_ELD_AUDIO_SPECIFIC_CONFIG, ARD_AUDIO_CHANNEL_COUNT)
    }

    pub fn new_for_pc_to_mac() -> Result<Self> {
        Self::new_with_contract(
            &ARD_AAC_ELD_PC_TO_MAC_AUDIO_SPECIFIC_CONFIG,
            ARD_AUDIO_PC_TO_MAC_CHANNEL_COUNT,
        )
    }

    fn new_with_contract(audio_specific_config: &[u8], output_channels: usize) -> Result<Self> {
        let mut decoder = Decoder::new(Transport::Raw);
        decoder
            .config_raw(audio_specific_config)
            .map_err(|error| anyhow!("配置 ARD AAC-ELD 解码器失败: {error}"))?;
        decoder
            .set_min_output_channels(output_channels)
            .map_err(|error| anyhow!("设置 AAC-ELD 最小输出声道数失败: {error}"))?;
        decoder
            .set_max_output_channels(output_channels)
            .map_err(|error| anyhow!("设置 AAC-ELD 最大输出声道数失败: {error}"))?;
        Ok(Self {
            decoder,
            expected_output_channels: output_channels,
            expected_output_sample_count: ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT * output_channels,
        })
    }

    pub fn decode_access_unit(&mut self, access_unit: &[u8]) -> Result<Vec<i16>> {
        ensure!(!access_unit.is_empty(), "拒绝空 AAC-ELD access unit");
        let consumed = self
            .decoder
            .fill(access_unit)
            .map_err(|error| anyhow!("提交 AAC-ELD access unit 失败: {error}"))?;
        ensure!(
            consumed == access_unit.len(),
            "AAC-ELD access unit 未完整提交: {consumed}/{}",
            access_unit.len()
        );

        let mut pcm = vec![0i16; AAC_ELD_DECODER_OUTPUT_CAPACITY_SAMPLES];
        self.decoder
            .decode_frame(&mut pcm)
            .map_err(|error| anyhow!("解码 AAC-ELD access unit 失败: {error}"))?;
        let decoded_samples = self.decoder.decoded_frame_size();
        ensure!(
            decoded_samples == self.expected_output_sample_count,
            "AAC-ELD 输出样本数不符合 ARD 协议: {decoded_samples} != {}",
            self.expected_output_sample_count
        );
        pcm.truncate(decoded_samples);
        let stream_info = self.decoder.stream_info();
        ensure!(
            stream_info.sampleRate == ARD_AUDIO_SAMPLE_RATE_HZ as i32,
            "AAC-ELD 输出采样率不符合 ARD 协议: {}",
            stream_info.sampleRate
        );
        ensure!(
            stream_info.numChannels == self.expected_output_channels as i32,
            "AAC-ELD 输出声道数不符合 ARD 协议: {}",
            stream_info.numChannels
        );
        Ok(pcm)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct DecodedAudioPacket {
    pub pcm: Vec<i16>,
    pub concealed_access_units: usize,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

const RTP_SEQUENCE_FORWARD_HALF_SPACE: u16 = 1 << (u16::BITS - 1);

#[derive(Debug, Eq, PartialEq)]
pub enum AudioReceiveOutcome {
    Decoded(DecodedAudioPacket),
    Resynchronized {
        decoded: DecodedAudioPacket,
        skipped_access_units: usize,
    },
    DiscardedLate {
        sequence: u16,
        last_forward_sequence: u16,
    },
}

fn forward_sequence_advance(last: u16, candidate: u16) -> Option<u16> {
    let advance = candidate.wrapping_sub(last);
    (advance != 0 && advance < RTP_SEQUENCE_FORWARD_HALF_SPACE).then_some(advance)
}

pub struct ArdAudioReceiver {
    decoder: AacEldDecoder,
    current_ssrc: Option<u32>,
    last_sequence: Option<u16>,
    last_timestamp: Option<u32>,
}

impl ArdAudioReceiver {
    pub fn new() -> Result<Self> {
        Ok(Self {
            decoder: AacEldDecoder::new()?,
            current_ssrc: None,
            last_sequence: None,
            last_timestamp: None,
        })
    }

    pub fn decode_rtp_packet(&mut self, packet: &[u8]) -> Result<AudioReceiveOutcome> {
        let packet = parse_rtp_packet(packet).context("解析 ARD audio RTP 数据报失败")?;
        ensure!(
            packet.header.payload_type == ARD_AUDIO_RTP_PAYLOAD_TYPE,
            "ARD audio RTP payload type 非法: {}",
            packet.header.payload_type
        );
        ensure!(!packet.payload.is_empty(), "ARD audio RTP payload 为空");

        if self.current_ssrc != Some(packet.header.ssrc) {
            self.decoder = AacEldDecoder::new()?;
            self.current_ssrc = Some(packet.header.ssrc);
            self.last_sequence = None;
            self.last_timestamp = None;
        }

        let concealed_access_units = match (self.last_sequence, self.last_timestamp) {
            (Some(last_sequence), Some(last_timestamp)) => {
                let Some(sequence_advance) =
                    forward_sequence_advance(last_sequence, packet.header.sequence)
                else {
                    return Ok(AudioReceiveOutcome::DiscardedLate {
                        sequence: packet.header.sequence,
                        last_forward_sequence: last_sequence,
                    });
                };
                let sequence_advance = sequence_advance as usize;
                if sequence_advance > MAX_CONCEALED_AUDIO_GAP_ACCESS_UNITS + 1 {
                    let mut decoder = AacEldDecoder::new()
                        .context("为 ARD audio RTP 大间隔重建 AAC-ELD 解码器失败")?;
                    let pcm = decoder.decode_access_unit(packet.payload)?;
                    let decoded = DecodedAudioPacket {
                        pcm,
                        concealed_access_units: 0,
                        sequence: packet.header.sequence,
                        timestamp: packet.header.timestamp,
                        ssrc: packet.header.ssrc,
                    };
                    self.decoder = decoder;
                    self.last_sequence = Some(packet.header.sequence);
                    self.last_timestamp = Some(packet.header.timestamp);
                    return Ok(AudioReceiveOutcome::Resynchronized {
                        decoded,
                        skipped_access_units: sequence_advance - 1,
                    });
                }
                let timestamp_advance = packet.header.timestamp.wrapping_sub(last_timestamp);
                let expected_timestamp_advance = u32::try_from(sequence_advance)
                    .context("ARD audio RTP sequence advance 超过 u32")?
                    .checked_mul(ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32)
                    .context("ARD audio RTP timestamp advance 溢出")?;
                ensure!(
                    timestamp_advance == expected_timestamp_advance,
                    "ARD audio RTP timestamp 跳变不符合帧长: {timestamp_advance} != {expected_timestamp_advance}"
                );
                sequence_advance - 1
            }
            (None, None) => 0,
            _ => unreachable!("sequence 与 timestamp 状态始终成对更新"),
        };

        let decoded = self.decoder.decode_access_unit(packet.payload)?;
        let mut pcm = Vec::with_capacity(
            (concealed_access_units + 1) * ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT,
        );
        pcm.resize(
            concealed_access_units * ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT,
            0,
        );
        pcm.extend_from_slice(&decoded);
        self.last_sequence = Some(packet.header.sequence);
        self.last_timestamp = Some(packet.header.timestamp);
        Ok(AudioReceiveOutcome::Decoded(DecodedAudioPacket {
            pcm,
            concealed_access_units,
            sequence: packet.header.sequence,
            timestamp: packet.header.timestamp,
            ssrc: packet.header.ssrc,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bundle_rfc3640_single_access_unit, AacEldDecoder, AacEldEncoder, ArdAudioReceiver,
        AudioReceiveOutcome, ARD_AAC_ELD_AUDIO_SPECIFIC_CONFIG, ARD_AAC_ELD_DTX_ACCESS_UNIT,
        ARD_AAC_ELD_PC_TO_MAC_AUDIO_SPECIFIC_CONFIG, ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT,
        ARD_AUDIO_PC_TO_MAC_PCM_SAMPLES_PER_ACCESS_UNIT, ARD_AUDIO_RTP_PAYLOAD_TYPE,
        ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT,
    };
    use crate::vnc::audio_input::p5_probe_pcm_frame;

    fn decode_hex_fixture(hex: &str) -> Vec<u8> {
        let hex = hex.trim();
        assert_eq!(hex.len() % 2, 0);
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(pair, 16).unwrap()
            })
            .collect()
    }

    #[test]
    fn apple_aac_eld_dtx_access_unit_decodes_to_one_pcm_frame() {
        let mut decoder = AacEldDecoder::new().unwrap();
        let pcm = decoder
            .decode_access_unit(&ARD_AAC_ELD_DTX_ACCESS_UNIT)
            .unwrap();

        assert_eq!(pcm.len(), ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT);
        assert!(pcm.iter().all(|sample| *sample == 0));
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AAC-ELD fixture"]
    fn authenticated_apple_aac_eld_access_unit_decodes_to_non_silent_pcm() {
        let fixture = crate::vnc::read_private_fixture_text(
            "ard_re/fixtures/ard_aac_eld_active_access_unit.hex",
        );
        let access_unit = decode_hex_fixture(&fixture);
        let mut decoder = AacEldDecoder::new().unwrap();
        let pcm = decoder.decode_access_unit(&access_unit).unwrap();

        assert_eq!(pcm.len(), ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT);
        assert!(pcm.iter().any(|sample| *sample != 0));
    }

    #[test]
    fn encoder_uses_the_same_audio_specific_config_as_apple() {
        let encoder = AacEldEncoder::new().unwrap();

        assert_eq!(
            encoder.audio_specific_config(),
            ARD_AAC_ELD_AUDIO_SPECIFIC_CONFIG
        );
    }

    #[test]
    fn pc_to_mac_encoder_uses_the_apple_mono_audio_specific_config() {
        const TEST_AMPLITUDE: i16 = 10_000;
        const TEST_HALF_PERIOD_FRAMES: usize = 24;
        let encoder = AacEldEncoder::new_for_pc_to_mac().unwrap();

        assert_eq!(
            encoder.audio_specific_config(),
            ARD_AAC_ELD_PC_TO_MAC_AUDIO_SPECIFIC_CONFIG
        );
        let pcm = (0..ARD_AUDIO_PC_TO_MAC_PCM_SAMPLES_PER_ACCESS_UNIT)
            .map(|frame| {
                if (frame / TEST_HALF_PERIOD_FRAMES).is_multiple_of(2) {
                    TEST_AMPLITUDE
                } else {
                    -TEST_AMPLITUDE
                }
            })
            .collect::<Vec<_>>();
        let encoded = encoder.encode_pcm_frame(&pcm).unwrap();
        println!(
            "rust_mono_access_unit_hex={}",
            encoded
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        assert!(!encoded.is_empty());
    }

    #[test]
    fn pc_to_mac_probe_roundtrip_is_non_silent_and_mono() {
        let encoder = AacEldEncoder::new_for_pc_to_mac().unwrap();
        let access_unit = encoder.encode_pcm_frame(&p5_probe_pcm_frame()).unwrap();
        let mut decoder = AacEldDecoder::new_for_pc_to_mac().unwrap();

        let pcm = decoder.decode_access_unit(&access_unit).unwrap();

        assert_eq!(pcm.len(), ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT);
        assert!(pcm.iter().any(|sample| sample.unsigned_abs() > 256));
    }

    #[test]
    fn scheme2_rfc3640_test_helper_writes_single_au_header() {
        const SCHEME2_FIXTURE_ACCESS_UNIT_BYTES: usize = 190;
        const SCHEME2_FIXTURE_RFC3640_HEADER: [u8; 4] = [0x00, 0x10, 0x05, 0xf0];
        let access_unit = vec![0xa5; SCHEME2_FIXTURE_ACCESS_UNIT_BYTES];

        let payload = bundle_rfc3640_single_access_unit(&access_unit).unwrap();

        assert_eq!(
            &payload[..SCHEME2_FIXTURE_RFC3640_HEADER.len()],
            &SCHEME2_FIXTURE_RFC3640_HEADER
        );
        assert_eq!(
            &payload[SCHEME2_FIXTURE_RFC3640_HEADER.len()..],
            access_unit
        );
    }

    #[test]
    fn encoder_produces_raw_access_units_accepted_by_the_apple_compatible_decoder() {
        const TEST_AMPLITUDE: i16 = 10_000;
        const TEST_HALF_PERIOD_FRAMES: usize = 24;
        const TEST_ACCESS_UNIT_COUNT: usize = 4;

        let encoder = AacEldEncoder::new().unwrap();
        let mut decoder = AacEldDecoder::new().unwrap();
        let mut decoded_nonzero_sample = false;
        for access_unit_index in 0..TEST_ACCESS_UNIT_COUNT {
            let mut pcm = vec![0i16; ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT];
            for frame_index in 0..super::ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT {
                let global_frame =
                    access_unit_index * super::ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT + frame_index;
                let sample = if (global_frame / TEST_HALF_PERIOD_FRAMES).is_multiple_of(2) {
                    TEST_AMPLITUDE
                } else {
                    -TEST_AMPLITUDE
                };
                let stereo_offset = frame_index * super::ARD_AUDIO_CHANNEL_COUNT;
                pcm[stereo_offset] = sample;
                pcm[stereo_offset + 1] = sample;
            }
            let encoded = encoder.encode_pcm_frame(&pcm).unwrap();
            let decoded = decoder.decode_access_unit(&encoded).unwrap();
            decoded_nonzero_sample |= decoded.iter().any(|sample| *sample != 0);
        }

        assert!(decoded_nonzero_sample);
    }

    fn audio_rtp_with_access_unit(
        sequence: u16,
        timestamp: u32,
        payload_type: u8,
        access_unit: &[u8],
    ) -> Vec<u8> {
        const RTP_VERSION_AND_FLAGS: u8 = 2 << 6;
        const TEST_SSRC: u32 = 0x1020_3040;
        let mut packet = vec![RTP_VERSION_AND_FLAGS, payload_type];
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&timestamp.to_be_bytes());
        packet.extend_from_slice(&TEST_SSRC.to_be_bytes());
        packet.extend_from_slice(access_unit);
        packet
    }

    fn audio_rtp(sequence: u16, timestamp: u32, payload_type: u8) -> Vec<u8> {
        audio_rtp_with_access_unit(
            sequence,
            timestamp,
            payload_type,
            &ARD_AAC_ELD_DTX_ACCESS_UNIT,
        )
    }

    #[test]
    fn audio_receiver_conceals_a_timestamp_aligned_sequence_gap() {
        const FIRST_SEQUENCE: u16 = 100;
        const FIRST_TIMESTAMP: u32 = 48_000;
        let mut receiver = ArdAudioReceiver::new().unwrap();
        receiver
            .decode_rtp_packet(&audio_rtp(
                FIRST_SEQUENCE,
                FIRST_TIMESTAMP,
                ARD_AUDIO_RTP_PAYLOAD_TYPE,
            ))
            .unwrap();

        let AudioReceiveOutcome::Decoded(decoded) = receiver
            .decode_rtp_packet(&audio_rtp(
                FIRST_SEQUENCE + 2,
                FIRST_TIMESTAMP + (ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32 * 2),
                ARD_AUDIO_RTP_PAYLOAD_TYPE,
            ))
            .unwrap()
        else {
            panic!("forward packet must decode")
        };

        assert_eq!(decoded.concealed_access_units, 1);
        assert_eq!(decoded.pcm.len(), ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT * 2);
    }

    #[test]
    fn audio_receiver_discards_late_packet_without_advancing_forward_state() {
        const FIRST_SEQUENCE: u16 = 100;
        const FIRST_TIMESTAMP: u32 = 48_000;
        let timestamp =
            |advance: u32| FIRST_TIMESTAMP + advance * ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32;
        let mut receiver = ArdAudioReceiver::new().unwrap();

        assert!(matches!(
            receiver
                .decode_rtp_packet(&audio_rtp(
                    FIRST_SEQUENCE,
                    timestamp(0),
                    ARD_AUDIO_RTP_PAYLOAD_TYPE,
                ))
                .unwrap(),
            AudioReceiveOutcome::Decoded(_)
        ));

        let AudioReceiveOutcome::Decoded(gapped) = receiver
            .decode_rtp_packet(&audio_rtp(
                FIRST_SEQUENCE + 2,
                timestamp(2),
                ARD_AUDIO_RTP_PAYLOAD_TYPE,
            ))
            .unwrap()
        else {
            panic!("forward packet must decode")
        };
        assert_eq!(gapped.concealed_access_units, 1);

        assert_eq!(
            receiver
                .decode_rtp_packet(&audio_rtp(
                    FIRST_SEQUENCE + 1,
                    timestamp(1),
                    ARD_AUDIO_RTP_PAYLOAD_TYPE,
                ))
                .unwrap(),
            AudioReceiveOutcome::DiscardedLate {
                sequence: FIRST_SEQUENCE + 1,
                last_forward_sequence: FIRST_SEQUENCE + 2,
            }
        );

        let AudioReceiveOutcome::Decoded(next) = receiver
            .decode_rtp_packet(&audio_rtp(
                FIRST_SEQUENCE + 3,
                timestamp(3),
                ARD_AUDIO_RTP_PAYLOAD_TYPE,
            ))
            .unwrap()
        else {
            panic!("next forward packet must decode")
        };
        assert_eq!(next.concealed_access_units, 0);
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AAC-ELD fixture"]
    fn audio_receiver_resynchronizes_a_large_forward_gap_and_reanchors_timestamp() {
        const FIRST_SEQUENCE: u16 = 100;
        const RESYNC_SEQUENCE: u16 = 218;
        const FIRST_TIMESTAMP: u32 = 48_000;
        const RESYNC_TIMESTAMP: u32 = 9_000_000;
        let fixture = crate::vnc::read_private_fixture_text(
            "ard_re/fixtures/ard_aac_eld_active_access_unit.hex",
        );
        let access_unit = decode_hex_fixture(&fixture);
        let mut receiver = ArdAudioReceiver::new().unwrap();

        assert!(matches!(
            receiver
                .decode_rtp_packet(&audio_rtp_with_access_unit(
                    FIRST_SEQUENCE,
                    FIRST_TIMESTAMP,
                    ARD_AUDIO_RTP_PAYLOAD_TYPE,
                    &access_unit,
                ))
                .unwrap(),
            AudioReceiveOutcome::Decoded(_)
        ));

        let outcome = receiver
            .decode_rtp_packet(&audio_rtp_with_access_unit(
                RESYNC_SEQUENCE,
                RESYNC_TIMESTAMP,
                ARD_AUDIO_RTP_PAYLOAD_TYPE,
                &access_unit,
            ))
            .expect("large authenticated forward gap must resynchronize");
        let AudioReceiveOutcome::Resynchronized {
            decoded: resynchronized,
            skipped_access_units,
        } = outcome
        else {
            panic!("large forward gap must produce a typed resynchronization")
        };
        assert_eq!(skipped_access_units, 117);
        assert_eq!(resynchronized.concealed_access_units, 0);
        assert_eq!(
            resynchronized.pcm.len(),
            ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT
        );

        let AudioReceiveOutcome::Decoded(next) = receiver
            .decode_rtp_packet(&audio_rtp_with_access_unit(
                RESYNC_SEQUENCE + 1,
                RESYNC_TIMESTAMP + ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32,
                ARD_AUDIO_RTP_PAYLOAD_TYPE,
                &access_unit,
            ))
            .unwrap()
        else {
            panic!("packet after resynchronization must decode normally")
        };
        assert_eq!(next.concealed_access_units, 0);
        assert_eq!(next.pcm.len(), ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT);
    }

    #[test]
    fn audio_receiver_failed_large_gap_decode_preserves_forward_anchor() {
        const FIRST_SEQUENCE: u16 = 100;
        const FIRST_TIMESTAMP: u32 = 48_000;
        let mut receiver = ArdAudioReceiver::new().unwrap();
        receiver
            .decode_rtp_packet(&audio_rtp(
                FIRST_SEQUENCE,
                FIRST_TIMESTAMP,
                ARD_AUDIO_RTP_PAYLOAD_TYPE,
            ))
            .unwrap();

        let error = receiver
            .decode_rtp_packet(&audio_rtp_with_access_unit(
                FIRST_SEQUENCE + 118,
                9_000_000,
                ARD_AUDIO_RTP_PAYLOAD_TYPE,
                &[0],
            ))
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("AAC-ELD"),
            "large-gap packet must reach the fresh decoder: {error:#}"
        );

        let AudioReceiveOutcome::Decoded(next) = receiver
            .decode_rtp_packet(&audio_rtp(
                FIRST_SEQUENCE + 1,
                FIRST_TIMESTAMP + ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32,
                ARD_AUDIO_RTP_PAYLOAD_TYPE,
            ))
            .unwrap()
        else {
            panic!("failed resynchronization must preserve the prior anchor")
        };
        assert_eq!(next.concealed_access_units, 0);
    }

    #[test]
    fn audio_receiver_rejects_an_unnegotiated_payload_type() {
        let mut receiver = ArdAudioReceiver::new().unwrap();
        let error = receiver
            .decode_rtp_packet(&audio_rtp(1, 0, ARD_AUDIO_RTP_PAYLOAD_TYPE - 1))
            .unwrap_err();

        assert!(error.to_string().contains("payload type"));
    }
}
