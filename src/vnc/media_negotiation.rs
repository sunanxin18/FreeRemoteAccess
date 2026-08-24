//! Apple Screen Sharing 的 AVConference 媒体协商控制面。
//!
//! 这里的布局来自当前 ScreenSharing/AVConference 的静态与离线运行时证据：
//! 客户端 `0x1c` version 3、二进制 plist offer、zlib protobuf，以及服务器
//! `MediaStream Message 2` version 2。SRTP 主材料固定为 AES-256 的 32 字节
//! 主密钥和 14 字节主盐。

use anyhow::{bail, ensure, Context, Result};
use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::vnc::media_protocol::{
    MEDIA_STREAM_ANSWER_KIND, MEDIA_STREAM_ANSWER_VERSION, MEDIA_STREAM_CONTROL_ENCODING,
    PRIMARY_MEDIA_STREAM_ID,
};

pub const SRTP_AES_256_MASTER_KEY_LEN: usize = 32;
pub const SRTP_MASTER_SALT_LEN: usize = 14;
pub const SRTP_MASTER_MATERIAL_LEN: usize = SRTP_AES_256_MASTER_KEY_LEN + SRTP_MASTER_SALT_LEN;

const CLIENT_MEDIA_CONFIGURATION_MESSAGE_TYPE: u8 = 0x1c;
const CLIENT_MEDIA_CONFIGURATION_RESERVED: u8 = 0;
const CLIENT_MEDIA_CONFIGURATION_VERSION: u16 = 3;
const CLIENT_MEDIA_CONFIGURATION_PREFIX_LEN: usize = 4;
const CLIENT_MEDIA_CONFIGURATION_HEADER_LEN: usize = 20;
const CLIENT_MEDIA_CONFIGURATION_SESSION_ID_LEN: usize = 16;
const CLIENT_MEDIA_CONFIGURATION_OFFER_COUNT: usize = 5;
const CLIENT_MEDIA_CONFIGURATION_STREAMS_SUPPORTING_OFFERS: usize = 3;
const MEDIA_CONFIGURATION_STREAM_1_SUPPORTS_60_FPS: u32 = 1 << 0;
const MEDIA_CONFIGURATION_DO_NOT_SEND_CURSOR: u32 = 1 << 2;
const MEDIA_CONFIGURATION_CALLER_KIND_ONE: u32 = 1 << 3;
const SCREEN_SHARING_ONE_VIDEO_CONFIGURATION_FLAGS: u32 =
    MEDIA_CONFIGURATION_STREAM_1_SUPPORTS_60_FPS
        | MEDIA_CONFIGURATION_DO_NOT_SEND_CURSOR
        | MEDIA_CONFIGURATION_CALLER_KIND_ONE;

const SCREEN_SHARING_AUDIO_NEGOTIATOR_MODE: u8 = 8;
const SCREEN_SHARING_REMOTE_MICROPHONE_NEGOTIATOR_MODE: u8 = 4;
const SCREEN_SHARING_VIDEO_NEGOTIATOR_MODE: u8 = 5;
const MEDIA_NEGOTIATOR_MODE_KEY: &str = "avcMediaStreamNegotiatorMode";
const MEDIA_NEGOTIATOR_MEDIA_BLOB_KEY: &str = "avcMediaStreamNegotiatorMediaBlob";
const MEDIA_STREAM_REMOTE_ENDPOINT_INFO_KEY: &str = "avcMediaStreamOptionRemoteEndpointInfo";
const MEDIA_STREAM_CALL_ID_KEY: &str = "avcMediaStreamOptionCallID";
const VICEROY_USER_AGENT: &[u8] = b"Viceroy 1.7.0";
const VICEROY_ALLOW_DYNAMIC_MAX_BITRATE_FIELD: u32 = 1;
const VICEROY_PRESERVE_ASPECT_ON_CONTENT_CHANGE_FIELD: u32 = 2;
const VICEROY_AUDIO_SETTINGS_FIELD: u32 = 3;
const VICEROY_VIDEO_SETTINGS_FIELD: u32 = 5;
const VICEROY_USER_AGENT_FIELD: u32 = 6;
const VICEROY_ACCESS_NETWORK_TYPE_FIELD: u32 = 8;
const VICEROY_BANDWIDTH_SETTINGS_FIELD: u32 = 9;
const VICEROY_NTP_TIME_FIELD: u32 = 13;
const VICEROY_BLOB_VERSION_FIELD: u32 = 14;
const VICEROY_MEDIA_CONTROL_INFO_VERSION_FIELD: u32 = 16;
const VICEROY_ACCESS_NETWORK_TYPE_UNKNOWN: u64 = 0;
const VICEROY_CURRENT_BLOB_VERSION: u64 = 2;
const VICEROY_CURRENT_MEDIA_CONTROL_INFO_VERSION: u64 = 0;

const BANDWIDTH_CONFIGURATION_FIELD: u32 = 1;
const BANDWIDTH_MAXIMUM_FIELD: u32 = 2;
const BANDWIDTH_CONFIGURATION_EXTENSION_FIELD: u32 = 3;

#[derive(Clone, Copy)]
struct BandwidthSetting {
    configuration: u64,
    maximum: u64,
    configuration_extension: Option<u64>,
}

// Version-locked default capability set returned by current AVConference's
// `VCMediaNegotiationBlobBandwidthSettings::newBandwidthConfigurations` and
// observed in modes 4, 5, and 8. Repeated-field ordering is not semantic.
const SCREEN_SHARING_BANDWIDTH_SETTINGS: [BandwidthSetting; 10] = [
    BandwidthSetting {
        configuration: 0,
        maximum: 40_000_000,
        configuration_extension: Some(12 * 1024),
    },
    BandwidthSetting {
        configuration: 4,
        maximum: 6_500,
        configuration_extension: None,
    },
    BandwidthSetting {
        configuration: 4_074,
        maximum: 0,
        configuration_extension: Some(16 * 1024),
    },
    BandwidthSetting {
        configuration: 0,
        maximum: 6_000_000,
        configuration_extension: Some(128 * 1024),
    },
    BandwidthSetting {
        configuration: 1,
        maximum: 299,
        configuration_extension: None,
    },
    BandwidthSetting {
        configuration: 0,
        maximum: 75_000_000,
        configuration_extension: Some(512 * 1024),
    },
    BandwidthSetting {
        configuration: 0,
        maximum: 20_000_000,
        configuration_extension: Some(96 * 1024),
    },
    BandwidthSetting {
        configuration: 0,
        maximum: 60_000_000,
        configuration_extension: Some(256 * 1024),
    },
    BandwidthSetting {
        configuration: 16,
        maximum: 4_100,
        configuration_extension: None,
    },
    BandwidthSetting {
        configuration: 0,
        maximum: 100_000_000,
        configuration_extension: Some(1024 * 1024),
    },
];

const REMOTE_ENDPOINT_NETWORK_KIND_FIELD: u32 = 1;
const REMOTE_ENDPOINT_DEVICE_ROLE_FIELD: u32 = 2;
const REMOTE_ENDPOINT_MODEL_FIELD: u32 = 3;
const REMOTE_ENDPOINT_PRODUCT_VERSION_FIELD: u32 = 4;
const REMOTE_ENDPOINT_BUILD_VERSION_FIELD: u32 = 5;
const SCREEN_SHARING_REMOTE_ENDPOINT_NETWORK_KIND: u64 = 0;
const SCREEN_SHARING_REMOTE_ENDPOINT_DEVICE_ROLE: u64 = 1;
const SCREEN_SHARING_COMPATIBILITY_MODEL: &[u8] = b"Mac16,10";
const SCREEN_SHARING_COMPATIBILITY_PRODUCT_VERSION: &[u8] = b"2215.5.1";
const SCREEN_SHARING_COMPATIBILITY_BUILD_VERSION: &[u8] = b"25G83";

const NTP_UNIX_EPOCH_OFFSET_SECONDS: u64 = 2_208_988_800;
const NTP_FRACTION_BITS: u32 = 32;
const NANOSECONDS_PER_SECOND: u128 = 1_000_000_000;
const UUID_VERSION_INDEX: usize = 6;
const UUID_VARIANT_INDEX: usize = 8;
const UUID_VERSION_MASK: u8 = 0x0f;
const UUID_VERSION_4: u8 = 0x40;
const UUID_VARIANT_MASK: u8 = 0x3f;
const UUID_VARIANT_RFC_4122: u8 = 0x80;
const UUID_TEXT_CAPACITY: usize = 36;
const UUID_HYPHEN_BEFORE_BYTE_INDICES: [usize; 4] = [4, 6, 8, 10];

const AUDIO_SETTINGS_RTP_SSRC_FIELD: u32 = 1;
const AUDIO_SETTINGS_AUDIO_UNIT_MODEL_FIELD: u32 = 2;
const AUDIO_SETTINGS_SUPPORT_FLAGS_FIELD: u32 = 3;
const AUDIO_SETTINGS_PAYLOAD_FLAGS_FIELD: u32 = 4;
const AUDIO_SETTINGS_SECONDARY_FLAGS_FIELD: u32 = 5;
const AUDIO_SETTINGS_USE_SBR_FIELD: u32 = 6;
const SCREEN_SHARING_AUDIO_UNIT_MODEL: u64 = 0;
const SCREEN_SHARING_AUDIO_SUPPORT_FLAGS: u64 = 0;
const SCREEN_SHARING_AUDIO_PAYLOAD_FLAGS: u64 = 24_191;
const SCREEN_SHARING_AUDIO_SECONDARY_FLAGS: u64 = 0;
const SCREEN_SHARING_AUDIO_USE_SBR: u64 = 0;

const VIDEO_SETTINGS_RTP_SSRC_FIELD: u32 = 1;
const VIDEO_SETTINGS_ALLOW_RTCP_FEEDBACK_FIELD: u32 = 2;
const VIDEO_SETTINGS_PAYLOAD_COLLECTION_FIELD: u32 = 3;
const VIDEO_SETTINGS_LTRP_ENABLED_FIELD: u32 = 7;
const VIDEO_SETTINGS_PIXEL_FORMATS_FIELD: u32 = 8;
const VIDEO_SETTINGS_BLACK_FRAME_ON_CLEAR_FIELD: u32 = 12;
const SCREEN_SHARING_ALLOW_RTCP_FEEDBACK: u64 = 0;
const SCREEN_SHARING_LTRP_ENABLED: u64 = 1;
const SCREEN_SHARING_PIXEL_FORMATS: u64 = 63;
const SCREEN_SHARING_BLACK_FRAME_ON_CLEAR: u64 = 1;

const VIDEO_PAYLOAD_TYPE_FIELD: u32 = 1;
const VIDEO_PAYLOAD_RULE_COLLECTION_FIELD: u32 = 2;
const VIDEO_PAYLOAD_FEATURE_STRING_FIELD: u32 = 3;
const VIDEO_PAYLOAD_PARAMETER_SET_FIELD: u32 = 4;
const SCREEN_SHARING_PRIMARY_VIDEO_PAYLOAD_TYPE: u64 = 123;
const SCREEN_SHARING_FALLBACK_VIDEO_PAYLOAD_TYPE: u64 = 100;
const SCREEN_SHARING_PRIMARY_VIDEO_FEATURES: &[u8] = b"FLS;SW:1;";
const SCREEN_SHARING_FALLBACK_VIDEO_FEATURES: &[u8] = b"FLS;VRAE:0;SW:1;";
const SCREEN_SHARING_PRIMARY_PARAMETER_SET: u64 = 1;
const SCREEN_SHARING_FALLBACK_PARAMETER_SET: u64 = 14;

const VIDEO_RULE_TRANSPORT_FIELD: u32 = 1;
const VIDEO_RULE_OPERATION_FIELD: u32 = 2;
const VIDEO_RULE_FORMATS_FIELD: u32 = 3;
const VIDEO_RULE_PREFERRED_FORMAT_FIELD: u32 = 4;
const SCREEN_SHARING_VIDEO_RULE_TRANSPORT: u64 = 1;
const SCREEN_SHARING_VIDEO_RULE_OPERATION_OFFER: u64 = 1;
const SCREEN_SHARING_VIDEO_RULE_OPERATION_ANSWER: u64 = 2;
const SCREEN_SHARING_VIDEO_FORMATS: u64 = 50_115;
const SCREEN_SHARING_VIDEO_PREFERRED_FORMAT: u64 = 0;

const MEDIA_STREAM_CONTROL_RECTANGLE_LEN: usize = 8;
const MEDIA_STREAM_ANSWER_STREAM_1_SUPPORTS_60_FPS: u32 = 1 << 0;
const MEDIA_STREAM_ANSWER_STREAM_2_SUPPORTS_60_FPS: u32 = 1 << 1;
const MEDIA_STREAM_ANSWER_KNOWN_FLAGS: u32 =
    MEDIA_STREAM_ANSWER_STREAM_1_SUPPORTS_60_FPS | MEDIA_STREAM_ANSWER_STREAM_2_SUPPORTS_60_FPS;
const MEDIA_STREAM_ANSWER_RESERVED_LEN: usize = 4;
const MEDIA_STREAM_ANSWER_COUNT: usize = 3;
const MEDIA_STREAM_ANSWER_FIXED_FRAME_LEN: usize = 36;
const MAX_COMPRESSED_NEGOTIATION_BLOB_LEN: usize = u16::MAX as usize;
const MAX_DECOMPRESSED_NEGOTIATION_BLOB_LEN: usize = 1024 * 1024;
const PROTOBUF_MAX_FIELD_NUMBER: u64 = (1 << 29) - 1;
const PROTOBUF_VARINT_WIRE_TYPE: u64 = 0;
const PROTOBUF_FIXED64_WIRE_TYPE: u64 = 1;
const PROTOBUF_LENGTH_DELIMITED_WIRE_TYPE: u64 = 2;
const PROTOBUF_FIXED32_WIRE_TYPE: u64 = 5;

const BINARY_PLIST_HEADER: &[u8] = b"bplist00";
const BINARY_PLIST_DICTIONARY_MARKER: u8 = 0xd0;
const BINARY_PLIST_ASCII_STRING_MARKER: u8 = 0x50;
const BINARY_PLIST_DATA_MARKER: u8 = 0x40;
const BINARY_PLIST_INTEGER_MARKER: u8 = 0x10;
const BINARY_PLIST_INLINE_LENGTH_LIMIT: usize = 15;
const BINARY_PLIST_TRAILER_LEN: usize = 32;
const BINARY_PLIST_TRAILER_UNUSED_LEN: usize = 6;
const BINARY_PLIST_TRAILER_OFFSET_SIZE_INDEX: usize = 6;
const BINARY_PLIST_TRAILER_REFERENCE_SIZE_INDEX: usize = 7;
const BINARY_PLIST_TRAILER_OBJECT_COUNT_RANGE: std::ops::Range<usize> = 8..16;
const BINARY_PLIST_TRAILER_ROOT_OBJECT_RANGE: std::ops::Range<usize> = 16..24;
const BINARY_PLIST_TRAILER_OFFSET_TABLE_RANGE: std::ops::Range<usize> = 24..32;
const BINARY_PLIST_OBJECT_TYPE_MASK: u8 = 0xf0;
const BINARY_PLIST_OBJECT_INFO_MASK: u8 = 0x0f;
const BINARY_PLIST_EXTENDED_COUNT: u8 = 0x0f;
const BINARY_PLIST_MAX_INTEGER_WIDTH: usize = size_of::<u64>();
const BINARY_PLIST_MAX_OBJECTS: usize = 256;
const BINARY_PLIST_MAX_DICTIONARY_ENTRIES: usize = 16;
const BINARY_PLIST_MAX_KEY_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SrtpMasterMaterial {
    pub master_key: [u8; SRTP_AES_256_MASTER_KEY_LEN],
    pub master_salt: [u8; SRTP_MASTER_SALT_LEN],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaNegotiatorMode {
    Audio,
    RemoteMicrophone,
    Video,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioMediaFlow {
    MacToPc,
    #[cfg_attr(
        not(any(feature = "viewer", test)),
        allow(
            dead_code,
            reason = "headless HPSS 仅接收远端音频，不公开远程麦克风入口"
        )
    )]
    PcToMac,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaNegotiatorOffer {
    pub mode: MediaNegotiatorMode,
    pub local_ssrc: u32,
    pub bytes: Vec<u8>,
    pub compressed_media_blob: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaStreamConfigurationEntry {
    pub viewer_to_server: SrtpMasterMaterial,
    pub server_to_viewer: SrtpMasterMaterial,
    pub offer: MediaNegotiatorOffer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientMediaStreamConfiguration {
    pub session_id: [u8; 16],
    pub audio: MediaStreamConfigurationEntry,
    pub video_stream_1: MediaStreamConfigurationEntry,
    pub video_stream_2: Option<MediaStreamConfigurationEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompressedProtobufAnswer {
    pub compressed: Vec<u8>,
    pub decompressed: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaStreamAnswer {
    pub stream_1_supports_60_fps: bool,
    pub stream_2_supports_60_fps: bool,
    pub audio: CompressedProtobufAnswer,
    pub video_stream_1: CompressedProtobufAnswer,
    pub video_stream_2: Option<CompressedProtobufAnswer>,
}

fn random_bytes<const LENGTH: usize>() -> Result<[u8; LENGTH]> {
    let mut bytes = [0u8; LENGTH];
    getrandom::getrandom(&mut bytes).context("生成媒体协商随机材料失败")?;
    Ok(bytes)
}

fn random_nonzero_ssrc() -> Result<u32> {
    loop {
        let ssrc = u32::from_ne_bytes(random_bytes()?);
        if ssrc != 0 {
            return Ok(ssrc);
        }
    }
}

fn random_uuid_v4_bytes() -> Result<[u8; CLIENT_MEDIA_CONFIGURATION_SESSION_ID_LEN]> {
    let mut uuid = random_bytes()?;
    uuid[UUID_VERSION_INDEX] = (uuid[UUID_VERSION_INDEX] & UUID_VERSION_MASK) | UUID_VERSION_4;
    uuid[UUID_VARIANT_INDEX] =
        (uuid[UUID_VARIANT_INDEX] & UUID_VARIANT_MASK) | UUID_VARIANT_RFC_4122;
    Ok(uuid)
}

fn uuid_text(uuid: &[u8; CLIENT_MEDIA_CONFIGURATION_SESSION_ID_LEN]) -> String {
    let mut text = String::with_capacity(UUID_TEXT_CAPACITY);
    for (index, byte) in uuid.iter().enumerate() {
        if UUID_HYPHEN_BEFORE_BYTE_INDICES.contains(&index) {
            text.push('-');
        }
        write!(&mut text, "{byte:02X}").expect("写入 String 不会失败");
    }
    debug_assert_eq!(text.len(), UUID_TEXT_CAPACITY);
    text
}

fn current_ntp_time() -> Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("系统时间早于 Unix epoch")?;
    let seconds = elapsed
        .as_secs()
        .checked_add(NTP_UNIX_EPOCH_OFFSET_SECONDS)
        .context("NTP 秒数溢出")?;
    ensure!(seconds <= u64::from(u32::MAX), "NTP era 0 秒数已耗尽");
    let fraction =
        (u128::from(elapsed.subsec_nanos()) << NTP_FRACTION_BITS) / NANOSECONDS_PER_SECOND;
    Ok((seconds << NTP_FRACTION_BITS) | fraction as u64)
}

impl SrtpMasterMaterial {
    fn generate() -> Result<Self> {
        Ok(Self {
            master_key: random_bytes()?,
            master_salt: random_bytes()?,
        })
    }
}

impl MediaStreamConfigurationEntry {
    fn generate(mode: MediaNegotiatorMode) -> Result<Self> {
        Ok(Self {
            viewer_to_server: SrtpMasterMaterial::generate()?,
            server_to_viewer: SrtpMasterMaterial::generate()?,
            offer: build_media_negotiator_offer(mode, random_nonzero_ssrc()?)?,
        })
    }

    fn generate_audio(flow: AudioMediaFlow) -> Result<Self> {
        let mode = match flow {
            AudioMediaFlow::MacToPc => MediaNegotiatorMode::Audio,
            AudioMediaFlow::PcToMac => MediaNegotiatorMode::RemoteMicrophone,
        };
        Self::generate(mode)
    }
}

impl ClientMediaStreamConfiguration {
    /// 生成当前 Screen Sharing 单视频流配置；每个方向和角色使用独立随机材料。
    #[cfg(test)]
    pub fn generate_one_video() -> Result<Self> {
        Self::generate_one_video_with_audio_flow(AudioMediaFlow::MacToPc)
    }

    pub fn generate_one_video_with_audio_flow(audio_flow: AudioMediaFlow) -> Result<Self> {
        Ok(Self {
            session_id: random_uuid_v4_bytes()?,
            audio: MediaStreamConfigurationEntry::generate_audio(audio_flow)?,
            video_stream_1: MediaStreamConfigurationEntry::generate(MediaNegotiatorMode::Video)?,
            video_stream_2: None,
        })
    }
}

impl MediaNegotiatorMode {
    fn wire_value(self) -> u8 {
        match self {
            Self::Audio => SCREEN_SHARING_AUDIO_NEGOTIATOR_MODE,
            Self::RemoteMicrophone => SCREEN_SHARING_REMOTE_MICROPHONE_NEGOTIATOR_MODE,
            Self::Video => SCREEN_SHARING_VIDEO_NEGOTIATOR_MODE,
        }
    }
}

fn append_protobuf_varint(mut value: u64, destination: &mut Vec<u8>) {
    while value >= 0x80 {
        destination.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    destination.push(value as u8);
}

fn append_protobuf_varint_field(field: u32, value: u64, destination: &mut Vec<u8>) {
    append_protobuf_varint(u64::from(field) << 3, destination);
    append_protobuf_varint(value, destination);
}

fn append_protobuf_bytes_field(field: u32, value: &[u8], destination: &mut Vec<u8>) {
    append_protobuf_varint(
        (u64::from(field) << 3) | PROTOBUF_LENGTH_DELIMITED_WIRE_TYPE,
        destination,
    );
    append_protobuf_varint(value.len() as u64, destination);
    destination.extend_from_slice(value);
}

fn build_audio_settings(local_ssrc: u32) -> Vec<u8> {
    let mut settings = Vec::new();
    append_protobuf_varint_field(
        AUDIO_SETTINGS_RTP_SSRC_FIELD,
        u64::from(local_ssrc),
        &mut settings,
    );
    append_protobuf_varint_field(
        AUDIO_SETTINGS_AUDIO_UNIT_MODEL_FIELD,
        SCREEN_SHARING_AUDIO_UNIT_MODEL,
        &mut settings,
    );
    append_protobuf_varint_field(
        AUDIO_SETTINGS_SUPPORT_FLAGS_FIELD,
        SCREEN_SHARING_AUDIO_SUPPORT_FLAGS,
        &mut settings,
    );
    append_protobuf_varint_field(
        AUDIO_SETTINGS_PAYLOAD_FLAGS_FIELD,
        SCREEN_SHARING_AUDIO_PAYLOAD_FLAGS,
        &mut settings,
    );
    append_protobuf_varint_field(
        AUDIO_SETTINGS_SECONDARY_FLAGS_FIELD,
        SCREEN_SHARING_AUDIO_SECONDARY_FLAGS,
        &mut settings,
    );
    append_protobuf_varint_field(
        AUDIO_SETTINGS_USE_SBR_FIELD,
        SCREEN_SHARING_AUDIO_USE_SBR,
        &mut settings,
    );
    settings
}

fn build_bandwidth_setting(setting: BandwidthSetting) -> Vec<u8> {
    let mut wire = Vec::new();
    append_protobuf_varint_field(
        BANDWIDTH_CONFIGURATION_FIELD,
        setting.configuration,
        &mut wire,
    );
    append_protobuf_varint_field(BANDWIDTH_MAXIMUM_FIELD, setting.maximum, &mut wire);
    if let Some(extension) = setting.configuration_extension {
        append_protobuf_varint_field(
            BANDWIDTH_CONFIGURATION_EXTENSION_FIELD,
            extension,
            &mut wire,
        );
    }
    wire
}

fn build_remote_endpoint_info() -> Vec<u8> {
    let mut info = Vec::new();
    append_protobuf_varint_field(
        REMOTE_ENDPOINT_NETWORK_KIND_FIELD,
        SCREEN_SHARING_REMOTE_ENDPOINT_NETWORK_KIND,
        &mut info,
    );
    append_protobuf_varint_field(
        REMOTE_ENDPOINT_DEVICE_ROLE_FIELD,
        SCREEN_SHARING_REMOTE_ENDPOINT_DEVICE_ROLE,
        &mut info,
    );
    append_protobuf_bytes_field(
        REMOTE_ENDPOINT_MODEL_FIELD,
        SCREEN_SHARING_COMPATIBILITY_MODEL,
        &mut info,
    );
    append_protobuf_bytes_field(
        REMOTE_ENDPOINT_PRODUCT_VERSION_FIELD,
        SCREEN_SHARING_COMPATIBILITY_PRODUCT_VERSION,
        &mut info,
    );
    append_protobuf_bytes_field(
        REMOTE_ENDPOINT_BUILD_VERSION_FIELD,
        SCREEN_SHARING_COMPATIBILITY_BUILD_VERSION,
        &mut info,
    );
    info
}

fn build_video_rule(operation: u64) -> Vec<u8> {
    let mut rule = Vec::new();
    append_protobuf_varint_field(
        VIDEO_RULE_TRANSPORT_FIELD,
        SCREEN_SHARING_VIDEO_RULE_TRANSPORT,
        &mut rule,
    );
    append_protobuf_varint_field(VIDEO_RULE_OPERATION_FIELD, operation, &mut rule);
    append_protobuf_varint_field(
        VIDEO_RULE_FORMATS_FIELD,
        SCREEN_SHARING_VIDEO_FORMATS,
        &mut rule,
    );
    append_protobuf_varint_field(
        VIDEO_RULE_PREFERRED_FORMAT_FIELD,
        SCREEN_SHARING_VIDEO_PREFERRED_FORMAT,
        &mut rule,
    );
    rule
}

fn build_video_payload(
    payload_type: u64,
    rule_operations: &[u64],
    feature_string: &[u8],
    parameter_set: u64,
) -> Vec<u8> {
    let mut payload = Vec::new();
    append_protobuf_varint_field(VIDEO_PAYLOAD_TYPE_FIELD, payload_type, &mut payload);
    for operation in rule_operations {
        append_protobuf_bytes_field(
            VIDEO_PAYLOAD_RULE_COLLECTION_FIELD,
            &build_video_rule(*operation),
            &mut payload,
        );
    }
    append_protobuf_bytes_field(
        VIDEO_PAYLOAD_FEATURE_STRING_FIELD,
        feature_string,
        &mut payload,
    );
    append_protobuf_varint_field(
        VIDEO_PAYLOAD_PARAMETER_SET_FIELD,
        parameter_set,
        &mut payload,
    );
    payload
}

fn build_video_settings(local_ssrc: u32) -> Vec<u8> {
    let primary_rule_operations = [
        SCREEN_SHARING_VIDEO_RULE_OPERATION_OFFER,
        SCREEN_SHARING_VIDEO_RULE_OPERATION_ANSWER,
        SCREEN_SHARING_VIDEO_RULE_OPERATION_OFFER,
        SCREEN_SHARING_VIDEO_RULE_OPERATION_ANSWER,
    ];
    let fallback_rule_operations = [
        SCREEN_SHARING_VIDEO_RULE_OPERATION_OFFER,
        SCREEN_SHARING_VIDEO_RULE_OPERATION_ANSWER,
    ];
    let primary_payload = build_video_payload(
        SCREEN_SHARING_PRIMARY_VIDEO_PAYLOAD_TYPE,
        &primary_rule_operations,
        SCREEN_SHARING_PRIMARY_VIDEO_FEATURES,
        SCREEN_SHARING_PRIMARY_PARAMETER_SET,
    );
    let fallback_payload = build_video_payload(
        SCREEN_SHARING_FALLBACK_VIDEO_PAYLOAD_TYPE,
        &fallback_rule_operations,
        SCREEN_SHARING_FALLBACK_VIDEO_FEATURES,
        SCREEN_SHARING_FALLBACK_PARAMETER_SET,
    );

    let mut settings = Vec::new();
    append_protobuf_varint_field(
        VIDEO_SETTINGS_RTP_SSRC_FIELD,
        u64::from(local_ssrc),
        &mut settings,
    );
    append_protobuf_varint_field(
        VIDEO_SETTINGS_ALLOW_RTCP_FEEDBACK_FIELD,
        SCREEN_SHARING_ALLOW_RTCP_FEEDBACK,
        &mut settings,
    );
    append_protobuf_bytes_field(
        VIDEO_SETTINGS_PAYLOAD_COLLECTION_FIELD,
        &primary_payload,
        &mut settings,
    );
    append_protobuf_bytes_field(
        VIDEO_SETTINGS_PAYLOAD_COLLECTION_FIELD,
        &fallback_payload,
        &mut settings,
    );
    append_protobuf_varint_field(
        VIDEO_SETTINGS_LTRP_ENABLED_FIELD,
        SCREEN_SHARING_LTRP_ENABLED,
        &mut settings,
    );
    append_protobuf_varint_field(
        VIDEO_SETTINGS_PIXEL_FORMATS_FIELD,
        SCREEN_SHARING_PIXEL_FORMATS,
        &mut settings,
    );
    append_protobuf_varint_field(
        VIDEO_SETTINGS_BLACK_FRAME_ON_CLEAR_FIELD,
        SCREEN_SHARING_BLACK_FRAME_ON_CLEAR,
        &mut settings,
    );
    settings
}

fn build_viceroy_offer_blob(mode: MediaNegotiatorMode, local_ssrc: u32, ntp_time: u64) -> Vec<u8> {
    let mut blob = Vec::new();
    append_protobuf_varint_field(VICEROY_ALLOW_DYNAMIC_MAX_BITRATE_FIELD, 1, &mut blob);
    append_protobuf_varint_field(
        VICEROY_PRESERVE_ASPECT_ON_CONTENT_CHANGE_FIELD,
        1,
        &mut blob,
    );
    let (role_field, settings) = match mode {
        MediaNegotiatorMode::Audio | MediaNegotiatorMode::RemoteMicrophone => (
            VICEROY_AUDIO_SETTINGS_FIELD,
            build_audio_settings(local_ssrc),
        ),
        MediaNegotiatorMode::Video => (
            VICEROY_VIDEO_SETTINGS_FIELD,
            build_video_settings(local_ssrc),
        ),
    };
    append_protobuf_bytes_field(role_field, &settings, &mut blob);
    append_protobuf_bytes_field(VICEROY_USER_AGENT_FIELD, VICEROY_USER_AGENT, &mut blob);
    append_protobuf_varint_field(
        VICEROY_ACCESS_NETWORK_TYPE_FIELD,
        VICEROY_ACCESS_NETWORK_TYPE_UNKNOWN,
        &mut blob,
    );
    for setting in SCREEN_SHARING_BANDWIDTH_SETTINGS {
        append_protobuf_bytes_field(
            VICEROY_BANDWIDTH_SETTINGS_FIELD,
            &build_bandwidth_setting(setting),
            &mut blob,
        );
    }
    append_protobuf_varint_field(VICEROY_NTP_TIME_FIELD, ntp_time, &mut blob);
    append_protobuf_varint_field(
        VICEROY_BLOB_VERSION_FIELD,
        VICEROY_CURRENT_BLOB_VERSION,
        &mut blob,
    );
    append_protobuf_varint_field(
        VICEROY_MEDIA_CONTROL_INFO_VERSION_FIELD,
        VICEROY_CURRENT_MEDIA_CONTROL_INFO_VERSION,
        &mut blob,
    );
    blob
}

fn append_binary_plist_count(marker: u8, count: usize, destination: &mut Vec<u8>) -> Result<()> {
    if count < BINARY_PLIST_INLINE_LENGTH_LIMIT {
        destination.push(marker | count as u8);
        return Ok(());
    }
    destination.push(marker | BINARY_PLIST_INLINE_LENGTH_LIMIT as u8);
    if let Ok(count) = u8::try_from(count) {
        destination.push(BINARY_PLIST_INTEGER_MARKER);
        destination.push(count);
    } else if let Ok(count) = u16::try_from(count) {
        destination.push(BINARY_PLIST_INTEGER_MARKER | 1);
        destination.extend_from_slice(&count.to_be_bytes());
    } else if let Ok(count) = u32::try_from(count) {
        destination.push(BINARY_PLIST_INTEGER_MARKER | 2);
        destination.extend_from_slice(&count.to_be_bytes());
    } else {
        let count = u64::try_from(count).context("二进制 plist 长度超过 u64")?;
        destination.push(BINARY_PLIST_INTEGER_MARKER | 3);
        destination.extend_from_slice(&count.to_be_bytes());
    }
    Ok(())
}

fn binary_plist_ascii_string(value: &str) -> Result<Vec<u8>> {
    ensure!(value.is_ascii(), "二进制 plist 键必须是 ASCII");
    let mut object = Vec::new();
    append_binary_plist_count(BINARY_PLIST_ASCII_STRING_MARKER, value.len(), &mut object)?;
    object.extend_from_slice(value.as_bytes());
    Ok(object)
}

fn binary_plist_data(value: &[u8]) -> Result<Vec<u8>> {
    let mut object = Vec::new();
    append_binary_plist_count(BINARY_PLIST_DATA_MARKER, value.len(), &mut object)?;
    object.extend_from_slice(value);
    Ok(object)
}

fn binary_plist_offer(
    mode: MediaNegotiatorMode,
    compressed_blob: &[u8],
    remote_endpoint_info: &[u8],
    call_id: &str,
) -> Result<Vec<u8>> {
    const OBJECT_COUNT: u64 = 9;
    const ROOT_OBJECT_INDEX: u64 = 0;
    const OBJECT_REFERENCE_SIZE: u8 = 1;
    const REMOTE_ENDPOINT_KEY_OBJECT: u8 = 1;
    const MODE_KEY_OBJECT: u8 = 2;
    const BLOB_KEY_OBJECT: u8 = 3;
    const CALL_ID_KEY_OBJECT: u8 = 4;
    const REMOTE_ENDPOINT_VALUE_OBJECT: u8 = 5;
    const MODE_VALUE_OBJECT: u8 = 6;
    const BLOB_VALUE_OBJECT: u8 = 7;
    const CALL_ID_VALUE_OBJECT: u8 = 8;

    let objects = [
        vec![
            BINARY_PLIST_DICTIONARY_MARKER | 4,
            REMOTE_ENDPOINT_KEY_OBJECT,
            MODE_KEY_OBJECT,
            BLOB_KEY_OBJECT,
            CALL_ID_KEY_OBJECT,
            REMOTE_ENDPOINT_VALUE_OBJECT,
            MODE_VALUE_OBJECT,
            BLOB_VALUE_OBJECT,
            CALL_ID_VALUE_OBJECT,
        ],
        binary_plist_ascii_string(MEDIA_STREAM_REMOTE_ENDPOINT_INFO_KEY)?,
        binary_plist_ascii_string(MEDIA_NEGOTIATOR_MODE_KEY)?,
        binary_plist_ascii_string(MEDIA_NEGOTIATOR_MEDIA_BLOB_KEY)?,
        binary_plist_ascii_string(MEDIA_STREAM_CALL_ID_KEY)?,
        binary_plist_data(remote_endpoint_info)?,
        vec![BINARY_PLIST_INTEGER_MARKER, mode.wire_value()],
        binary_plist_data(compressed_blob)?,
        binary_plist_ascii_string(call_id)?,
    ];
    let mut plist = Vec::from(BINARY_PLIST_HEADER);
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(plist.len());
        plist.extend_from_slice(&object);
    }
    let offset_table_offset = plist.len();
    let maximum_offset = offsets.iter().copied().max().unwrap_or(0);
    let offset_size = if u8::try_from(maximum_offset).is_ok() {
        1u8
    } else if u16::try_from(maximum_offset).is_ok() {
        2u8
    } else if u32::try_from(maximum_offset).is_ok() {
        4u8
    } else {
        8u8
    };
    for offset in offsets {
        let encoded = u64::try_from(offset)
            .context("二进制 plist 对象偏移超过 u64")?
            .to_be_bytes();
        plist.extend_from_slice(&encoded[encoded.len() - usize::from(offset_size)..]);
    }
    plist.extend_from_slice(&[0; BINARY_PLIST_TRAILER_UNUSED_LEN]);
    plist.push(offset_size);
    plist.push(OBJECT_REFERENCE_SIZE);
    plist.extend_from_slice(&OBJECT_COUNT.to_be_bytes());
    plist.extend_from_slice(&ROOT_OBJECT_INDEX.to_be_bytes());
    plist.extend_from_slice(
        &u64::try_from(offset_table_offset)
            .context("二进制 plist 偏移表位置超过 u64")?
            .to_be_bytes(),
    );
    debug_assert!(plist.len() >= BINARY_PLIST_TRAILER_LEN);
    Ok(plist)
}

pub fn build_media_negotiator_offer(
    mode: MediaNegotiatorMode,
    local_ssrc: u32,
) -> Result<MediaNegotiatorOffer> {
    ensure!(local_ssrc != 0, "媒体 offer 的本地 SSRC 不能为零");
    let protobuf = build_viceroy_offer_blob(mode, local_ssrc, current_ntp_time()?);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&protobuf)
        .context("压缩 AVConference offer protobuf 失败")?;
    let compressed_media_blob = encoder.finish().context("完成 offer zlib 失败")?;
    let call_id = uuid_text(&random_uuid_v4_bytes()?);
    let bytes = binary_plist_offer(
        mode,
        &compressed_media_blob,
        &build_remote_endpoint_info(),
        &call_id,
    )?;
    ensure!(
        bytes.len() <= usize::from(u16::MAX),
        "AVConference offer 超过 u16 长度"
    );
    Ok(MediaNegotiatorOffer {
        mode,
        local_ssrc,
        bytes,
        compressed_media_blob,
    })
}

fn append_master_material(material: &SrtpMasterMaterial, destination: &mut Vec<u8>) {
    destination.extend_from_slice(&material.master_key);
    destination.extend_from_slice(&material.master_salt);
}

fn append_configuration_entry(entry: &MediaStreamConfigurationEntry, destination: &mut Vec<u8>) {
    append_master_material(&entry.viewer_to_server, destination);
    append_master_material(&entry.server_to_viewer, destination);
    destination.extend_from_slice(&entry.offer.bytes);
}

pub fn encode_client_media_stream_configuration(
    configuration: &ClientMediaStreamConfiguration,
) -> Result<Vec<u8>> {
    ensure!(
        matches!(
            configuration.audio.offer.mode,
            MediaNegotiatorMode::Audio | MediaNegotiatorMode::RemoteMicrophone
        ),
        "音频角色必须携带 audio negotiator offer"
    );
    ensure!(
        configuration.video_stream_1.offer.mode == MediaNegotiatorMode::Video,
        "视频流 1 必须携带 video negotiator offer"
    );
    if let Some(video_stream_2) = &configuration.video_stream_2 {
        ensure!(
            video_stream_2.offer.mode == MediaNegotiatorMode::Video,
            "视频流 2 必须携带 video negotiator offer"
        );
    }
    let offer_lengths = [
        configuration.audio.offer.bytes.len(),
        configuration.video_stream_1.offer.bytes.len(),
        configuration
            .video_stream_2
            .as_ref()
            .map_or(0, |entry| entry.offer.bytes.len()),
    ];
    let mut wire_offer_lengths = [0u16; CLIENT_MEDIA_CONFIGURATION_OFFER_COUNT];
    for (destination, length) in wire_offer_lengths
        .iter_mut()
        .zip(offer_lengths.iter())
        .take(CLIENT_MEDIA_CONFIGURATION_STREAMS_SUPPORTING_OFFERS)
    {
        *destination = u16::try_from(*length).context("媒体 offer 超过 u16 长度")?;
    }

    let stream_count = if configuration.video_stream_2.is_some() {
        3usize
    } else {
        2usize
    };
    let expected_capacity = CLIENT_MEDIA_CONFIGURATION_HEADER_LEN
        + CLIENT_MEDIA_CONFIGURATION_SESSION_ID_LEN
        + stream_count * (SRTP_MASTER_MATERIAL_LEN * 2)
        + offer_lengths.iter().sum::<usize>();
    let mut frame = Vec::with_capacity(expected_capacity);
    frame.push(CLIENT_MEDIA_CONFIGURATION_MESSAGE_TYPE);
    frame.push(CLIENT_MEDIA_CONFIGURATION_RESERVED);
    frame.extend_from_slice(&0u16.to_be_bytes());
    frame.extend_from_slice(&CLIENT_MEDIA_CONFIGURATION_VERSION.to_be_bytes());
    frame.extend_from_slice(&SCREEN_SHARING_ONE_VIDEO_CONFIGURATION_FLAGS.to_be_bytes());
    for length in wire_offer_lengths {
        frame.extend_from_slice(&length.to_be_bytes());
    }
    frame.extend_from_slice(&configuration.session_id);
    append_configuration_entry(&configuration.audio, &mut frame);
    append_configuration_entry(&configuration.video_stream_1, &mut frame);
    if let Some(video_stream_2) = &configuration.video_stream_2 {
        append_configuration_entry(video_stream_2, &mut frame);
    }
    ensure!(
        frame.len() == expected_capacity,
        "媒体配置序列化长度与模型不一致"
    );
    let message_size = frame
        .len()
        .checked_sub(CLIENT_MEDIA_CONFIGURATION_PREFIX_LEN)
        .context("媒体配置长度小于前缀")?;
    let message_size = u16::try_from(message_size).context("媒体配置超过 u16 长度")?;
    frame[2..4].copy_from_slice(&message_size.to_be_bytes());
    Ok(frame)
}

struct WireCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> WireCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn take(&mut self, count: usize, field: &str) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .with_context(|| format!("{field} 长度溢出"))?;
        ensure!(end <= self.bytes.len(), "{field} 截断");
        let value = &self.bytes[self.position..end];
        self.position = end;
        Ok(value)
    }

    fn read_u16(&mut self, field: &str) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2, field)?.try_into()?))
    }

    fn read_u32(&mut self, field: &str) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4, field)?.try_into()?))
    }

    fn read_i32(&mut self, field: &str) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4, field)?.try_into()?))
    }
}

fn parse_protobuf_varint(bytes: &[u8], position: &mut usize) -> Result<u64> {
    let start = *position;
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let octet = *bytes.get(*position).context("protobuf varint 截断")?;
        *position += 1;
        if shift == 63 {
            ensure!(octet <= 1, "protobuf varint 超过 u64");
        }
        value |= u64::from(octet & 0x7f) << shift;
        if octet & 0x80 == 0 {
            let mut canonical = Vec::new();
            append_protobuf_varint(value, &mut canonical);
            ensure!(
                bytes[start..*position] == canonical,
                "protobuf varint 不是最短编码"
            );
            return Ok(value);
        }
    }
    bail!("protobuf varint 过长")
}

fn validate_protobuf(bytes: &[u8]) -> Result<()> {
    ensure!(!bytes.is_empty(), "协商 protobuf 为空");
    let mut position = 0usize;
    while position < bytes.len() {
        let tag = parse_protobuf_varint(bytes, &mut position)?;
        let field_number = tag >> 3;
        let wire_type = tag & 0x07;
        ensure!(
            (1..=PROTOBUF_MAX_FIELD_NUMBER).contains(&field_number),
            "protobuf 字段编号非法"
        );
        match wire_type {
            PROTOBUF_VARINT_WIRE_TYPE => {
                parse_protobuf_varint(bytes, &mut position)?;
            }
            PROTOBUF_FIXED64_WIRE_TYPE => {
                position = position.checked_add(8).context("fixed64 长度溢出")?;
                ensure!(position <= bytes.len(), "protobuf fixed64 截断");
            }
            PROTOBUF_LENGTH_DELIMITED_WIRE_TYPE => {
                let length = usize::try_from(parse_protobuf_varint(bytes, &mut position)?)
                    .context("protobuf 长度超过 usize")?;
                position = position
                    .checked_add(length)
                    .context("protobuf length-delimited 长度溢出")?;
                ensure!(
                    position <= bytes.len(),
                    "protobuf length-delimited 字段截断"
                );
            }
            PROTOBUF_FIXED32_WIRE_TYPE => {
                position = position.checked_add(4).context("fixed32 长度溢出")?;
                ensure!(position <= bytes.len(), "protobuf fixed32 截断");
            }
            _ => bail!("protobuf wire type {wire_type} 未被当前协议允许"),
        }
    }
    Ok(())
}

fn parse_compressed_answer(compressed: &[u8], role: &str) -> Result<CompressedProtobufAnswer> {
    ensure!(!compressed.is_empty(), "{role} answer 为空");
    ensure!(
        compressed.len() <= MAX_COMPRESSED_NEGOTIATION_BLOB_LEN,
        "{role} answer 压缩数据超过资源预算"
    );
    let mut inflater = Decompress::new(true);
    let mut decompressed = vec![0u8; MAX_DECOMPRESSED_NEGOTIATION_BLOB_LEN + 1];
    let status = inflater
        .decompress(compressed, &mut decompressed, FlushDecompress::Finish)
        .with_context(|| format!("{role} answer zlib 解压失败"))?;
    ensure!(
        status == Status::StreamEnd,
        "{role} answer zlib 流不完整或超限"
    );
    ensure!(
        inflater.total_in() == compressed.len() as u64,
        "{role} answer zlib 存在尾随成员或字节"
    );
    let decompressed_len =
        usize::try_from(inflater.total_out()).context("answer 解压长度超过 usize")?;
    ensure!(
        decompressed_len <= MAX_DECOMPRESSED_NEGOTIATION_BLOB_LEN,
        "{role} answer 解压后超过资源预算"
    );
    decompressed.truncate(decompressed_len);
    validate_protobuf(&decompressed).with_context(|| format!("{role} answer protobuf 非法"))?;
    Ok(CompressedProtobufAnswer {
        compressed: compressed.to_vec(),
        decompressed,
    })
}

fn read_binary_plist_unsigned(bytes: &[u8], field: &str) -> Result<u64> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= BINARY_PLIST_MAX_INTEGER_WIDTH,
        "{field} 整数宽度非法"
    );
    Ok(bytes
        .iter()
        .fold(0u64, |value, octet| (value << 8) | u64::from(*octet)))
}

struct BinaryPlistReader<'a> {
    bytes: &'a [u8],
    offsets: Vec<usize>,
    reference_size: usize,
    root_object: usize,
    offset_table_offset: usize,
}

impl<'a> BinaryPlistReader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self> {
        ensure!(
            bytes.starts_with(BINARY_PLIST_HEADER),
            "answer 不是 binary plist"
        );
        ensure!(
            bytes.len() >= BINARY_PLIST_HEADER.len() + BINARY_PLIST_TRAILER_LEN,
            "binary plist 头或 trailer 截断"
        );
        let trailer_start = bytes.len() - BINARY_PLIST_TRAILER_LEN;
        let trailer = &bytes[trailer_start..];
        ensure!(
            trailer[..BINARY_PLIST_TRAILER_UNUSED_LEN]
                .iter()
                .all(|byte| *byte == 0),
            "binary plist trailer 保留区必须为零"
        );
        let offset_size = usize::from(trailer[BINARY_PLIST_TRAILER_OFFSET_SIZE_INDEX]);
        let reference_size = usize::from(trailer[BINARY_PLIST_TRAILER_REFERENCE_SIZE_INDEX]);
        ensure!(
            (1..=BINARY_PLIST_MAX_INTEGER_WIDTH).contains(&offset_size),
            "binary plist offset 宽度非法"
        );
        ensure!(
            (1..=BINARY_PLIST_MAX_INTEGER_WIDTH).contains(&reference_size),
            "binary plist object reference 宽度非法"
        );
        let object_count = usize::try_from(read_binary_plist_unsigned(
            &trailer[BINARY_PLIST_TRAILER_OBJECT_COUNT_RANGE],
            "binary plist object count",
        )?)
        .context("binary plist object count 超过 usize")?;
        ensure!(
            (1..=BINARY_PLIST_MAX_OBJECTS).contains(&object_count),
            "binary plist object count 超过预算"
        );
        let root_object = usize::try_from(read_binary_plist_unsigned(
            &trailer[BINARY_PLIST_TRAILER_ROOT_OBJECT_RANGE],
            "binary plist root object",
        )?)
        .context("binary plist root object 超过 usize")?;
        ensure!(root_object < object_count, "binary plist root object 越界");
        let offset_table_offset = usize::try_from(read_binary_plist_unsigned(
            &trailer[BINARY_PLIST_TRAILER_OFFSET_TABLE_RANGE],
            "binary plist offset table",
        )?)
        .context("binary plist offset table 超过 usize")?;
        ensure!(
            (BINARY_PLIST_HEADER.len()..=trailer_start).contains(&offset_table_offset),
            "binary plist offset table 位置非法"
        );
        let offset_table_len = object_count
            .checked_mul(offset_size)
            .context("binary plist offset table 长度溢出")?;
        ensure!(
            offset_table_offset.checked_add(offset_table_len) == Some(trailer_start),
            "binary plist offset table 长度与 trailer 不一致"
        );

        let mut offsets = Vec::with_capacity(object_count);
        let mut unique_offsets = BTreeSet::new();
        for index in 0..object_count {
            let start = offset_table_offset + index * offset_size;
            let offset = usize::try_from(read_binary_plist_unsigned(
                &bytes[start..start + offset_size],
                "binary plist object offset",
            )?)
            .context("binary plist object offset 超过 usize")?;
            ensure!(
                (BINARY_PLIST_HEADER.len()..offset_table_offset).contains(&offset),
                "binary plist object offset 越界"
            );
            ensure!(
                unique_offsets.insert(offset),
                "binary plist object offset 重复"
            );
            offsets.push(offset);
        }
        Ok(Self {
            bytes,
            offsets,
            reference_size,
            root_object,
            offset_table_offset,
        })
    }

    fn take_object_bytes(&self, start: usize, count: usize, field: &str) -> Result<&'a [u8]> {
        let end = start
            .checked_add(count)
            .ok_or_else(|| anyhow::anyhow!("{field} 长度溢出"))?;
        ensure!(end <= self.offset_table_offset, "{field} 越过 object table");
        Ok(&self.bytes[start..end])
    }

    fn object_payload(
        &self,
        object_index: usize,
        expected_type: u8,
        maximum_count: usize,
        field: &str,
    ) -> Result<(usize, usize)> {
        let object_offset = *self
            .offsets
            .get(object_index)
            .with_context(|| format!("{field} object reference 越界"))?;
        let marker = self.take_object_bytes(object_offset, 1, field)?[0];
        ensure!(
            marker & BINARY_PLIST_OBJECT_TYPE_MASK == expected_type,
            "{field} object 类型非法"
        );
        let inline_count = marker & BINARY_PLIST_OBJECT_INFO_MASK;
        let (count, payload_offset) = if inline_count != BINARY_PLIST_EXTENDED_COUNT {
            (usize::from(inline_count), object_offset + 1)
        } else {
            let count_marker_offset = object_offset + 1;
            let count_marker = self.take_object_bytes(count_marker_offset, 1, field)?[0];
            ensure!(
                count_marker & BINARY_PLIST_OBJECT_TYPE_MASK == BINARY_PLIST_INTEGER_MARKER,
                "{field} 扩展长度不是 integer object"
            );
            let width_power = u32::from(count_marker & BINARY_PLIST_OBJECT_INFO_MASK);
            let integer_width = 1usize
                .checked_shl(width_power)
                .context("binary plist 扩展长度宽度溢出")?;
            ensure!(
                integer_width <= BINARY_PLIST_MAX_INTEGER_WIDTH,
                "{field} 扩展长度整数过宽"
            );
            let integer_offset = count_marker_offset + 1;
            let count = usize::try_from(read_binary_plist_unsigned(
                self.take_object_bytes(integer_offset, integer_width, field)?,
                field,
            )?)
            .with_context(|| format!("{field} 长度超过 usize"))?;
            (count, integer_offset + integer_width)
        };
        ensure!(count <= maximum_count, "{field} 长度超过预算");
        Ok((count, payload_offset))
    }

    fn object_reference(&self, position: usize, field: &str) -> Result<usize> {
        let index = usize::try_from(read_binary_plist_unsigned(
            self.take_object_bytes(position, self.reference_size, field)?,
            field,
        )?)
        .with_context(|| format!("{field} reference 超过 usize"))?;
        ensure!(index < self.offsets.len(), "{field} reference 越界");
        Ok(index)
    }

    fn ascii_string(&self, object_index: usize, field: &str) -> Result<String> {
        let (length, payload_offset) = self.object_payload(
            object_index,
            BINARY_PLIST_ASCII_STRING_MARKER,
            BINARY_PLIST_MAX_KEY_BYTES,
            field,
        )?;
        let bytes = self.take_object_bytes(payload_offset, length, field)?;
        ensure!(bytes.is_ascii(), "{field} 不是 ASCII");
        Ok(std::str::from_utf8(bytes)?.to_owned())
    }

    fn data(&self, object_index: usize, field: &str) -> Result<Vec<u8>> {
        let maximum = self.offset_table_offset - BINARY_PLIST_HEADER.len();
        let (length, payload_offset) =
            self.object_payload(object_index, BINARY_PLIST_DATA_MARKER, maximum, field)?;
        Ok(self
            .take_object_bytes(payload_offset, length, field)?
            .to_vec())
    }

    fn root_data_dictionary(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        let (entry_count, payload_offset) = self.object_payload(
            self.root_object,
            BINARY_PLIST_DICTIONARY_MARKER,
            BINARY_PLIST_MAX_DICTIONARY_ENTRIES,
            "binary plist root dictionary",
        )?;
        let reference_bytes = entry_count
            .checked_mul(self.reference_size)
            .context("binary plist dictionary reference 长度溢出")?;
        self.take_object_bytes(
            payload_offset,
            reference_bytes
                .checked_mul(2)
                .context("binary plist dictionary key/value 长度溢出")?,
            "binary plist root dictionary references",
        )?;
        let value_references_offset = payload_offset + reference_bytes;
        let mut dictionary = BTreeMap::new();
        for index in 0..entry_count {
            let key_reference = self.object_reference(
                payload_offset + index * self.reference_size,
                "binary plist key",
            )?;
            let value_reference = self.object_reference(
                value_references_offset + index * self.reference_size,
                "binary plist value",
            )?;
            let key = self.ascii_string(key_reference, "binary plist key string")?;
            let value = self.data(value_reference, "binary plist data value")?;
            ensure!(
                dictionary.insert(key, value).is_none(),
                "binary plist dictionary key 重复"
            );
        }
        Ok(dictionary)
    }
}

fn parse_avconference_answer_container(
    container: &[u8],
    role: &str,
) -> Result<CompressedProtobufAnswer> {
    ensure!(
        container.len() <= MAX_COMPRESSED_NEGOTIATION_BLOB_LEN,
        "{role} answer plist 超过资源预算"
    );
    let mut dictionary = BinaryPlistReader::new(container)
        .with_context(|| format!("{role} answer plist 非法"))?
        .root_data_dictionary()
        .with_context(|| format!("{role} answer plist 根对象非法"))?;
    let remote_endpoint = dictionary.remove(MEDIA_STREAM_REMOTE_ENDPOINT_INFO_KEY);
    let compressed = dictionary
        .remove(MEDIA_NEGOTIATOR_MEDIA_BLOB_KEY)
        .with_context(|| format!("{role} answer 缺少 media blob"))?;
    ensure!(dictionary.is_empty(), "{role} answer plist 包含未知键");
    if let Some(remote_endpoint) = remote_endpoint {
        validate_protobuf(&remote_endpoint)
            .with_context(|| format!("{role} answer remote endpoint protobuf 非法"))?;
    }
    parse_compressed_answer(&compressed, role)
}

pub fn parse_media_stream_answer(frame: &[u8]) -> Result<MediaStreamAnswer> {
    ensure!(
        frame.len() >= MEDIA_STREAM_ANSWER_FIXED_FRAME_LEN,
        "MediaStream Message 2 头截断"
    );
    let mut cursor = WireCursor::new(frame);
    ensure!(
        cursor.read_u32("媒体流 ID")? == PRIMARY_MEDIA_STREAM_ID,
        "MediaStream Message 2 的媒体流 ID 非法"
    );
    ensure!(
        cursor
            .take(MEDIA_STREAM_CONTROL_RECTANGLE_LEN, "媒体矩形")?
            .iter()
            .all(|byte| *byte == 0),
        "MediaStream Message 2 必须使用零矩形"
    );
    ensure!(
        cursor.read_i32("媒体编码")? == MEDIA_STREAM_CONTROL_ENCODING,
        "不是 MediaStream Message 2 编码"
    );
    let declared_size = usize::from(cursor.read_u16("Message 2 长度")?);
    ensure!(
        cursor.remaining() == declared_size,
        "MediaStream Message 2 实际长度与声明不一致"
    );
    ensure!(
        cursor.read_u16("Message 2 版本")? == MEDIA_STREAM_ANSWER_VERSION,
        "MediaStream Message 2 版本不支持"
    );
    ensure!(
        cursor.read_u16("Message 2 种类")? == MEDIA_STREAM_ANSWER_KIND,
        "媒体流控制消息不是 Message 2 answer"
    );
    let flags = cursor.read_u32("Message 2 标志")?;
    ensure!(
        flags & !MEDIA_STREAM_ANSWER_KNOWN_FLAGS == 0,
        "MediaStream Message 2 包含未知标志"
    );
    let mut answer_lengths = [0usize; MEDIA_STREAM_ANSWER_COUNT];
    for (index, length) in answer_lengths.iter_mut().enumerate() {
        *length = usize::from(cursor.read_u16(&format!("answer {index} 长度"))?);
    }
    ensure!(
        answer_lengths[0] != 0 && answer_lengths[1] != 0,
        "audio 和 video stream 1 answer 不能为空"
    );
    ensure!(
        cursor
            .take(MEDIA_STREAM_ANSWER_RESERVED_LEN, "Message 2 保留区")?
            .iter()
            .all(|byte| *byte == 0),
        "MediaStream Message 2 保留区必须为零"
    );
    let total_answer_len = answer_lengths.iter().try_fold(0usize, |sum, length| {
        sum.checked_add(*length)
            .context("Message 2 answer 长度溢出")
    })?;
    ensure!(
        cursor.remaining() == total_answer_len,
        "MediaStream Message 2 answer 长度与剩余载荷不一致"
    );
    let audio = parse_avconference_answer_container(
        cursor.take(answer_lengths[0], "audio answer")?,
        "audio",
    )?;
    let video_stream_1 = parse_avconference_answer_container(
        cursor.take(answer_lengths[1], "video stream 1 answer")?,
        "video stream 1",
    )?;
    let video_stream_2 = if answer_lengths[2] == 0 {
        None
    } else {
        Some(parse_avconference_answer_container(
            cursor.take(answer_lengths[2], "video stream 2 answer")?,
            "video stream 2",
        )?)
    };
    ensure!(
        cursor.remaining() == 0,
        "MediaStream Message 2 存在尾随数据"
    );
    Ok(MediaStreamAnswer {
        stream_1_supports_60_fps: flags & MEDIA_STREAM_ANSWER_STREAM_1_SUPPORTS_60_FPS != 0,
        stream_2_supports_60_fps: flags & MEDIA_STREAM_ANSWER_STREAM_2_SUPPORTS_60_FPS != 0,
        audio,
        video_stream_1,
        video_stream_2,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    const TEST_AUDIO_SSRC: u32 = 0x1020_3040;
    const TEST_VIDEO_SSRC: u32 = 0x5060_7080;

    fn decode_hex_fixture(hex: &str) -> Vec<u8> {
        let compact = hex.trim();
        assert!(compact.len().is_multiple_of(2));
        compact
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    fn material(key_byte: u8, salt_byte: u8) -> SrtpMasterMaterial {
        SrtpMasterMaterial {
            master_key: [key_byte; SRTP_AES_256_MASTER_KEY_LEN],
            master_salt: [salt_byte; SRTP_MASTER_SALT_LEN],
        }
    }

    fn entry(
        mode: MediaNegotiatorMode,
        ssrc: u32,
        send_byte: u8,
        receive_byte: u8,
    ) -> MediaStreamConfigurationEntry {
        MediaStreamConfigurationEntry {
            viewer_to_server: material(send_byte, send_byte),
            server_to_viewer: material(receive_byte, receive_byte),
            offer: build_media_negotiator_offer(mode, ssrc).unwrap(),
        }
    }

    fn captured_answer_container() -> Vec<u8> {
        let fixture =
            crate::vnc::read_private_fixture_text("ard_re/fixtures/avc_mode_4_answer.bplist.hex");
        decode_hex_fixture(&fixture)
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AVConference fixture"]
    fn parses_captured_apple_binary_plist_answer_container() {
        let fixture = captured_answer_container();

        let answer = parse_avconference_answer_container(&fixture, "audio").unwrap();

        assert!(answer.compressed.starts_with(&[0x78, 0xda]));
        assert!(!answer.decompressed.is_empty());
    }

    fn answer_fixture(include_video_2: bool) -> Vec<u8> {
        let audio = captured_answer_container();
        let video_1 = captured_answer_container();
        let video_2 = include_video_2
            .then(captured_answer_container)
            .unwrap_or_default();

        let mut body = Vec::new();
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&2u16.to_be_bytes());
        let flags = if include_video_2 { 0b11u32 } else { 0b01u32 };
        body.extend_from_slice(&flags.to_be_bytes());
        body.extend_from_slice(&(audio.len() as u16).to_be_bytes());
        body.extend_from_slice(&(video_1.len() as u16).to_be_bytes());
        body.extend_from_slice(&(video_2.len() as u16).to_be_bytes());
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&audio);
        body.extend_from_slice(&video_1);
        body.extend_from_slice(&video_2);

        let mut frame = Vec::new();
        frame.extend_from_slice(&1u32.to_be_bytes());
        frame.extend_from_slice(&[0; 8]);
        frame.extend_from_slice(&0x03f2i32.to_be_bytes());
        frame.extend_from_slice(&(body.len() as u16).to_be_bytes());
        frame.extend_from_slice(&body);
        frame
    }

    #[test]
    fn builds_current_binary_plist_offers_with_mode_specific_protobuf() {
        let audio =
            build_media_negotiator_offer(MediaNegotiatorMode::Audio, TEST_AUDIO_SSRC).unwrap();
        let remote_microphone =
            build_media_negotiator_offer(MediaNegotiatorMode::RemoteMicrophone, TEST_AUDIO_SSRC)
                .unwrap();
        let video =
            build_media_negotiator_offer(MediaNegotiatorMode::Video, TEST_VIDEO_SSRC).unwrap();

        if let Some(export_directory) = std::env::var_os("FRD_MEDIA_OFFER_EXPORT_DIR") {
            let export_directory = std::path::PathBuf::from(export_directory);
            std::fs::create_dir_all(&export_directory).unwrap();
            std::fs::write(
                export_directory.join("rust_audio_offer.bplist"),
                &audio.bytes,
            )
            .unwrap();
            std::fs::write(
                export_directory.join("rust_remote_microphone_offer.bplist"),
                &remote_microphone.bytes,
            )
            .unwrap();
            std::fs::write(
                export_directory.join("rust_video_offer.bplist"),
                &video.bytes,
            )
            .unwrap();
        }

        assert!(audio.bytes.starts_with(b"bplist00"));
        assert!(video.bytes.starts_with(b"bplist00"));
        assert_ne!(audio.bytes, video.bytes);
        for offer in [audio, remote_microphone, video] {
            let mut protobuf = Vec::new();
            ZlibDecoder::new(offer.compressed_media_blob.as_slice())
                .read_to_end(&mut protobuf)
                .unwrap();
            assert!(protobuf
                .windows(VICEROY_USER_AGENT.len())
                .any(|part| part == VICEROY_USER_AGENT));
        }
    }

    #[test]
    fn binary_plist_trailer_serialization_and_parse_boundaries_are_exact() {
        let bytes = binary_plist_offer(
            MediaNegotiatorMode::Audio,
            &[0x78, 0xda, 0x01],
            &[0x11, 0x22],
            "00112233-4455-6677-8899-aabbccddeeff",
        )
        .unwrap();
        let trailer_start = bytes.len() - 32;
        let trailer = &bytes[trailer_start..];

        assert_eq!(trailer.len(), 32);
        assert_eq!(&trailer[..6], &[0, 0, 0, 0, 0, 0]);
        assert_eq!(trailer[6], 1);
        assert_eq!(trailer[7], 1);
        assert_eq!(&trailer[8..16], &9u64.to_be_bytes());
        assert_eq!(&trailer[16..24], &0u64.to_be_bytes());
        let offset_table_offset =
            usize::try_from(u64::from_be_bytes(trailer[24..32].try_into().unwrap())).unwrap();
        assert_eq!(offset_table_offset + 9, trailer_start);

        let parsed = BinaryPlistReader::new(&bytes).unwrap();
        assert_eq!(parsed.offsets.len(), 9);
        assert_eq!(parsed.reference_size, 1);
        assert_eq!(parsed.root_object, 0);
        assert_eq!(parsed.offset_table_offset, offset_table_offset);

        let mut nonzero_reserved = bytes.clone();
        nonzero_reserved[trailer_start] = 1;
        assert!(BinaryPlistReader::new(&nonzero_reserved).is_err());

        let mut truncated = bytes;
        truncated.pop();
        assert!(BinaryPlistReader::new(&truncated).is_err());
    }

    #[test]
    fn remote_microphone_offer_carries_complete_apple_negotiation_profile() {
        let offer =
            build_media_negotiator_offer(MediaNegotiatorMode::RemoteMicrophone, TEST_AUDIO_SSRC)
                .unwrap();
        let mut protobuf = Vec::new();
        ZlibDecoder::new(offer.compressed_media_blob.as_slice())
            .read_to_end(&mut protobuf)
            .unwrap();

        let mut position = 0usize;
        let mut field_numbers = Vec::new();
        while position < protobuf.len() {
            let tag = parse_protobuf_varint(&protobuf, &mut position).unwrap();
            let field_number = tag >> 3;
            let wire_type = tag & 0x07;
            field_numbers.push(field_number);
            match wire_type {
                PROTOBUF_VARINT_WIRE_TYPE => {
                    parse_protobuf_varint(&protobuf, &mut position).unwrap();
                }
                PROTOBUF_LENGTH_DELIMITED_WIRE_TYPE => {
                    let length = parse_protobuf_varint(&protobuf, &mut position).unwrap() as usize;
                    position += length;
                }
                _ => panic!("测试 offer 包含意外 wire type {wire_type}"),
            }
        }

        assert!(field_numbers.contains(&u64::from(VICEROY_ACCESS_NETWORK_TYPE_FIELD)));
        assert_eq!(
            field_numbers
                .iter()
                .filter(|field| **field == u64::from(VICEROY_BANDWIDTH_SETTINGS_FIELD))
                .count(),
            SCREEN_SHARING_BANDWIDTH_SETTINGS.len()
        );
        assert!(field_numbers.contains(&u64::from(VICEROY_NTP_TIME_FIELD)));
        for key in [
            MEDIA_NEGOTIATOR_MODE_KEY,
            MEDIA_NEGOTIATOR_MEDIA_BLOB_KEY,
            MEDIA_STREAM_REMOTE_ENDPOINT_INFO_KEY,
            MEDIA_STREAM_CALL_ID_KEY,
        ] {
            assert!(offer
                .bytes
                .windows(key.len())
                .any(|window| window == key.as_bytes()));
        }
        assert_eq!(
            build_remote_endpoint_info(),
            b"\x08\x00\x10\x01\x1a\x08Mac16,10\x22\x08"
                .iter()
                .chain(b"2215.5.1\x2a\x0525G83")
                .copied()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn generated_configuration_uses_uuid_and_independent_directional_material() {
        let configuration = ClientMediaStreamConfiguration::generate_one_video().unwrap();

        assert_eq!(configuration.session_id[6] >> 4, 4);
        assert_eq!(configuration.session_id[8] >> 6, 2);
        assert_ne!(configuration.audio.offer.local_ssrc, 0);
        assert_ne!(configuration.video_stream_1.offer.local_ssrc, 0);
        assert_ne!(
            configuration.audio.viewer_to_server,
            configuration.audio.server_to_viewer
        );
        assert_ne!(
            configuration.audio.viewer_to_server,
            configuration.video_stream_1.viewer_to_server
        );
    }

    #[test]
    fn audio_flow_selects_the_named_system_audio_or_remote_microphone_mode() {
        let mac_to_pc = ClientMediaStreamConfiguration::generate_one_video_with_audio_flow(
            AudioMediaFlow::MacToPc,
        )
        .unwrap();
        let pc_to_mac = ClientMediaStreamConfiguration::generate_one_video_with_audio_flow(
            AudioMediaFlow::PcToMac,
        )
        .unwrap();

        assert_eq!(mac_to_pc.audio.offer.mode, MediaNegotiatorMode::Audio);
        assert_eq!(
            mac_to_pc.audio.offer.mode.wire_value(),
            SCREEN_SHARING_AUDIO_NEGOTIATOR_MODE
        );
        assert_eq!(
            pc_to_mac.audio.offer.mode,
            MediaNegotiatorMode::RemoteMicrophone
        );
        assert_eq!(
            pc_to_mac.audio.offer.mode.wire_value(),
            SCREEN_SHARING_REMOTE_MICROPHONE_NEGOTIATOR_MODE
        );
    }

    #[test]
    fn encodes_version_three_configuration_and_splits_directional_material() {
        let configuration = ClientMediaStreamConfiguration {
            session_id: [0x77; 16],
            audio: entry(MediaNegotiatorMode::Audio, TEST_AUDIO_SSRC, 0x11, 0x22),
            video_stream_1: entry(MediaNegotiatorMode::Video, TEST_VIDEO_SSRC, 0x33, 0x44),
            video_stream_2: None,
        };

        let frame = encode_client_media_stream_configuration(&configuration).unwrap();

        assert_eq!(frame[0], 0x1c);
        assert_eq!(
            u16::from_be_bytes(frame[2..4].try_into().unwrap()) as usize + 4,
            frame.len()
        );
        assert_eq!(&frame[4..6], &3u16.to_be_bytes());
        let audio_material_offset = 20 + 16;
        assert_eq!(
            &frame[audio_material_offset..audio_material_offset + 32],
            &[0x11; 32]
        );
        assert_eq!(
            &frame[audio_material_offset + 32..audio_material_offset + 46],
            &[0x11; 14]
        );
        assert_eq!(
            &frame[audio_material_offset + 46..audio_material_offset + 78],
            &[0x22; 32]
        );
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AVConference fixture"]
    fn parses_message_two_and_rejects_reserved_or_trailing_data() {
        let frame = answer_fixture(true);
        let answer = parse_media_stream_answer(&frame).unwrap();
        assert!(answer.stream_1_supports_60_fps);
        assert!(answer.stream_2_supports_60_fps);
        assert!(answer.video_stream_2.is_some());

        let mut reserved = frame.clone();
        reserved[32] = 1;
        assert!(parse_media_stream_answer(&reserved).is_err());

        let mut trailing = frame;
        trailing.push(0);
        assert!(parse_media_stream_answer(&trailing).is_err());
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AVConference fixture"]
    fn media_stream_answer_accepts_independent_shared_control_header_fixture() {
        let answer = parse_media_stream_answer(&answer_fixture(false)).unwrap();

        assert!(answer.stream_1_supports_60_fps);
        assert!(!answer.stream_2_supports_60_fps);
        assert!(answer.video_stream_2.is_none());
    }
}
