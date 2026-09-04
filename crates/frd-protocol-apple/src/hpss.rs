//! HPSS（高性能屏幕共享）会话层。
//!
//! 在类型 36 加密会话（session.rs）之上协商虚拟显示器并接收媒体流。
//! Apple 会话层消息自成帧（一条 EncryptOneMessage = 一条消息明文），
//! 因此全部走 `RfbConn::read_app_frame`（帧模式），与标准 RFB 的流式读取分离。
//!
//! 协商链路（docs/ARD_SESSION_PROTOCOL.md 第九章）：
//! - C→S 0x1d SetDisplayConfiguration：请求虚拟显示器（服务器回 1440×2560 ServerState）
//! - C↔S 0x08 与 ServerState 在现有样本中相关；其 subtype、帧率与预设/尺寸语义尚未证实
//! - C→S 0x09 显示信息 / 0x0d fence / 0x15 AutoPasteboard
//! - S→C 媒体矩形：[u32 stream=1][x][y][w][h][s32 enc][数据]
//!   - 0x3f3(1011) MVS 视频帧：[u32 总长][000f19 头][Huffman 表 | 系数流]
//!     首条 w=h=0 为表初始化；大帧按 0x8000 分块（总长跨块，需拼接）
//!   - 0x450(1104) 光标：[u32 0x3e8][u32 zlib 长][zlib 数据]
//!
//! 实时交互视图由平台层消费本模块发布的帧与输入边界。

use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};

use crate::connection::AppleConnection as RfbConn;
use crate::dynamic_resolution::DisplaySize;
use crate::media_negotiation::{self, MediaStreamAnswer};
use crate::media_protocol::{self, MediaStreamPortAnnouncement};
use crate::media_transport::{
    MediaDatagram, MediaDiscardCounters, MediaRole, MediaSocketReceiveBufferCapacity,
    MediaTransport, MediaTransportPhase,
};
use crate::mvs::{self, MvsRecordKind, MVS_FULL_FRAME_SIGNATURE};
use crate::mvs_stream::{MvsRecord, MvsRecordAssembler, MvsRect, MAX_MVS_RECORD_PAYLOAD};
use crate::protocol;
use crate::srtp::parse_rtp_header;

/// Apple 会话层编码（矩形头 s32）
pub mod encoding {
    /// MVS 视频帧（JPEG 类：首条为 Huffman/量化表初始化，之后为系数流）
    pub const MVS: i32 = 0x3f3;
    /// 光标
    pub const CURSOR: i32 = 0x450;
    /// ServerState（显示配置；与 0x08 相关的具体查询/应答语义未定）
    pub const SERVER_STATE: i32 = 0x451;
    pub const APPLE_STATE_MIN: i32 = 0x44c;
    pub const APPLE_STATE_MAX: i32 = 0x456;
}

pub mod cursor {
    pub const PAYLOAD_MAGIC: u32 = 0x03e8;
    pub const PAYLOAD_MAGIC_BYTES: usize = size_of::<u32>();
    pub const PAYLOAD_LENGTH_BYTES: usize = size_of::<u32>();
    pub const WIRE_HEADER_LEN: usize = PAYLOAD_MAGIC_BYTES + PAYLOAD_LENGTH_BYTES;
    pub const MAX_PAYLOAD_BYTES: usize = 4 * crate::protocol::limits::BINARY_MEBIBYTE_BYTES;
}

/// 会话层消息类型（帧明文首字节）
pub mod msg {
    pub const QUERY_08: u8 = 0x08;
    pub const FENCE: u8 = 0x0d;
    pub const AUTO_PASTEBOARD: u8 = 0x15;
}

/// HPSS 客户端显示控制消息类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum HpssClientMessageType {
    DisplayQuery = 0x09,
    SetDisplayConfiguration = 0x1d,
}

pub const SET_DISPLAY_CONFIGURATION_WIRE_BYTES: usize = 308;
pub const DISPLAY_NAME_WIRE_CAPACITY: usize = 40;
pub const DISPLAY_QUERY_WIRE_BYTES: usize = 16;

const SET_DISPLAY_CONFIGURATION_VERSION: u16 = 1;
const SET_DISPLAY_CONFIGURATION_RECORD_TAG: u8 = 0x30;
const SET_DISPLAY_CONFIGURATION_RECORD_COUNT: u16 = 1;
const SET_DISPLAY_CONFIGURATION_NAME_FIELD_COUNT: u16 = 1;
const SET_DISPLAY_CONFIGURATION_DEFAULT_FLAGS: u32 = 0;
const SET_DISPLAY_CONFIGURATION_NAME_FIELD_PRESENT: u8 = 1;
const SET_DISPLAY_CONFIGURATION_RESERVED_BYTE: u8 = 0;
const DISPLAY_NAME_WIRE_CAPACITY_FIELD: u8 = DISPLAY_NAME_WIRE_CAPACITY as u8;
const DISPLAY_QUERY_VERSION_BYTES: [u8; 3] = [0x00, 0x00, 0x01];
const DISPLAY_QUERY_RESERVED_FIELD: u32 = 0;
const DISPLAY_QUERY_RESERVED_FIELD_COUNT: usize = 2;
const HIGH_PERFORMANCE_DISPLAY_RECORD_BYTES: u16 = 0x0128;
const HIGH_PERFORMANCE_DISPLAY_NAME_BYTES: usize = 120;
const HIGH_PERFORMANCE_DISPLAY_WIDTH_MM_BITS: u32 = 0x43b8_ba2f;
const HIGH_PERFORMANCE_DISPLAY_HEIGHT_MM_BITS: u32 = 0x434f_d174;
const HIGH_PERFORMANCE_DISPLAY_MAX_PIXEL_WIDTH: u32 = 3840;
const HIGH_PERFORMANCE_DISPLAY_MAX_PIXEL_HEIGHT: u32 = 2160;
const HIGH_PERFORMANCE_DISPLAY_UNKNOWN_CONFIG: u32 = 7;
const HIGH_PERFORMANCE_DISPLAY_MODE_COUNT: u16 = 5;
const HIGH_PERFORMANCE_DISPLAY_MODE_BYTES: usize = 0x1c;
const HIGH_PERFORMANCE_DISPLAY_REFRESH_RATE_30_HZ_BITS: u64 = 0x403e_0000_0000_0000;
const HIGH_PERFORMANCE_DISPLAY_REFRESH_RATE_60_HZ_BITS: u64 = 0x404e_0000_0000_0000;

/// Apple High Performance `0x1d` 显示模式的显式刷新率档位。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HighPerformanceRefreshTier {
    /// 30.0 Hz 降级档位。
    Hz30,
    /// 60.0 Hz 首选档位。
    Hz60,
}

impl HighPerformanceRefreshTier {
    fn wire_bits(self) -> u64 {
        match self {
            Self::Hz30 => HIGH_PERFORMANCE_DISPLAY_REFRESH_RATE_30_HZ_BITS,
            Self::Hz60 => HIGH_PERFORMANCE_DISPLAY_REFRESH_RATE_60_HZ_BITS,
        }
    }
}

struct SetDisplayConfiguration<'a> {
    display_name: &'a str,
}

impl SetDisplayConfiguration<'_> {
    fn encode(&self) -> Vec<u8> {
        let mut name = [SET_DISPLAY_CONFIGURATION_RESERVED_BYTE; DISPLAY_NAME_WIRE_CAPACITY];
        let name_bytes = self.display_name.as_bytes();
        let copied_name_bytes = name_bytes.len().min(DISPLAY_NAME_WIRE_CAPACITY);
        name[..copied_name_bytes].copy_from_slice(&name_bytes[..copied_name_bytes]);

        let mut message = Vec::with_capacity(SET_DISPLAY_CONFIGURATION_WIRE_BYTES);
        message.push(HpssClientMessageType::SetDisplayConfiguration as u8);
        message.extend_from_slice(&SET_DISPLAY_CONFIGURATION_VERSION.to_be_bytes());
        message.push(SET_DISPLAY_CONFIGURATION_RECORD_TAG);
        message.extend_from_slice(&SET_DISPLAY_CONFIGURATION_RECORD_COUNT.to_be_bytes());
        message.extend_from_slice(&SET_DISPLAY_CONFIGURATION_NAME_FIELD_COUNT.to_be_bytes());
        message.extend_from_slice(&SET_DISPLAY_CONFIGURATION_DEFAULT_FLAGS.to_be_bytes());
        message.push(SET_DISPLAY_CONFIGURATION_NAME_FIELD_PRESENT);
        message.push(DISPLAY_NAME_WIRE_CAPACITY_FIELD);
        message.extend_from_slice(&name);
        message.resize(
            SET_DISPLAY_CONFIGURATION_WIRE_BYTES,
            SET_DISPLAY_CONFIGURATION_RESERVED_BYTE,
        );
        message
    }
}

#[derive(Clone, Copy)]
struct HighPerformanceDisplayMode {
    pixel_width: u32,
    pixel_height: u32,
    point_width: u32,
    point_height: u32,
}

impl HighPerformanceDisplayMode {
    fn encode_into(self, message: &mut Vec<u8>, refresh_tier: HighPerformanceRefreshTier) {
        message.extend_from_slice(&self.pixel_width.to_be_bytes());
        message.extend_from_slice(&self.pixel_height.to_be_bytes());
        message.extend_from_slice(&self.point_width.to_be_bytes());
        message.extend_from_slice(&self.point_height.to_be_bytes());
        message.extend_from_slice(&refresh_tier.wire_bits().to_be_bytes());
        message.extend_from_slice(&0u32.to_be_bytes());
    }
}

struct HighPerformanceSetDisplayConfiguration<'a> {
    display_name: &'a str,
    primary: DisplaySize,
    refresh_tier: HighPerformanceRefreshTier,
}

impl HighPerformanceSetDisplayConfiguration<'_> {
    fn encode(self) -> Vec<u8> {
        let mut display_name = [0u8; HIGH_PERFORMANCE_DISPLAY_NAME_BYTES];
        let copied_name_bytes = self
            .display_name
            .len()
            .min(HIGH_PERFORMANCE_DISPLAY_NAME_BYTES - 1);
        display_name[..copied_name_bytes]
            .copy_from_slice(&self.display_name.as_bytes()[..copied_name_bytes]);

        let primary_pixel_width = u32::from(self.primary.width);
        let primary_pixel_height = u32::from(self.primary.height);
        let modes = [
            HighPerformanceDisplayMode {
                pixel_width: primary_pixel_width,
                pixel_height: primary_pixel_height,
                point_width: primary_pixel_width,
                point_height: primary_pixel_height,
            },
            HighPerformanceDisplayMode {
                pixel_width: 2880,
                pixel_height: 1800,
                point_width: 1440,
                point_height: 900,
            },
            HighPerformanceDisplayMode {
                pixel_width: 3840,
                pixel_height: 2160,
                point_width: 1920,
                point_height: 1080,
            },
            HighPerformanceDisplayMode {
                pixel_width: 2880,
                pixel_height: 1620,
                point_width: 1440,
                point_height: 810,
            },
            HighPerformanceDisplayMode {
                pixel_width: 2624,
                pixel_height: 1696,
                point_width: 1312,
                point_height: 848,
            },
        ];

        let mut message = Vec::with_capacity(SET_DISPLAY_CONFIGURATION_WIRE_BYTES);
        message.push(HpssClientMessageType::SetDisplayConfiguration as u8);
        message.push(0);
        message.extend_from_slice(&0x0130u16.to_be_bytes());
        message.extend_from_slice(&1u16.to_be_bytes());
        message.extend_from_slice(&1u16.to_be_bytes());
        message.extend_from_slice(&0u32.to_be_bytes());
        message.extend_from_slice(&HIGH_PERFORMANCE_DISPLAY_RECORD_BYTES.to_be_bytes());
        message.extend_from_slice(&display_name);
        message.extend_from_slice(&0u32.to_be_bytes());
        message.extend_from_slice(&0u32.to_be_bytes());
        message.extend_from_slice(&HIGH_PERFORMANCE_DISPLAY_WIDTH_MM_BITS.to_be_bytes());
        message.extend_from_slice(&HIGH_PERFORMANCE_DISPLAY_HEIGHT_MM_BITS.to_be_bytes());
        message.extend_from_slice(&HIGH_PERFORMANCE_DISPLAY_MAX_PIXEL_WIDTH.to_be_bytes());
        message.extend_from_slice(&HIGH_PERFORMANCE_DISPLAY_MAX_PIXEL_HEIGHT.to_be_bytes());
        message.extend_from_slice(&0u16.to_be_bytes());
        message.extend_from_slice(&0u16.to_be_bytes());
        message.extend_from_slice(&HIGH_PERFORMANCE_DISPLAY_UNKNOWN_CONFIG.to_be_bytes());
        message.extend_from_slice(&HIGH_PERFORMANCE_DISPLAY_MODE_COUNT.to_be_bytes());
        for mode in modes {
            let mode_start = message.len();
            mode.encode_into(&mut message, self.refresh_tier);
            debug_assert_eq!(
                message.len() - mode_start,
                HIGH_PERFORMANCE_DISPLAY_MODE_BYTES
            );
        }
        debug_assert_eq!(message.len(), SET_DISPLAY_CONFIGURATION_WIRE_BYTES);
        message
    }
}

struct DisplayQuery {
    size: DisplaySize,
}

impl DisplayQuery {
    fn encode(self) -> Vec<u8> {
        let mut message = Vec::with_capacity(DISPLAY_QUERY_WIRE_BYTES);
        message.push(HpssClientMessageType::DisplayQuery as u8);
        message.extend_from_slice(&DISPLAY_QUERY_VERSION_BYTES);
        for _ in 0..DISPLAY_QUERY_RESERVED_FIELD_COUNT {
            message.extend_from_slice(&DISPLAY_QUERY_RESERVED_FIELD.to_be_bytes());
        }
        message.extend_from_slice(&self.size.width.to_be_bytes());
        message.extend_from_slice(&self.size.height.to_be_bytes());
        debug_assert_eq!(message.len(), DISPLAY_QUERY_WIRE_BYTES);
        message
    }
}

/// HPSS 运行统计
#[derive(Default, Debug)]
pub struct HpssStats {
    pub mvs_frames: usize,
    pub mvs_bytes: usize,
    pub mvs_chunks: usize,
    pub table_inits: usize,
    pub malformed_table_diagnostics: usize,
    pub cursor_frames: usize,
    pub state_messages: usize,
    pub media_port_announcements: usize,
    pub media_stream_answers: usize,
    pub authenticated_audio_rtp_packets: usize,
    pub authenticated_audio_rtp_payload_bytes: usize,
    pub authenticated_video_rtp_packets: usize,
    pub authenticated_video_rtp_payload_bytes: usize,
    pub authenticated_rtcp_packets: usize,
    pub media_discard_counters: MediaDiscardCounters,
    pub media_receive_buffer_capacities: Vec<MediaSocketReceiveBufferCapacity>,
    pub unknown: Vec<u8>,
}

/// HPSS 会话结果
pub struct HpssSession {
    pub display: Option<(u16, u16)>,
    pub stats: HpssStats,
    /// 服务器 Message 2 原始帧；仅供显式诊断落盘，不包含客户端 SRTP 主材料。
    pub media_answer_frame: Option<Vec<u8>>,
    /// 通过 SRTP 认证并解密的音频 RTP 诊断捕获。
    pub audio_rtp_capture: AudioRtpCapture,
    /// 通过 SRTP 认证并解密的视频 RTP 诊断捕获。
    pub video_rtp_capture: VideoRtpCapture,
}

const MVS_CAPTURE_MAGIC: &[u8; 8] = b"FRDMVS01";
const MVS_CAPTURE_RECTANGLE_FIELD_BYTES: usize = size_of::<u16>();
const MVS_CAPTURE_RECTANGLE_FIELD_COUNT: usize = 4;
const MVS_CAPTURE_PAYLOAD_LENGTH_BYTES: usize = size_of::<u32>();
const MVS_CAPTURE_RECORD_HEADER_LEN: usize = MVS_CAPTURE_RECTANGLE_FIELD_COUNT
    * MVS_CAPTURE_RECTANGLE_FIELD_BYTES
    + MVS_CAPTURE_PAYLOAD_LENGTH_BYTES;
const MAX_MALFORMED_TABLE_DIAGNOSTICS: usize = 1;
const AUDIO_RTP_CAPTURE_MAGIC: &[u8; 8] = b"FRDRTP01";
const AUDIO_RTP_CAPTURE_RECORD_LENGTH_BYTES: usize = size_of::<u32>();
const MAX_AUDIO_RTP_CAPTURE_BYTES: usize = 16 * crate::protocol::limits::BINARY_MEBIBYTE_BYTES;
const VIDEO_RTP_CAPTURE_MAGIC: &[u8; 8] = b"FRDVTP01";
const VIDEO_RTP_CAPTURE_ROLE_BYTES: usize = size_of::<u8>();
const VIDEO_RTP_CAPTURE_RECORD_LENGTH_BYTES: usize = size_of::<u32>();
const MAX_VIDEO_RTP_CAPTURE_BYTES: usize = 32 * crate::protocol::limits::BINARY_MEBIBYTE_BYTES;
const TCP_CONTROL_HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_millis(500);
const TCP_CONTROL_ACTIVE_READ_TIMEOUT: Duration = Duration::from_millis(5);
const UDP_MEDIA_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HpssPollRound {
    tcp_read_timeout_update: Option<Duration>,
    drain_udp: bool,
}

#[derive(Debug, Default)]
struct HpssPollScheduler {
    applied_tcp_read_timeout: Option<Duration>,
}

impl HpssPollScheduler {
    fn next_round(&mut self, media_phase: MediaTransportPhase) -> HpssPollRound {
        let drain_udp = media_phase == MediaTransportPhase::Active;
        let required_timeout = if drain_udp {
            TCP_CONTROL_ACTIVE_READ_TIMEOUT
        } else {
            TCP_CONTROL_HANDSHAKE_READ_TIMEOUT
        };
        let tcp_read_timeout_update =
            (self.applied_tcp_read_timeout != Some(required_timeout)).then_some(required_timeout);
        self.applied_tcp_read_timeout = Some(required_timeout);
        HpssPollRound {
            tcp_read_timeout_update,
            drain_udp,
        }
    }
}

#[derive(Debug)]
pub struct AudioRtpCapture {
    encoded: Vec<u8>,
    packet_count: usize,
}

impl Default for AudioRtpCapture {
    fn default() -> Self {
        Self {
            encoded: AUDIO_RTP_CAPTURE_MAGIC.to_vec(),
            packet_count: 0,
        }
    }
}

impl AudioRtpCapture {
    pub fn is_empty(&self) -> bool {
        self.packet_count == 0
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.encoded
    }

    fn push(&mut self, packet: &[u8]) -> Result<()> {
        let packet_len = u32::try_from(packet.len()).context("RTP 包长度超过 u32")?;
        let record_bytes = AUDIO_RTP_CAPTURE_RECORD_LENGTH_BYTES
            .checked_add(packet.len())
            .context("RTP 捕获记录长度溢出")?;
        let next_len = self
            .encoded
            .len()
            .checked_add(record_bytes)
            .context("RTP 捕获文件长度溢出")?;
        if next_len > MAX_AUDIO_RTP_CAPTURE_BYTES {
            bail!("RTP 捕获超过最大允许容量");
        }
        self.encoded.extend_from_slice(&packet_len.to_be_bytes());
        self.encoded.extend_from_slice(packet);
        self.packet_count += 1;
        Ok(())
    }
}

#[derive(Debug)]
pub struct VideoRtpCapture {
    encoded: Option<Vec<u8>>,
    packet_count: usize,
}

impl Default for VideoRtpCapture {
    fn default() -> Self {
        Self {
            encoded: None,
            packet_count: 0,
        }
    }
}

impl VideoRtpCapture {
    fn enabled() -> Self {
        Self {
            encoded: Some(VIDEO_RTP_CAPTURE_MAGIC.to_vec()),
            packet_count: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.packet_count == 0
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.encoded.as_deref().unwrap_or_default()
    }

    fn push(&mut self, role: MediaRole, packet: &[u8]) -> Result<()> {
        let Some(encoded) = self.encoded.as_mut() else {
            return Ok(());
        };
        let role = match role {
            MediaRole::VideoStream1 => 1,
            MediaRole::VideoStream2 => 2,
            MediaRole::Audio => bail!("Video RTP 捕获不支持的媒体角色"),
        };
        let packet_len = u32::try_from(packet.len()).context("Video RTP 包长度超过 u32")?;
        let record_bytes = VIDEO_RTP_CAPTURE_ROLE_BYTES
            .checked_add(VIDEO_RTP_CAPTURE_RECORD_LENGTH_BYTES)
            .and_then(|length| length.checked_add(packet.len()))
            .context("Video RTP 捕获记录长度溢出")?;
        let next_len = encoded
            .len()
            .checked_add(record_bytes)
            .context("Video RTP 捕获文件长度溢出")?;
        if next_len > MAX_VIDEO_RTP_CAPTURE_BYTES {
            bail!("Video RTP 捕获超过最大允许容量");
        }
        encoded.push(role);
        encoded.extend_from_slice(&packet_len.to_be_bytes());
        encoded.extend_from_slice(packet);
        self.packet_count += 1;
        Ok(())
    }
}

struct HpssWireCursor<'a> {
    remaining: &'a [u8],
}

impl<'a> HpssWireCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn remaining_len(&self) -> usize {
        self.remaining.len()
    }

    fn into_remaining(self) -> &'a [u8] {
        self.remaining
    }

    fn take(&mut self, count: usize, field: &str) -> Result<&'a [u8]> {
        if self.remaining.len() < count {
            bail!("{field} 截断");
        }
        let (value, remaining) = self.remaining.split_at(count);
        self.remaining = remaining;
        Ok(value)
    }

    fn read_u16(&mut self, field: &str) -> Result<u16> {
        Ok(u16::from_be_bytes(
            self.take(size_of::<u16>(), field)?.try_into()?,
        ))
    }

    fn read_u32(&mut self, field: &str) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(size_of::<u32>(), field)?.try_into()?,
        ))
    }

    fn read_i32(&mut self, field: &str) -> Result<i32> {
        Ok(i32::from_be_bytes(
            self.take(size_of::<i32>(), field)?.try_into()?,
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MvsCaptureRecordHeader {
    rect: MvsRect,
    payload_len: usize,
}

impl MvsCaptureRecordHeader {
    fn parse(data: &[u8]) -> Result<(Self, &[u8])> {
        if data.len() < MVS_CAPTURE_RECORD_HEADER_LEN {
            bail!("MVS 捕获记录头截断");
        }
        let mut cursor = HpssWireCursor::new(data);
        let rect = MvsRect {
            x: cursor.read_u16("MVS 捕获记录 x")?,
            y: cursor.read_u16("MVS 捕获记录 y")?,
            width: cursor.read_u16("MVS 捕获记录 width")?,
            height: cursor.read_u16("MVS 捕获记录 height")?,
        };
        let payload_len = usize::try_from(cursor.read_u32("MVS 捕获记录 payload 长度")?)
            .context("MVS 捕获记录 payload 长度无法表示为 usize")?;
        Ok((Self { rect, payload_len }, cursor.into_remaining()))
    }
}
/// Versioned geometry-aware MVS capture writer.
pub struct MvsCaptureWriter<'a> {
    sink: &'a mut dyn std::io::Write,
}

impl<'a> MvsCaptureWriter<'a> {
    pub fn new(sink: &'a mut dyn std::io::Write) -> Result<Self> {
        sink.write_all(MVS_CAPTURE_MAGIC)?;
        Ok(Self { sink })
    }

    pub fn write_record(&mut self, record: &MvsRecord) -> Result<()> {
        if record.payload.is_empty() {
            bail!("MVS 捕获记录 payload 不能为空");
        }
        if record.payload.len() > MAX_MVS_RECORD_PAYLOAD {
            bail!("MVS 捕获记录 payload 超过上限: {}", record.payload.len());
        }
        self.sink.write_all(&record.rect.x.to_be_bytes())?;
        self.sink.write_all(&record.rect.y.to_be_bytes())?;
        self.sink.write_all(&record.rect.width.to_be_bytes())?;
        self.sink.write_all(&record.rect.height.to_be_bytes())?;
        self.sink
            .write_all(&(record.payload.len() as u32).to_be_bytes())?;
        self.sink.write_all(&record.payload)?;
        Ok(())
    }
}

/// Decode only the current versioned geometry-aware MVS capture format.
/// Legacy length-prefixed files are intentionally rejected rather than guessed.
pub fn read_mvs_capture(data: &[u8]) -> Result<Vec<MvsRecord>> {
    if !data.starts_with(MVS_CAPTURE_MAGIC) {
        bail!("不支持 legacy/无版本 MVS 捕获格式（需要 FRDMVS01）");
    }

    let mut remaining = &data[MVS_CAPTURE_MAGIC.len()..];
    let mut records = Vec::new();
    while !remaining.is_empty() {
        let (header, after_header) = MvsCaptureRecordHeader::parse(remaining)?;
        if header.payload_len == 0 {
            bail!("MVS 捕获记录 payload 长度为零");
        }
        if header.payload_len > MAX_MVS_RECORD_PAYLOAD {
            bail!("MVS 捕获记录 payload 超过上限: {}", header.payload_len);
        }
        if after_header.len() < header.payload_len {
            bail!("MVS 捕获记录 payload 截断");
        }
        let (payload, next_record) = after_header.split_at(header.payload_len);
        records.push(MvsRecord {
            rect: header.rect,
            payload: payload.to_vec(),
        });
        remaining = next_record;
    }
    Ok(records)
}

/// Capture-side MVS transport state. Continuation application frames are opaque
/// and must be consumed before session-message classification.
#[derive(Default)]
struct HpssMvsCollector {
    assembler: MvsRecordAssembler,
}

impl HpssMvsCollector {
    fn begin(&mut self, rect: MvsRect, total: u32, first: &[u8]) -> Result<Option<MvsRecord>> {
        self.assembler.begin(rect, total, first)
    }

    fn push_continuation(&mut self, chunk: &[u8]) -> Result<Option<MvsRecord>> {
        self.assembler.push_continuation(chunk)
    }

    fn is_pending(&self) -> bool {
        self.assembler.is_pending()
    }
}

/// True only when the fixed media header identifies MVS but its declared-total
/// field is truncated.
pub fn is_truncated_mvs_envelope(m: &[u8]) -> bool {
    MediaRectangle::parse(m).is_ok_and(|rectangle| {
        rectangle.encoding == encoding::MVS && rectangle.payload.len() < MVS_DECLARED_TOTAL_BYTES
    })
}

fn record_complete_mvs(
    sess: &mut HpssSession,
    capture_writer: &mut Option<MvsCaptureWriter<'_>>,
    record: MvsRecord,
) -> Result<()> {
    if let Some(writer) = capture_writer.as_mut() {
        writer
            .write_record(&record)
            .context("写入完整 MVS 捕获记录失败")?;
    }
    sess.stats.mvs_frames += 1;
    sess.stats.mvs_bytes += record.payload.len();
    match mvs::classify_mvs_record(record.rect, &record.payload) {
        Ok(MvsRecordKind::Tables(_)) => {
            sess.stats.table_inits += 1;
            eprintln!("[hpss] MVS 表初始化 {}B", record.payload.len());
        }
        Ok(MvsRecordKind::Frame(_)) => {}
        Err(error) if sess.stats.malformed_table_diagnostics < MAX_MALFORMED_TABLE_DIAGNOSTICS => {
            sess.stats.malformed_table_diagnostics += 1;
            eprintln!("[hpss] MVS 畸形表初始化记录已保留: {error:#}");
        }
        Err(_) => {}
    }
    Ok(())
}

/// 构造 0x1d SetDisplayConfiguration（308B，真机抓包对齐）
pub fn build_set_display_config(display_name: &str) -> Vec<u8> {
    SetDisplayConfiguration { display_name }.encode()
}

/// 构造 Apple High Performance 初始虚拟显示使用的已恢复 0x1d wire。
pub fn build_high_performance_set_display_config(
    display_name: &str,
    primary: DisplaySize,
    refresh_tier: HighPerformanceRefreshTier,
) -> Vec<u8> {
    HighPerformanceSetDisplayConfiguration {
        display_name,
        primary,
        refresh_tier,
    }
    .encode()
}

/// 构造现有 HPSS 路径使用的 16 字节 0x09 显示尺寸查询。
pub fn build_display_query(size: DisplaySize) -> Vec<u8> {
    DisplayQuery { size }.encode()
}

const MEDIA_RECTANGLE_STREAM_ID_BYTES: usize = size_of::<u32>();
const MEDIA_RECTANGLE_COORDINATE_BYTES: usize = size_of::<u16>();
const MEDIA_RECTANGLE_COORDINATE_COUNT: usize = 4;
const MEDIA_RECTANGLE_ENCODING_BYTES: usize = size_of::<i32>();
const MEDIA_RECTANGLE_HEADER_LEN: usize = MEDIA_RECTANGLE_STREAM_ID_BYTES
    + MEDIA_RECTANGLE_COORDINATE_COUNT * MEDIA_RECTANGLE_COORDINATE_BYTES
    + MEDIA_RECTANGLE_ENCODING_BYTES;
const MVS_DECLARED_TOTAL_BYTES: usize = size_of::<u32>();
const SERVER_STATE_SIZE_FIELD_LEN: usize = size_of::<u16>();
const SERVER_STATE_BODY_PREFIX_LEN: usize = 20;
const SERVER_STATE_DISPLAY_RECORD_LEN: usize = 56;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MediaRectangle<'a> {
    stream_id: u32,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    encoding: i32,
    payload: &'a [u8],
}

impl<'a> MediaRectangle<'a> {
    fn parse(message: &'a [u8]) -> Result<Self> {
        if message.len() < MEDIA_RECTANGLE_HEADER_LEN {
            bail!("媒体矩形过短: {}", message.len());
        }
        let mut cursor = HpssWireCursor::new(message);
        Ok(Self {
            stream_id: cursor.read_u32("媒体流 ID")?,
            x: cursor.read_u16("媒体矩形 x")?,
            y: cursor.read_u16("媒体矩形 y")?,
            width: cursor.read_u16("媒体矩形 width")?,
            height: cursor.read_u16("媒体矩形 height")?,
            encoding: cursor.read_i32("媒体矩形 encoding")?,
            payload: cursor.into_remaining(),
        })
    }
}

pub fn mvs_envelope_candidate_rect(message: &[u8]) -> Option<MvsRect> {
    let rectangle = MediaRectangle::parse(message).ok()?;
    (rectangle.encoding == encoding::MVS).then_some(MvsRect {
        x: rectangle.x,
        y: rectangle.y,
        width: rectangle.width,
        height: rectangle.height,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerStateGeometry {
    pub message_version: u16,
    pub display_count: u16,
    pub width: u16,
    pub height: u16,
}

fn checked_server_state_body_len(display_count: usize) -> Result<usize> {
    // Apple 的固定前缀 20B 和每条 display record 56B 均已是 4B 倍数，
    // 因而 align4(20 + count * 56) 与下式严格等价，不需要隐式补齐。
    display_count
        .checked_mul(SERVER_STATE_DISPLAY_RECORD_LEN)
        .and_then(|records_len| SERVER_STATE_BODY_PREFIX_LEN.checked_add(records_len))
        .context("ServerState display records 长度溢出")
}

/// 严格解析固定媒体矩形中的 ServerState 活动 framebuffer 尺寸。
pub fn parse_server_state_geometry(message: &[u8]) -> Result<ServerStateGeometry> {
    let minimum_len =
        MEDIA_RECTANGLE_HEADER_LEN + SERVER_STATE_SIZE_FIELD_LEN + SERVER_STATE_BODY_PREFIX_LEN;
    if message.len() < minimum_len {
        bail!("ServerState 截断: {} 字节", message.len());
    }
    let rectangle = MediaRectangle::parse(message)?;
    if rectangle.stream_id != media_protocol::PRIMARY_MEDIA_STREAM_ID {
        bail!("ServerState 流 ID 非法: {}", rectangle.stream_id);
    }
    if [rectangle.x, rectangle.y, rectangle.width, rectangle.height]
        .into_iter()
        .any(|coordinate| coordinate != 0)
    {
        bail!("ServerState 必须使用零矩形");
    }
    if rectangle.encoding != encoding::SERVER_STATE {
        bail!("不是 ServerState 编码: {}", rectangle.encoding);
    }
    let mut payload = HpssWireCursor::new(rectangle.payload);
    let declared_len = usize::from(payload.read_u16("ServerState 声明长度")?);
    let actual_len = payload.remaining_len();
    if declared_len != actual_len {
        bail!("ServerState 声明长度 {declared_len} 与实际 {actual_len} 不一致");
    }
    let message_version = payload.read_u16("ServerState messageVersion")?;
    let _unclassified_width = payload.read_u16("ServerState 未分类几何组 width")?;
    let _unclassified_height = payload.read_u16("ServerState 未分类几何组 height")?;
    let width = payload.read_u16("ServerState 活动 framebuffer width")?;
    let height = payload.read_u16("ServerState 活动 framebuffer height")?;
    let _unclassified_flags = payload.read_u32("ServerState 未分类 flags")?;
    let _unclassified_sequence = payload.read_u32("ServerState 未分类 sequence")?;
    let display_count = payload.read_u16("ServerState display count")?;
    if display_count == 0 {
        bail!("ServerState display count 为零");
    }
    let expected_len = checked_server_state_body_len(usize::from(display_count))?;
    if declared_len < expected_len {
        bail!(
            "ServerState body 长度 {declared_len} 小于 display count {display_count} 要求的 {expected_len}"
        );
    }
    let pixel_count = usize::from(width)
        .checked_mul(usize::from(height))
        .context("ServerState framebuffer 像素数量溢出")?;
    if pixel_count == 0 || pixel_count > protocol::limits::MAX_FRAMEBUFFER_PIXELS {
        bail!("ServerState framebuffer 尺寸超过资源预算: {width}x{height}");
    }
    Ok(ServerStateGeometry {
        message_version,
        display_count,
        width,
        height,
    })
}

pub fn parse_server_state_w_h(message: &[u8]) -> Option<(u16, u16)> {
    let state = parse_server_state_geometry(message).ok()?;
    Some((state.width, state.height))
}

/// 已解析的媒体矩形
pub enum Media {
    /// 服务器 `0x3f2` MediaStream Message 1：UDP 媒体端口公告。
    PortAnnouncement(MediaStreamPortAnnouncement),
    /// 服务器 `0x3f2` MediaStream Message 2：AVConference answer。
    StreamAnswer(MediaStreamAnswer),
    /// MVS 块（x/y/w/h + [u32 总长][000f19 头 + 负载]）
    Mvs {
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        total: u32,
        body: Vec<u8>,
    },
    /// 光标 zlib
    Cursor {
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        zlib: Vec<u8>,
    },
    /// 会话状态族
    State(i32),
}

/// 解析一条服务器帧明文为媒体矩形（[u32 1][x][y][w][h][s32 enc][数据]）
pub fn parse_media(m: &[u8]) -> Result<Media> {
    let rectangle = MediaRectangle::parse(m)?;
    if rectangle.stream_id != media_protocol::PRIMARY_MEDIA_STREAM_ID {
        bail!("未知流 id {}", rectangle.stream_id);
    }
    let MediaRectangle {
        x,
        y,
        width: w,
        height: h,
        encoding: enc,
        payload,
        ..
    } = rectangle;
    match enc {
        media_protocol::MEDIA_STREAM_CONTROL_ENCODING => {
            match media_protocol::parse_media_stream_control_kind(payload)? {
                media_protocol::MediaStreamControlKind::PortAnnouncement => {
                    Ok(Media::PortAnnouncement(
                        media_protocol::parse_media_stream_port_announcement(m)?,
                    ))
                }
                media_protocol::MediaStreamControlKind::Answer => Ok(Media::StreamAnswer(
                    media_negotiation::parse_media_stream_answer(m)?,
                )),
            }
        }
        encoding::MVS => {
            if payload.len() < MVS_DECLARED_TOTAL_BYTES {
                bail!("MVS 帧缺总长");
            }
            let mut payload = HpssWireCursor::new(payload);
            let total = payload.read_u32("MVS 声明总长")?;
            Ok(Media::Mvs {
                x,
                y,
                w,
                h,
                total,
                body: payload.into_remaining().to_vec(),
            })
        }
        encoding::CURSOR => {
            // [u32 0x3e8][u32 zlib 长][zlib]
            if payload.len() < cursor::WIRE_HEADER_LEN {
                bail!("光标 payload 头截断");
            }
            let mut payload = HpssWireCursor::new(payload);
            let magic = payload.read_u32("光标 payload magic")?;
            if magic != cursor::PAYLOAD_MAGIC {
                bail!("光标 payload magic 非法: 0x{magic:08x}");
            }
            let declared_len = usize::try_from(payload.read_u32("光标 payload 长度")?)
                .context("光标 payload 长度无法表示为 usize")?;
            if declared_len > cursor::MAX_PAYLOAD_BYTES {
                bail!("光标 payload 超过资源预算: {declared_len}");
            }
            let actual_len = payload.remaining_len();
            if declared_len != actual_len {
                bail!("光标 payload 声明长度 {declared_len} 与实际 {actual_len} 不一致");
            }
            let zlib = payload.into_remaining().to_vec();
            Ok(Media::Cursor { x, y, w, h, zlib })
        }
        e => Ok(Media::State(e)),
    }
}

fn full_refresh_request(
    width: u16,
    height: u16,
) -> Result<[u8; protocol::FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES]> {
    protocol::msg_fb_update_request(false, 0, 0, width, height)
}

/// HPSS 主流程：0x1d 协商 → 媒体流接收（`seconds` 秒）。
/// 最小推流触发集（真机逐步排除定案）：0x1d + 0x09 显示信息查询 + 0x03 帧请求。
/// 0x09 是启动 MVS 捕获管道的关键——没有它服务器不开始编码。
pub fn run(
    conn: &mut RfbConn,
    display_name: &str,
    seconds: u64,
    init_w: u16,
    init_h: u16,
    sink: Option<&mut dyn std::io::Write>,
    capture_video_rtp: bool,
) -> Result<HpssSession> {
    let media_server_address = conn.peer_addr()?.ip();
    let media_bind_address = conn.local_addr()?.ip();
    let mut media_transport = MediaTransport::new(0, media_server_address);
    let mut capture_writer = sink
        .map(MvsCaptureWriter::new)
        .transpose()
        .context("写入 MVS 捕获文件头失败")?;
    // 等服务器完成会话状态迁移（cmd2 后 <600ms 发帧会被断连）
    std::thread::sleep(Duration::from_millis(200));

    let virtual_display_active = init_h > init_w;
    conn.write_all(&build_set_display_config(display_name))
        .context("发送 0x1d SetDisplayConfiguration 失败")?;
    if virtual_display_active {
        eprintln!("[hpss] 服务器已挂虚拟显示器（{init_w}x{init_h}），重发 0x1d");
    }
    std::thread::sleep(Duration::from_millis(150));

    let w = if virtual_display_active { init_w } else { 1440 };
    let h = if virtual_display_active { init_h } else { 2560 };
    let q09 = build_display_query(DisplaySize::new(w, h).context("HPSS 初始显示尺寸无效")?);
    let fb_req = full_refresh_request(w, h)?;
    for q in [&q09[..], &fb_req[..]] {
        conn.write_all(q)?;
        std::thread::sleep(Duration::from_millis(120));
    }
    eprintln!("[hpss] 已发 0x09 + 0x03（推流触发集）");

    let mut sess = HpssSession {
        display: if virtual_display_active {
            Some((init_w, init_h))
        } else {
            None
        },
        stats: HpssStats::default(),
        media_answer_frame: None,
        audio_rtp_capture: AudioRtpCapture::default(),
        video_rtp_capture: if capture_video_rtp {
            VideoRtpCapture::enabled()
        } else {
            VideoRtpCapture::default()
        },
    };
    let mut collector = HpssMvsCollector::default();
    let mut poll_scheduler = HpssPollScheduler::default();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut last_full_request = Instant::now();
    let mut seen_full_frame = false;
    let mut tcp_control_open = true;

    while Instant::now() < deadline {
        let media_phase = media_transport.phase();
        let poll_round = poll_scheduler.next_round(media_phase);
        if let Some(read_timeout) = poll_round.tcp_read_timeout_update {
            conn.set_read_timeout(Some(read_timeout))?;
        }
        if poll_round.drain_udp {
            media_transport
                .service_control_reports_at(0, Instant::now())
                .context("发送周期 SRTCP 控制报告失败")?;
        }
        let accepted_udp = if poll_round.drain_udp {
            receive_one_udp_round(&mut media_transport, &mut sess)?
        } else {
            0
        };
        if !tcp_control_open {
            if accepted_udp == 0 {
                std::thread::sleep(UDP_MEDIA_IDLE_POLL_INTERVAL);
            }
            continue;
        }
        let m = match conn.read_app_frame() {
            Ok(m) => m,
            Err(e) if crate::connection::is_timeout(&e) => continue,
            Err(e)
                if crate::connection::is_peer_closed(&e)
                    && media_transport.phase() == MediaTransportPhase::Active =>
            {
                tcp_control_open = false;
                eprintln!("[hpss] TCP 控制连接已结束；继续接收 UDP 媒体数据面");
                continue;
            }
            Err(e) => return Err(e.context("HPSS 流读取失败")),
        };
        // Continuation application frames have no media header. Consume them
        // before checking for heartbeat/query bytes so their first payload byte
        // cannot change transport ordering.
        if collector.is_pending() {
            match collector.push_continuation(&m) {
                Ok(Some(record)) => record_complete_mvs(&mut sess, &mut capture_writer, record)?,
                Ok(None) => {}
                Err(e) => {
                    eprintln!("[hpss] MVS continuation 结构错误，丢弃记录: {e:#}");
                }
            }
            continue;
        }

        let first = m.first().copied().unwrap_or(0);
        match first {
            // 服务端单向保活通知；回写 0x14 会被当作未知客户端命令并断开。
            value if value == protocol::apple_session::SERVER_KEEPALIVE_MESSAGE_TYPE => {}
            msg::QUERY_08 => {
                // 0x08 的 subtype/数值语义未定；当前样本路径无需客户端动作。
            }
            value
                if value == protocol::RfbClientMessageType::FramebufferUpdateRequest as u8
                    || value == msg::FENCE
                    || value == msg::AUTO_PASTEBOARD => {}
            _ => match parse_media(&m) {
                Ok(Media::Mvs {
                    x,
                    y,
                    w,
                    h,
                    total,
                    body,
                }) => {
                    // 检测全量帧（000f19 头）
                    if body.starts_with(&MVS_FULL_FRAME_SIGNATURE) {
                        seen_full_frame = true;
                    }
                    // 定期重发非增量请求（触发服务器发全量 I 帧）
                    if !seen_full_frame && last_full_request.elapsed() > Duration::from_millis(1500)
                    {
                        last_full_request = Instant::now();
                        let refresh = full_refresh_request(w, h)?;
                        conn.write_all(&refresh)
                            .context("发送 MVS 全量刷新请求失败")?;
                    }
                    let rect = MvsRect {
                        x,
                        y,
                        width: w,
                        height: h,
                    };
                    match collector.begin(rect, total, &body) {
                        Ok(Some(record)) => {
                            record_complete_mvs(&mut sess, &mut capture_writer, record)?
                        }
                        Ok(None) => sess.stats.mvs_chunks += 1,
                        Err(e) => eprintln!("[hpss] MVS 首片结构错误，丢弃记录: {e:#}"),
                    }
                }
                Ok(Media::PortAnnouncement(announcement)) => {
                    sess.stats.media_port_announcements += 1;
                    eprintln!(
                        "[hpss] UDP 端口公告: audio={} announced={} hdr={}, video1={} announced={} hdr={}, video2={} announced={} hdr={}, flags=0x{:08x}",
                        announcement.audio.port,
                        announcement.audio.is_announced(),
                        announcement.audio.flags.hdr(),
                        announcement.video_stream_1.port,
                        announcement.video_stream_1.is_announced(),
                        announcement.video_stream_1.flags.hdr(),
                        announcement.video_stream_2.port,
                        announcement.video_stream_2.is_announced(),
                        announcement.video_stream_2.flags.hdr(),
                        announcement.message_flags,
                    );
                    if media_transport.phase() == MediaTransportPhase::Idle {
                        media_transport.accept_port_announcement(0, announcement)?;
                        media_transport.bind_local_sockets(0, media_bind_address)?;
                        for role in [
                            MediaRole::Audio,
                            MediaRole::VideoStream1,
                            MediaRole::VideoStream2,
                        ] {
                            if let Ok(local) = media_transport.local_addr(0, role) {
                                eprintln!("[hpss] {role:?} 本地 UDP socket 已绑定 {local}");
                            }
                        }
                        let configuration = media_transport.prepare_configuration(0)?;
                        conn.write_all(&configuration)
                            .context("发送 0x1c 媒体配置失败")?;
                        media_transport.mark_configuration_sent(0)?;
                        eprintln!(
                            "[hpss] 已发送经验证的 0x1c 媒体配置（{}B）",
                            configuration.len()
                        );
                    }
                }
                Ok(Media::StreamAnswer(answer)) => {
                    sess.media_answer_frame = Some(m.clone());
                    media_transport.accept_answer(0, answer)?;
                    media_transport.activate(0)?;
                    sess.stats.media_stream_answers += 1;
                    eprintln!(
                        "[hpss] 已验证 MediaStream Message 2；SRTP 数据面已激活并发送初始 SRTCP 报告"
                    );
                }
                Ok(Media::Cursor { x, y, w, h, zlib }) => {
                    let _validated_cursor_metadata = (x, y, w, h, zlib.len());
                    sess.stats.cursor_frames += 1;
                }
                Ok(Media::State(encoding::SERVER_STATE)) => {
                    sess.stats.state_messages += 1;
                    if sess.display.is_none() {
                        if let Some((w, h)) = parse_server_state_w_h(&m) {
                            sess.display = Some((w, h));
                            eprintln!("[hpss] 虚拟显示器确认 {w}x{h}");
                        }
                    }
                }
                Ok(Media::State(_)) => {
                    sess.stats.state_messages += 1;
                }
                Err(_) => {
                    if !sess.stats.unknown.contains(&first) {
                        sess.stats.unknown.push(first);
                    }
                }
            },
        }
    }
    snapshot_media_transport_diagnostics(&mut sess, &media_transport)?;
    media_transport.close(0)?;
    Ok(sess)
}

fn snapshot_media_transport_diagnostics(
    session: &mut HpssSession,
    media_transport: &MediaTransport,
) -> Result<()> {
    session.stats.media_discard_counters = media_transport.discard_counters();
    session.stats.media_receive_buffer_capacities = media_transport.receive_buffer_capacities(0)?;
    let counters = session.stats.media_discard_counters;
    eprintln!(
        "[hpss] UDP 传输丢弃统计：unexpected_source={} empty={} truncated={} malformed={} auth_failed={} replay_or_too_old={}",
        counters.unexpected_source,
        counters.empty_datagram,
        counters.truncated_header,
        counters.malformed_packet,
        counters.authentication_failed,
        counters.replay_or_too_old
    );
    Ok(())
}

fn receive_one_udp_round(
    media_transport: &mut MediaTransport,
    session: &mut HpssSession,
) -> Result<usize> {
    let summary = media_transport.drain_receive_round(0, |role, datagram| {
        record_authenticated_datagram(session, role, datagram)
    })?;
    Ok(summary.accepted_total)
}

fn record_authenticated_datagram(
    session: &mut HpssSession,
    role: MediaRole,
    datagram: MediaDatagram,
) -> Result<()> {
    match datagram {
        MediaDatagram::Rtcp(_) => {
            session.stats.authenticated_rtcp_packets += 1;
        }
        MediaDatagram::Rtp(packet) => {
            let header = parse_rtp_header(&packet).context("解析已认证 RTP 头失败")?;
            let payload_bytes = packet.len() - header.payload_offset;
            match role {
                MediaRole::Audio => {
                    session.stats.authenticated_audio_rtp_packets += 1;
                    session.stats.authenticated_audio_rtp_payload_bytes += payload_bytes;
                    session.audio_rtp_capture.push(&packet)?;
                }
                MediaRole::VideoStream1 | MediaRole::VideoStream2 => {
                    session.stats.authenticated_video_rtp_packets += 1;
                    session.stats.authenticated_video_rtp_payload_bytes += payload_bytes;
                    session.video_rtp_capture.push(role, &packet)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) const CAPTURED_SERVER_STATE_WITH_ACTIVE_FRAMEBUFFER: [u8; 94] = [
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x51,
    0x00, 0x4c, 0x00, 0x05, 0x05, 0xa0, 0x0a, 0x00, 0x05, 0x33, 0x09, 0x3d, 0xff, 0xff, 0xff, 0xff,
    0x00, 0x00, 0x00, 0x06, 0x00, 0x01, 0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f, 0xed,
    0x91, 0x11, 0x11, 0x11, 0x11, 0x11, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x00,
    0x05, 0xa0, 0x00, 0x00, 0x00, 0x00, 0x09, 0x3d, 0x05, 0x33, 0x00, 0x00, 0x00, 0x01, 0x20, 0x20,
    0x00, 0x01, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x10, 0x08, 0x00, 0x00, 0x00, 0x00,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_negotiation::{
        CompressedProtobufAnswer, SrtpMasterMaterial, SRTP_AES_256_MASTER_KEY_LEN,
        SRTP_MASTER_MATERIAL_LEN, SRTP_MASTER_SALT_LEN,
    };
    use crate::media_protocol::parse_media_stream_port_announcement;
    use crate::srtp::{
        build_compound_rtcp_receiver_report, derive_session_keys, protect_rtp_packet, SrtcpSender,
        SrtpPacketKind,
    };
    use std::net::{IpAddr, Ipv4Addr, UdpSocket};

    struct FailAfterFirstWrite {
        writes: usize,
    }

    struct ReceiveOneUdpRoundLoopback {
        transport: MediaTransport,
        remote: UdpSocket,
        local: std::net::SocketAddr,
        incoming_material: SrtpMasterMaterial,
    }

    impl ReceiveOneUdpRoundLoopback {
        fn new() -> Self {
            let remote = crate::bind_test_udp_loopback();
            remote
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let remote_port = remote.local_addr().unwrap().port();
            let announcement =
                parse_media_stream_port_announcement(&audio_port_announcement_fixture(remote_port))
                    .unwrap();
            let mut transport = MediaTransport::new(0, IpAddr::V4(Ipv4Addr::LOCALHOST));
            transport.accept_port_announcement(0, announcement).unwrap();
            transport
                .bind_test_loopback_sockets(0, &[(MediaRole::Audio, &remote)])
                .unwrap();
            let configuration = transport.prepare_configuration(0).unwrap();
            let incoming_material =
                incoming_material_from_configuration(&configuration, MediaRole::Audio);
            transport.mark_configuration_sent(0).unwrap();
            transport
                .accept_answer(0, loopback_answer_fixture())
                .unwrap();
            transport.activate(0).unwrap();
            let local = transport.local_addr(0, MediaRole::Audio).unwrap();
            let mut initial_report = [0u8; 128];
            remote.recv_from(&mut initial_report).unwrap();
            Self {
                transport,
                remote,
                local,
                incoming_material,
            }
        }

        fn send_discarded_traffic(&self) {
            let attacker = crate::bind_test_udp_loopback();
            attacker.send_to(b"unexpected-source", self.local).unwrap();
            self.remote.send_to(&[], self.local).unwrap();
        }

        fn send_rtp(&self, sequence: u16, payload: &[u8]) -> Vec<u8> {
            let plaintext = test_rtp_packet(sequence, payload);
            let keys = derive_session_keys(&self.incoming_material, SrtpPacketKind::Rtp);
            let protected = protect_rtp_packet(&plaintext, &keys, 0).unwrap();
            self.remote.send_to(&protected, self.local).unwrap();
            plaintext
        }

        fn send_rtcp(&self) -> Vec<u8> {
            let plaintext = build_compound_rtcp_receiver_report(0x5566_7788);
            let keys = derive_session_keys(&self.incoming_material, SrtpPacketKind::Rtcp);
            let protected = SrtcpSender::new(keys).protect(&plaintext).unwrap();
            self.remote.send_to(&protected, self.local).unwrap();
            plaintext
        }
    }

    fn audio_port_announcement_fixture(port: u16) -> [u8; 54] {
        let mut frame = [0u8; 54];
        frame[0..4].copy_from_slice(&1u32.to_be_bytes());
        frame[12..16].copy_from_slice(&0x03f2i32.to_be_bytes());
        frame[16..18].copy_from_slice(&36u16.to_be_bytes());
        frame[18..20].copy_from_slice(&1u16.to_be_bytes());
        frame[20..22].copy_from_slice(&1u16.to_be_bytes());
        frame[26..28].copy_from_slice(&port.to_be_bytes());
        frame[28..32].copy_from_slice(&1u32.to_be_bytes());
        frame
    }

    fn loopback_answer_fixture() -> MediaStreamAnswer {
        let opaque = CompressedProtobufAnswer {
            compressed: vec![1],
            decompressed: vec![1],
        };
        MediaStreamAnswer {
            stream_1_supports_60_fps: true,
            stream_2_supports_60_fps: false,
            audio: opaque.clone(),
            video_stream_1: opaque,
            video_stream_2: None,
        }
    }

    fn incoming_material_from_configuration(
        configuration: &[u8],
        role: MediaRole,
    ) -> SrtpMasterMaterial {
        const CONFIGURATION_ENTRIES_OFFSET: usize = 36;
        const AUDIO_OFFER_LENGTH_OFFSET: usize = 10;
        let audio_offer_len = usize::from(u16::from_be_bytes(
            configuration[AUDIO_OFFER_LENGTH_OFFSET..AUDIO_OFFER_LENGTH_OFFSET + 2]
                .try_into()
                .unwrap(),
        ));
        let entry_offset = match role {
            MediaRole::Audio => CONFIGURATION_ENTRIES_OFFSET,
            MediaRole::VideoStream1 => {
                CONFIGURATION_ENTRIES_OFFSET + SRTP_MASTER_MATERIAL_LEN * 2 + audio_offer_len
            }
            MediaRole::VideoStream2 => panic!("测试配置没有第二视频流"),
        };
        let incoming_offset = entry_offset + SRTP_MASTER_MATERIAL_LEN;
        let key_end = incoming_offset + SRTP_AES_256_MASTER_KEY_LEN;
        let salt_end = key_end + SRTP_MASTER_SALT_LEN;
        SrtpMasterMaterial {
            master_key: configuration[incoming_offset..key_end].try_into().unwrap(),
            master_salt: configuration[key_end..salt_end].try_into().unwrap(),
        }
    }

    fn test_rtp_packet(sequence: u16, payload: &[u8]) -> Vec<u8> {
        const RTP_VERSION_2: u8 = 0x80;
        const SCREEN_SHARING_AUDIO_PAYLOAD_TYPE: u8 = 101;
        let mut packet = vec![RTP_VERSION_2, SCREEN_SHARING_AUDIO_PAYLOAD_TYPE];
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&960u32.to_be_bytes());
        packet.extend_from_slice(&0x5566_7788u32.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    fn empty_hpss_session() -> HpssSession {
        HpssSession {
            display: None,
            stats: HpssStats::default(),
            media_answer_frame: None,
            audio_rtp_capture: AudioRtpCapture::default(),
            video_rtp_capture: VideoRtpCapture::default(),
        }
    }

    #[test]
    fn media_activation_switches_to_fair_udp_polling_every_round() {
        let mut scheduler = HpssPollScheduler::default();

        let handshake = scheduler.next_round(MediaTransportPhase::ConfigSent);
        assert!(!handshake.drain_udp);
        assert_eq!(
            handshake.tcp_read_timeout_update,
            Some(TCP_CONTROL_HANDSHAKE_READ_TIMEOUT)
        );

        let first_active = scheduler.next_round(MediaTransportPhase::Active);
        assert!(first_active.drain_udp);
        assert_eq!(
            first_active.tcp_read_timeout_update,
            Some(TCP_CONTROL_ACTIVE_READ_TIMEOUT)
        );
        assert!(TCP_CONTROL_ACTIVE_READ_TIMEOUT <= UDP_MEDIA_IDLE_POLL_INTERVAL);

        let next_active = scheduler.next_round(MediaTransportPhase::Active);
        assert!(next_active.drain_udp);
        assert_eq!(next_active.tcp_read_timeout_update, None);
    }

    #[test]
    fn receive_one_udp_round_counts_accepted_and_mutates_session() {
        let mut loopback = ReceiveOneUdpRoundLoopback::new();
        loopback.send_discarded_traffic();
        let rtp = loopback.send_rtp(1, b"captured");
        loopback.send_rtcp();
        let mut session = empty_hpss_session();

        let accepted = receive_one_udp_round(&mut loopback.transport, &mut session).unwrap();

        assert_eq!(accepted, 2);
        assert_eq!(session.stats.authenticated_audio_rtp_packets, 1);
        assert_eq!(session.stats.authenticated_audio_rtp_payload_bytes, 8);
        assert_eq!(session.stats.authenticated_rtcp_packets, 1);
        let mut expected_capture = AUDIO_RTP_CAPTURE_MAGIC.to_vec();
        expected_capture.extend_from_slice(&(rtp.len() as u32).to_be_bytes());
        expected_capture.extend_from_slice(&rtp);
        assert_eq!(session.audio_rtp_capture.as_bytes(), expected_capture);
        let counters = loopback.transport.discard_counters();
        assert_eq!(counters.unexpected_source, 1);
        assert_eq!(counters.empty_datagram, 1);
        snapshot_media_transport_diagnostics(&mut session, &loopback.transport).unwrap();
        assert_eq!(session.stats.media_discard_counters, counters);
        let capacities = &session.stats.media_receive_buffer_capacities;
        assert_eq!(capacities.len(), 1);
        assert_eq!(capacities[0].role, MediaRole::Audio);
        assert!(capacities[0].actual_bytes > 0);
    }

    #[test]
    fn receive_one_udp_round_propagates_accepted_handler_error() {
        let mut loopback = ReceiveOneUdpRoundLoopback::new();
        loopback.send_rtp(1, b"capture-overflow");
        let mut session = empty_hpss_session();
        session.audio_rtp_capture = AudioRtpCapture {
            encoded: vec![0; MAX_AUDIO_RTP_CAPTURE_BYTES],
            packet_count: 0,
        };

        let error = receive_one_udp_round(&mut loopback.transport, &mut session).unwrap_err();

        assert!(format!("{error:#}").contains("RTP 捕获超过最大允许容量"));
    }

    #[test]
    fn parse_media_dispatches_verified_port_announcement() {
        let fixture: [u8; 54] = [
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x03, 0xf2, 0x00, 0x24, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x45, 0x67,
            0x00, 0x00, 0x00, 0x01, 0x45, 0x68, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let Media::PortAnnouncement(announcement) = parse_media(&fixture).unwrap() else {
            panic!("0x3f2 必须进入 typed Message 1 解析器");
        };
        assert_eq!(announcement.audio.port, 0x4567);
        assert!(announcement.audio.is_announced());
        assert_eq!(announcement.video_stream_1.port, 0x4568);
    }

    #[test]
    fn parse_media_fixed_header_preserves_fields_and_payload() {
        let fixture = [
            0x00, 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x04, 0x05, 0x06, 0x07, 0x00, 0x00,
            0x03, 0xf3, 0xaa, 0xbb,
        ];

        let rectangle = MediaRectangle::parse(&fixture).unwrap();

        assert_eq!(rectangle.stream_id, 1);
        assert_eq!(rectangle.x, 2);
        assert_eq!(rectangle.y, 3);
        assert_eq!(rectangle.width, 0x0405);
        assert_eq!(rectangle.height, 0x0607);
        assert_eq!(rectangle.encoding, 0x03f3);
        assert_eq!(rectangle.payload, &[0xaa, 0xbb]);
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AVConference fixture"]
    fn parse_media_dispatches_verified_message_two_answer() {
        fn captured_answer_container() -> Vec<u8> {
            let fixture =
                crate::read_private_fixture_text("ard_re/fixtures/avc_mode_4_answer.bplist.hex");
            let compact = fixture.trim();
            compact
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        }

        let audio = captured_answer_container();
        let video = captured_answer_container();
        let mut body = Vec::new();
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&(audio.len() as u16).to_be_bytes());
        body.extend_from_slice(&(video.len() as u16).to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&audio);
        body.extend_from_slice(&video);
        let mut fixture = Vec::new();
        fixture.extend_from_slice(&1u32.to_be_bytes());
        fixture.extend_from_slice(&[0; 8]);
        fixture.extend_from_slice(&media_protocol::MEDIA_STREAM_CONTROL_ENCODING.to_be_bytes());
        fixture.extend_from_slice(&(body.len() as u16).to_be_bytes());
        fixture.extend_from_slice(&body);

        let Media::StreamAnswer(answer) = parse_media(&fixture).unwrap() else {
            panic!("0x3f2 Message 2 必须进入 typed answer 解析器");
        };
        assert!(answer.stream_1_supports_60_fps);
        assert!(!answer.stream_2_supports_60_fps);
    }

    impl std::io::Write for FailAfterFirstWrite {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.writes > 0 {
                return Err(std::io::Error::other("injected capture failure"));
            }
            self.writes += 1;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn complete_mvs_record_propagates_capture_sink_failure() {
        let mut sink = FailAfterFirstWrite { writes: 0 };
        let writer = MvsCaptureWriter::new(&mut sink).unwrap();
        let mut writer = Some(writer);
        let mut session = HpssSession {
            display: None,
            stats: HpssStats::default(),
            media_answer_frame: None,
            audio_rtp_capture: AudioRtpCapture::default(),
            video_rtp_capture: VideoRtpCapture::default(),
        };
        let record = MvsRecord {
            rect: MvsRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            payload: vec![1],
        };

        assert!(record_complete_mvs(&mut session, &mut writer, record).is_err());
    }

    #[test]
    fn authenticated_audio_rtp_records_payload_and_versioned_capture() {
        const RTP_VERSION_2: u8 = 0x80;
        const SCREEN_SHARING_AUDIO_PAYLOAD_TYPE: u8 = 101;
        const RTP_FIXED_HEADER_BYTES: usize = 12;
        let mut first = vec![RTP_VERSION_2, SCREEN_SHARING_AUDIO_PAYLOAD_TYPE];
        first.extend_from_slice(&1u16.to_be_bytes());
        first.extend_from_slice(&2u32.to_be_bytes());
        first.extend_from_slice(&3u32.to_be_bytes());
        first.extend_from_slice(b"first");
        let mut second = first[..RTP_FIXED_HEADER_BYTES].to_vec();
        second.extend_from_slice(b"second");
        let mut session = HpssSession {
            display: None,
            stats: HpssStats::default(),
            media_answer_frame: None,
            audio_rtp_capture: AudioRtpCapture::default(),
            video_rtp_capture: VideoRtpCapture::default(),
        };

        record_authenticated_datagram(
            &mut session,
            MediaRole::Audio,
            MediaDatagram::Rtp(first.clone()),
        )
        .unwrap();
        record_authenticated_datagram(&mut session, MediaRole::Audio, MediaDatagram::Rtp(second))
            .unwrap();

        assert_eq!(session.stats.authenticated_audio_rtp_packets, 2);
        assert_eq!(session.stats.authenticated_audio_rtp_payload_bytes, 11);
        let mut expected_capture = AUDIO_RTP_CAPTURE_MAGIC.to_vec();
        expected_capture.extend_from_slice(&(first.len() as u32).to_be_bytes());
        expected_capture.extend_from_slice(&first);
        expected_capture.extend_from_slice(&(RTP_FIXED_HEADER_BYTES as u32 + 6).to_be_bytes());
        expected_capture.extend_from_slice(&[
            RTP_VERSION_2,
            SCREEN_SHARING_AUDIO_PAYLOAD_TYPE,
            0,
            1,
            0,
            0,
            0,
            2,
            0,
            0,
            0,
            3,
        ]);
        expected_capture.extend_from_slice(b"second");
        assert_eq!(session.audio_rtp_capture.as_bytes(), expected_capture);
    }

    #[test]
    fn video_rtp_capture_uses_role_tagged_versioned_format() {
        let stream_one_packet = test_rtp_packet(1, b"stream-one");
        let stream_two_packet = test_rtp_packet(2, b"stream-two");
        let mut capture = VideoRtpCapture::enabled();

        capture
            .push(MediaRole::VideoStream1, &stream_one_packet)
            .unwrap();
        capture
            .push(MediaRole::VideoStream2, &stream_two_packet)
            .unwrap();

        let mut expected = b"FRDVTP01".to_vec();
        expected.push(1);
        expected.extend_from_slice(&(stream_one_packet.len() as u32).to_be_bytes());
        expected.extend_from_slice(&stream_one_packet);
        expected.push(2);
        expected.extend_from_slice(&(stream_two_packet.len() as u32).to_be_bytes());
        expected.extend_from_slice(&stream_two_packet);
        assert_eq!(capture.as_bytes(), expected);
    }

    #[test]
    fn video_rtp_capture_rejects_non_video_roles_and_budget_overflow() {
        let packet = test_rtp_packet(1, b"video");
        let mut capture = VideoRtpCapture::enabled();

        let non_video = capture.push(MediaRole::Audio, &packet).unwrap_err();
        assert!(format!("{non_video:#}").contains("Video RTP 捕获不支持的媒体角色"));

        capture
            .encoded
            .as_mut()
            .unwrap()
            .resize(MAX_VIDEO_RTP_CAPTURE_BYTES, 0);
        let overflow = capture.push(MediaRole::VideoStream1, &packet).unwrap_err();
        assert!(format!("{overflow:#}").contains("Video RTP 捕获超过最大允许容量"));
    }

    #[test]
    fn authenticated_video_rtcp_is_not_written_to_video_rtp_capture() {
        let mut session = empty_hpss_session();
        session.video_rtp_capture = VideoRtpCapture::enabled();
        let rtp = test_rtp_packet(1, b"video");

        record_authenticated_datagram(
            &mut session,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(rtp.clone()),
        )
        .unwrap();
        record_authenticated_datagram(
            &mut session,
            MediaRole::VideoStream2,
            MediaDatagram::Rtcp(vec![0x80, 201, 0, 1]),
        )
        .unwrap();

        let mut expected = b"FRDVTP01".to_vec();
        expected.push(1);
        expected.extend_from_slice(&(rtp.len() as u32).to_be_bytes());
        expected.extend_from_slice(&rtp);
        assert_eq!(session.video_rtp_capture.as_bytes(), expected);
        assert_eq!(session.stats.authenticated_rtcp_packets, 1);
    }

    #[test]
    fn disabled_video_rtp_capture_does_not_accumulate_or_exhaust_budget() {
        const PAYLOAD_BYTES: usize = 64 * 1024;
        let mut session = empty_hpss_session();
        let packet = test_rtp_packet(1, &vec![0x5a; PAYLOAD_BYTES]);
        let packet_count = MAX_VIDEO_RTP_CAPTURE_BYTES / packet.len() + 2;

        for _ in 0..packet_count {
            record_authenticated_datagram(
                &mut session,
                MediaRole::VideoStream1,
                MediaDatagram::Rtp(packet.clone()),
            )
            .expect("未启用视频捕获不得因诊断预算终止 UDP 产品会话");
        }

        assert_eq!(session.stats.authenticated_video_rtp_packets, packet_count);
        assert_eq!(session.video_rtp_capture.as_bytes(), []);
    }

    #[test]
    fn authenticated_rtcp_has_a_separate_counter() {
        let mut session = HpssSession {
            display: None,
            stats: HpssStats::default(),
            media_answer_frame: None,
            audio_rtp_capture: AudioRtpCapture::default(),
            video_rtp_capture: VideoRtpCapture::default(),
        };

        record_authenticated_datagram(
            &mut session,
            MediaRole::VideoStream1,
            MediaDatagram::Rtcp(vec![0x80, 201, 0, 1]),
        )
        .unwrap();

        assert_eq!(session.stats.authenticated_rtcp_packets, 1);
        assert_eq!(session.stats.authenticated_video_rtp_packets, 0);
    }

    use crate::dynamic_resolution::DisplaySize;

    #[test]
    fn build_display_query_uses_exact_1920x1080_wire_layout() {
        let size = DisplaySize::new(1920, 1080).unwrap();
        let expected: [u8; 16] = [
            0x09, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x80,
            0x04, 0x38,
        ];

        assert_eq!(build_display_query(size), expected);
    }

    #[test]
    fn build_set_display_config_preserves_literal_wire_layout() {
        let m = build_set_display_config("测试显示器");
        let mut expected = [0u8; 308];
        expected[..29].copy_from_slice(&[
            0x1d, 0x00, 0x01, 0x30, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x28,
            0xe6, 0xb5, 0x8b, 0xe8, 0xaf, 0x95, 0xe6, 0x98, 0xbe, 0xe7, 0xa4, 0xba, 0xe5, 0x99,
            0xa8,
        ]);

        assert_eq!(m, expected);
    }

    #[test]
    fn high_performance_set_display_config_encodes_exact_60_hz_primary_tier() {
        let display_name = "D".repeat(121);
        let primary = DisplaySize::new(2560, 1440).unwrap();
        let message = build_high_performance_set_display_config(
            &display_name,
            primary,
            HighPerformanceRefreshTier::Hz60,
        );

        assert_eq!(message.len(), 308);
        assert_eq!(
            &message[..12],
            &[0x1d, 0x00, 0x01, 0x30, 0x00, 0x01, 0x00, 0x01, 0, 0, 0, 0]
        );
        assert_eq!(&message[12..14], &[0x01, 0x28]);
        assert_eq!(&message[14..133], &[b'D'; 119]);
        assert_eq!(message[133], 0);
        assert_eq!(&message[134..142], &[0; 8]);
        assert_eq!(&message[142..146], &[0x43, 0xb8, 0xba, 0x2f]);
        assert_eq!(&message[146..150], &[0x43, 0x4f, 0xd1, 0x74]);
        assert_eq!(&message[150..154], &3840u32.to_be_bytes());
        assert_eq!(&message[154..158], &2160u32.to_be_bytes());
        assert_eq!(&message[158..162], &[0; 4]);
        assert_eq!(&message[162..166], &7u32.to_be_bytes());
        assert_eq!(&message[166..168], &5u16.to_be_bytes());

        let expected_modes = [
            (2560, 1440, 2560, 1440),
            (2880, 1800, 1440, 900),
            (3840, 2160, 1920, 1080),
            (2880, 1620, 1440, 810),
            (2624, 1696, 1312, 848),
        ];
        for (mode, expected) in message[168..].chunks_exact(0x1c).zip(expected_modes) {
            assert_eq!(
                u32::from_be_bytes(mode[0..4].try_into().unwrap()),
                expected.0
            );
            assert_eq!(
                u32::from_be_bytes(mode[4..8].try_into().unwrap()),
                expected.1
            );
            assert_eq!(
                u32::from_be_bytes(mode[8..12].try_into().unwrap()),
                expected.2
            );
            assert_eq!(
                u32::from_be_bytes(mode[12..16].try_into().unwrap()),
                expected.3
            );
            assert_eq!(&mode[16..24], &[0x40, 0x4e, 0, 0, 0, 0, 0, 0]);
            assert_eq!(&mode[24..28], &[0; 4]);
        }
    }

    #[test]
    fn high_performance_set_display_config_encodes_exact_30_hz_fallback_tier() {
        let message = build_high_performance_set_display_config(
            "Apple HP 30 Hz",
            DisplaySize::new(2560, 1440).unwrap(),
            HighPerformanceRefreshTier::Hz30,
        );

        assert_eq!(message.len(), 308);
        let modes = message[168..].chunks_exact(0x1c);
        assert_eq!(modes.len(), 5);
        for (index, mode) in modes.enumerate() {
            if index == 0 {
                assert_eq!(
                    &mode[..16],
                    &[
                        0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x05, 0xa0, 0x00, 0x00, 0x0a, 0x00,
                        0x00, 0x00, 0x05, 0xa0,
                    ]
                );
            }
            assert_eq!(&mode[16..24], &[0x40, 0x3e, 0, 0, 0, 0, 0, 0]);
        }
    }

    #[test]
    fn parse_server_state_uses_the_captured_active_framebuffer_group() {
        let mut m = CAPTURED_SERVER_STATE_WITH_ACTIVE_FRAMEBUFFER.to_vec();
        assert_eq!(parse_server_state_w_h(&m), Some((1331, 2365)));
        assert_eq!(
            parse_server_state_geometry(&m).unwrap(),
            ServerStateGeometry {
                message_version: 5,
                display_count: 1,
                width: 1331,
                height: 2365,
            }
        );

        assert!(parse_server_state_geometry(&m[..m.len() - 1]).is_err());
        m[12..16].copy_from_slice(&encoding::MVS.to_be_bytes());
        assert!(parse_server_state_geometry(&m).is_err());
    }

    #[test]
    fn parse_server_state_rejects_dimensions_without_display_records() {
        let mut message = vec![0u8; 28];
        message[0..4].copy_from_slice(&media_protocol::PRIMARY_MEDIA_STREAM_ID.to_be_bytes());
        message[12..16].copy_from_slice(&encoding::SERVER_STATE.to_be_bytes());
        message[16..18].copy_from_slice(&10u16.to_be_bytes());
        message[18..20].copy_from_slice(&5u16.to_be_bytes());
        message[20..22].copy_from_slice(&1440u16.to_be_bytes());
        message[22..24].copy_from_slice(&2560u16.to_be_bytes());
        message[24..26].copy_from_slice(&1331u16.to_be_bytes());
        message[26..28].copy_from_slice(&2365u16.to_be_bytes());

        assert!(parse_server_state_geometry(&message).is_err());
    }

    #[test]
    fn parse_server_state_accepts_stock_style_trailing_extension() {
        let mut message = CAPTURED_SERVER_STATE_WITH_ACTIVE_FRAMEBUFFER.to_vec();
        message.extend_from_slice(&[0; 4]);
        message[16..18].copy_from_slice(&80u16.to_be_bytes());

        assert_eq!(parse_server_state_w_h(&message), Some((1331, 2365)));
    }

    #[test]
    fn parse_media_rect_mvs() {
        // [u32 1][x y w h][s32 0x3f3][u32 总长][000f19...]
        let mut m = vec![0u8; 16];
        m[0..4].copy_from_slice(&1u32.to_be_bytes());
        m[8..10].copy_from_slice(&1358u16.to_be_bytes());
        m[12..16].copy_from_slice(&0x3f3i32.to_be_bytes());
        m.extend_from_slice(&2288u32.to_be_bytes());
        m.extend_from_slice(&[0x00, 0x0f, 0x19]);
        match parse_media(&m).unwrap() {
            Media::Mvs { w, total, body, .. } => {
                assert_eq!(w, 1358);
                assert_eq!(total, 2288);
                assert_eq!(body, vec![0x00, 0x0f, 0x19]);
            }
            _ => panic!("应解析为 MVS"),
        }
    }

    #[test]
    fn cursor_payload_requires_magic_and_exact_declared_length() {
        let compressed = [1u8, 2, 3, 4];
        let mut message = vec![0u8; 16];
        message[0..4].copy_from_slice(&1u32.to_be_bytes());
        message[12..16].copy_from_slice(&encoding::CURSOR.to_be_bytes());
        message.extend_from_slice(&cursor::PAYLOAD_MAGIC.to_be_bytes());
        message.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        message.extend_from_slice(&compressed);
        assert!(matches!(
            parse_media(&message).unwrap(),
            Media::Cursor { zlib, .. } if zlib == compressed
        ));

        let mut wrong_magic = message.clone();
        wrong_magic[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert!(parse_media(&wrong_magic).is_err());
        assert!(parse_media(&message[..message.len() - 1]).is_err());
    }

    #[test]
    fn full_refresh_request_uses_non_incremental_rfb_wire_layout() {
        assert_eq!(
            full_refresh_request(0x1234, 0xabcd).unwrap(),
            [0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x34, 0xab, 0xcd]
        );
    }

    #[test]
    fn collector_treats_heartbeat_and_query_shaped_continuations_as_opaque() {
        for continuation_first_byte in [0x14, 0x08] {
            let mut collector = HpssMvsCollector::default();
            let rect = MvsRect {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            };
            assert!(collector.begin(rect, 2, &[0]).unwrap().is_none());

            let record = collector
                .push_continuation(&[continuation_first_byte])
                .unwrap()
                .unwrap();

            assert_eq!(record.payload, vec![0, continuation_first_byte]);
        }
    }

    #[test]
    fn capture_round_trip_preserves_rect_and_payload() {
        let record = MvsRecord {
            rect: MvsRect {
                x: 11,
                y: 22,
                width: 333,
                height: 444,
            },
            payload: vec![0xaa, 0xbb],
        };
        let mut sink = Vec::new();

        let mut writer = MvsCaptureWriter::new(&mut sink).unwrap();
        writer.write_record(&record).unwrap();

        assert_eq!(&sink[..8], b"FRDMVS01");
        assert_eq!(&sink[8..10], &11u16.to_be_bytes());
        assert_eq!(&sink[10..12], &22u16.to_be_bytes());
        assert_eq!(&sink[12..14], &333u16.to_be_bytes());
        assert_eq!(&sink[14..16], &444u16.to_be_bytes());
        assert_eq!(&sink[16..20], &2u32.to_be_bytes());
        assert_eq!(read_mvs_capture(&sink).unwrap(), vec![record]);
    }

    #[test]
    fn mvs_capture_header_parses_named_fields_and_remaining_payload() {
        let fixture = [
            0x00, 0x0b, 0x00, 0x16, 0x01, 0x4d, 0x01, 0xbc, 0x00, 0x00, 0x00, 0x02, 0xaa, 0xbb,
        ];

        let (header, remaining) = MvsCaptureRecordHeader::parse(&fixture).unwrap();

        assert_eq!(header.rect.x, 11);
        assert_eq!(header.rect.y, 22);
        assert_eq!(header.rect.width, 333);
        assert_eq!(header.rect.height, 444);
        assert_eq!(header.payload_len, 2);
        assert_eq!(remaining, &[0xaa, 0xbb]);
    }

    #[test]
    fn capture_round_trip_preserves_table_init_zero_rect() {
        let table = MvsRecord {
            rect: MvsRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            payload: vec![1; 129],
        };
        let mut sink = Vec::new();

        let mut writer = MvsCaptureWriter::new(&mut sink).unwrap();
        writer.write_record(&table).unwrap();

        assert_eq!(read_mvs_capture(&sink).unwrap(), vec![table]);
    }

    #[test]
    fn malformed_mvs_table_candidate_is_captured_without_table_initialization_count() {
        let mut sink = Vec::new();
        let mut session = HpssSession {
            display: None,
            stats: HpssStats::default(),
            media_answer_frame: None,
            audio_rtp_capture: AudioRtpCapture::default(),
            video_rtp_capture: VideoRtpCapture::default(),
        };
        let malformed = MvsRecord {
            rect: MvsRect {
                x: 1,
                y: 0,
                width: 0,
                height: 0,
            },
            payload: vec![0; mvs::MVS_TABLE_INITIALIZATION_BYTES],
        };

        {
            let mut writer = Some(MvsCaptureWriter::new(&mut sink).unwrap());
            record_complete_mvs(&mut session, &mut writer, malformed).unwrap();
            record_complete_mvs(
                &mut session,
                &mut writer,
                MvsRecord {
                    rect: MvsRect {
                        x: 0,
                        y: 1,
                        width: 0,
                        height: 0,
                    },
                    payload: vec![0; mvs::MVS_TABLE_INITIALIZATION_BYTES],
                },
            )
            .unwrap();
        }

        assert_eq!(session.stats.mvs_frames, 2);
        assert_eq!(
            session.stats.mvs_bytes,
            2 * mvs::MVS_TABLE_INITIALIZATION_BYTES
        );
        assert_eq!(session.stats.table_inits, 0);
        assert_eq!(session.stats.malformed_table_diagnostics, 1);
        assert_eq!(read_mvs_capture(&sink).unwrap().len(), 2);
    }

    #[test]
    fn capture_reader_rejects_legacy_truncated_and_oversized_input() {
        assert!(read_mvs_capture(&[0, 0, 0, 2, 0xaa, 0xbb])
            .unwrap_err()
            .to_string()
            .contains("legacy"));
        assert!(read_mvs_capture(b"FRDMVS01\x00")
            .unwrap_err()
            .to_string()
            .contains("截断"));

        let mut oversized = b"FRDMVS01".to_vec();
        oversized.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 1]);
        oversized.extend_from_slice(&((MAX_MVS_RECORD_PAYLOAD + 1) as u32).to_be_bytes());
        assert!(read_mvs_capture(&oversized)
            .unwrap_err()
            .to_string()
            .contains("超过上限"));
    }

    #[test]
    fn capture_reader_rejects_declared_payload_that_is_truncated() {
        let mut truncated_payload = b"FRDMVS01".to_vec();
        truncated_payload.extend_from_slice(&[0, 1, 0, 2, 0, 3, 0, 4]);
        truncated_payload.extend_from_slice(&4u32.to_be_bytes());
        truncated_payload.extend_from_slice(&[0xaa, 0xbb]);

        assert!(read_mvs_capture(&truncated_payload)
            .unwrap_err()
            .to_string()
            .contains("payload 截断"));
    }
}
