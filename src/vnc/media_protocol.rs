//! Apple Screen Sharing 媒体流控制协议。
//!
//! 本模块只接受已由 `screensharingd` 静态编码器或真机 fixture 证明的布局。
//! 原始 fixture 是证据，不是生产逻辑中的魔数来源。

use anyhow::{bail, ensure, Result};

/// 服务器媒体控制矩形使用的主流 ID。
pub const PRIMARY_MEDIA_STREAM_ID: u32 = 1;
/// `EncodeRFBMediaStreamMessage1` 使用的矩形编码。
pub const MEDIA_STREAM_CONTROL_ENCODING: i32 = 0x03f2;

/// `MediaStream Message 1` 的已验证版本字段。
pub const MEDIA_STREAM_PORT_ANNOUNCEMENT_VERSION: u16 = 1;
/// `MediaStream Message 1` 的端口公告种类字段。
pub const MEDIA_STREAM_PORT_ANNOUNCEMENT_KIND: u16 = 1;
/// `MediaStream Message 2` 的已验证版本字段。
pub const MEDIA_STREAM_ANSWER_VERSION: u16 = 2;
/// `MediaStream Message 2` 的 answer 种类字段。
pub const MEDIA_STREAM_ANSWER_KIND: u16 = 2;
const MEDIA_STREAM_PORT_ANNOUNCEMENT_BODY_LEN: usize = 36;
const MEDIA_STREAM_PORT_ANNOUNCEMENT_RESERVED_LEN: usize = 10;
const MEDIA_STREAM_CONTROL_DECLARED_LENGTH_BYTES: usize = size_of::<u16>();
const MEDIA_STREAM_CONTROL_VERSION_BYTES: usize = size_of::<u16>();
const MEDIA_STREAM_CONTROL_KIND_BYTES: usize = size_of::<u16>();
const MEDIA_STREAM_CONTROL_DISCRIMINATOR_BYTES: usize = MEDIA_STREAM_CONTROL_DECLARED_LENGTH_BYTES
    + MEDIA_STREAM_CONTROL_VERSION_BYTES
    + MEDIA_STREAM_CONTROL_KIND_BYTES;
/// 编码器对所有已发出的描述符置此位；其私有语义名称尚未恢复。
const MEDIA_STREAM_DESCRIPTOR_EMITTED_MARKER: u32 = 1 << 0;
const MEDIA_STREAM_DESCRIPTOR_HDR_FLAG: u32 = 1 << 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaStreamDescriptorFlags(u32);

impl MediaStreamDescriptorFlags {
    pub fn encoder_emitted_marker(self) -> bool {
        self.0 & MEDIA_STREAM_DESCRIPTOR_EMITTED_MARKER != 0
    }

    pub fn hdr(self) -> bool {
        self.0 & MEDIA_STREAM_DESCRIPTOR_HDR_FLAG != 0
    }

    fn is_zero(self) -> bool {
        self.0 == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaStreamPortDescriptor {
    pub port: u16,
    pub flags: MediaStreamDescriptorFlags,
}

impl MediaStreamPortDescriptor {
    pub fn is_announced(self) -> bool {
        self.port != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaStreamPortAnnouncement {
    pub message_flags: u32,
    pub audio: MediaStreamPortDescriptor,
    pub video_stream_1: MediaStreamPortDescriptor,
    pub video_stream_2: MediaStreamPortDescriptor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaStreamControlKind {
    PortAnnouncement,
    Answer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MediaStreamControlDiscriminator {
    declared_body_len: usize,
    kind: MediaStreamControlKind,
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

    fn remaining_bytes(&self) -> &'a [u8] {
        &self.bytes[self.position..]
    }

    fn take(&mut self, count: usize, field: &str) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| anyhow::anyhow!("{field} 长度溢出"))?;
        if end > self.bytes.len() {
            bail!("{field} 截断");
        }
        let value = &self.bytes[self.position..end];
        self.position = end;
        Ok(value)
    }

    fn read_u16(&mut self, field: &str) -> Result<u16> {
        let bytes: [u8; size_of::<u16>()] = self.take(size_of::<u16>(), field)?.try_into()?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn read_u32(&mut self, field: &str) -> Result<u32> {
        let bytes: [u8; size_of::<u32>()] = self.take(size_of::<u32>(), field)?.try_into()?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn read_i32(&mut self, field: &str) -> Result<i32> {
        let bytes: [u8; size_of::<i32>()] = self.take(size_of::<i32>(), field)?.try_into()?;
        Ok(i32::from_be_bytes(bytes))
    }
}

fn parse_media_stream_control_discriminator(
    payload: &[u8],
) -> Result<MediaStreamControlDiscriminator> {
    let mut cursor = WireCursor::new(payload);
    let declared_body_len = usize::from(cursor.read_u16("消息体长度")?);
    let version = cursor.read_u16("消息版本")?;
    let kind = cursor.read_u16("消息种类")?;
    let kind = match (version, kind) {
        (MEDIA_STREAM_PORT_ANNOUNCEMENT_VERSION, MEDIA_STREAM_PORT_ANNOUNCEMENT_KIND) => {
            MediaStreamControlKind::PortAnnouncement
        }
        (MEDIA_STREAM_ANSWER_VERSION, MEDIA_STREAM_ANSWER_KIND) => MediaStreamControlKind::Answer,
        _ => bail!("不支持的媒体流控制消息 version={version} kind={kind}"),
    };
    Ok(MediaStreamControlDiscriminator {
        declared_body_len,
        kind,
    })
}

pub fn parse_media_stream_control_kind(payload: &[u8]) -> Result<MediaStreamControlKind> {
    Ok(parse_media_stream_control_discriminator(payload)?.kind)
}

fn read_port_descriptor(
    cursor: &mut WireCursor<'_>,
    role: &str,
) -> Result<MediaStreamPortDescriptor> {
    let port = cursor.read_u16(&format!("{role} UDP 端口"))?;
    let flags = cursor.read_u32(&format!("{role} 标志"))?;
    Ok(MediaStreamPortDescriptor {
        port,
        flags: MediaStreamDescriptorFlags(flags),
    })
}

/// 严格解析服务器 `MediaStream Message 1` 端口公告。
pub fn parse_media_stream_port_announcement(frame: &[u8]) -> Result<MediaStreamPortAnnouncement> {
    let mut cursor = WireCursor::new(frame);
    ensure!(
        cursor.read_u32("媒体流 ID")? == PRIMARY_MEDIA_STREAM_ID,
        "MediaStream Message 1 的媒体流 ID 非法"
    );

    for coordinate in ["x", "y", "width", "height"] {
        ensure!(
            cursor.read_u16(&format!("媒体矩形 {coordinate}"))? == 0,
            "MediaStream Message 1 必须使用零矩形"
        );
    }
    ensure!(
        cursor.read_i32("媒体编码")? == MEDIA_STREAM_CONTROL_ENCODING,
        "不是 MediaStream Message 1 编码"
    );

    let control = parse_media_stream_control_discriminator(cursor.remaining_bytes())?;
    ensure!(
        control.declared_body_len == MEDIA_STREAM_PORT_ANNOUNCEMENT_BODY_LEN,
        "MediaStream Message 1 长度字段非法: {}",
        control.declared_body_len
    );
    ensure!(
        cursor.remaining()
            == MEDIA_STREAM_CONTROL_DECLARED_LENGTH_BYTES + control.declared_body_len,
        "MediaStream Message 1 实际长度与声明不一致"
    );
    ensure!(
        control.kind == MediaStreamControlKind::PortAnnouncement,
        "媒体流控制消息不是端口公告"
    );
    cursor.take(
        MEDIA_STREAM_CONTROL_DISCRIMINATOR_BYTES,
        "MediaStream Message 1 判别字段",
    )?;

    let message_flags = cursor.read_u32("端口公告标志")?;
    let audio = read_port_descriptor(&mut cursor, "音频流")?;
    let video_stream_1 = read_port_descriptor(&mut cursor, "视频流 1")?;
    let video_stream_2 = read_port_descriptor(&mut cursor, "视频流 2")?;
    for (role, descriptor) in [
        ("音频流", audio),
        ("视频流 1", video_stream_1),
        ("视频流 2", video_stream_2),
    ] {
        if descriptor.is_announced() {
            ensure!(
                descriptor.flags.encoder_emitted_marker(),
                "{role} 非零端口缺少编码器已发出标记"
            );
        } else {
            ensure!(descriptor.flags.is_zero(), "{role} 空描述符带有非零标志");
        }
    }
    let reserved = cursor.take(
        MEDIA_STREAM_PORT_ANNOUNCEMENT_RESERVED_LEN,
        "MediaStream Message 1 保留区",
    )?;
    ensure!(
        reserved.iter().all(|byte| *byte == 0),
        "MediaStream Message 1 保留区必须为零"
    );
    ensure!(
        cursor.remaining() == 0,
        "MediaStream Message 1 存在尾随数据"
    );

    Ok(MediaStreamPortAnnouncement {
        message_flags,
        audio,
        video_stream_1,
        video_stream_2,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        parse_media_stream_control_kind, parse_media_stream_port_announcement,
        MediaStreamControlKind,
    };

    const TEST_BASE_UDP_PORT: u16 = 0x4567;

    /// `EncodeRFBMediaStreamMessage1` x86 指令序列的逐字节对应物：
    /// primary stream、零矩形、encoding 0x3f2、Message 1、一个视频流。
    const STATIC_ENCODER_ONE_VIDEO_FIXTURE: [u8; 54] = [
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03,
        0xf2, 0x00, 0x24, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x45, 0x67, 0x00, 0x00,
        0x00, 0x01, 0x45, 0x68, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn media_stream_control_discriminator_classifies_verified_pairs() {
        let port_announcement = [0x00, 0x04, 0x00, 0x01, 0x00, 0x01];
        let answer = [0x00, 0x04, 0x00, 0x02, 0x00, 0x02];

        assert_eq!(
            parse_media_stream_control_kind(&port_announcement).unwrap(),
            MediaStreamControlKind::PortAnnouncement
        );
        assert_eq!(
            parse_media_stream_control_kind(&answer).unwrap(),
            MediaStreamControlKind::Answer
        );
    }

    #[test]
    fn parses_static_server_port_announcement_exactly() {
        let announcement =
            parse_media_stream_port_announcement(&STATIC_ENCODER_ONE_VIDEO_FIXTURE).unwrap();

        assert_eq!(announcement.audio.port, TEST_BASE_UDP_PORT);
        assert!(announcement.audio.is_announced());
        assert!(announcement.audio.flags.encoder_emitted_marker());
        assert!(!announcement.audio.flags.hdr());
        assert_eq!(announcement.video_stream_1.port, TEST_BASE_UDP_PORT + 1);
        assert!(announcement.video_stream_1.is_announced());
        assert!(announcement.video_stream_1.flags.encoder_emitted_marker());
        assert!(!announcement.video_stream_1.flags.hdr());
        assert_eq!(announcement.video_stream_2.port, 0);
        assert!(!announcement.video_stream_2.is_announced());
    }

    #[test]
    fn rejects_truncated_port_announcement() {
        assert!(parse_media_stream_port_announcement(
            &STATIC_ENCODER_ONE_VIDEO_FIXTURE[..STATIC_ENCODER_ONE_VIDEO_FIXTURE.len() - 1]
        )
        .is_err());
    }

    #[test]
    fn rejects_nonzero_reserved_tail() {
        let mut malformed = STATIC_ENCODER_ONE_VIDEO_FIXTURE;
        let last = malformed.len() - 1;
        malformed[last] = 1;

        assert!(parse_media_stream_port_announcement(&malformed).is_err());
    }

    #[test]
    fn rejects_nonzero_port_without_encoder_emitted_marker() {
        let mut malformed = STATIC_ENCODER_ONE_VIDEO_FIXTURE;
        malformed[28..32].copy_from_slice(&0u32.to_be_bytes());

        assert!(parse_media_stream_port_announcement(&malformed).is_err());
    }

    #[test]
    fn rejects_flags_on_an_absent_descriptor() {
        let mut malformed = STATIC_ENCODER_ONE_VIDEO_FIXTURE;
        malformed[40..44].copy_from_slice(&1u32.to_be_bytes());

        assert!(parse_media_stream_port_announcement(&malformed).is_err());
    }
}
