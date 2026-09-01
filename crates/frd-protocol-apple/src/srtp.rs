//! Apple AVConference 外部 cipher suite 5（内部 transform policy 7）的
//! SRTP/SRTCP 数据面。
//!
//! 当前策略使用 AES-256 Counter Mode、14 字节主盐以及截断为 80 位的
//! HMAC-SHA1。派生标签、计数器布局、SRTCP 明文前缀和认证顺序均由当前
//! AVConference `_setTransformPolicyFromCipherSuite`、`_MakeSessionKey`、
//! `_SRTPEncryptData`、`_SRTCPEncrypt`、`_SRTCPAddAuthenticationTag` 确认。

use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes256;
use anyhow::{ensure, Context, Result};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::media_negotiation::SrtpMasterMaterial;

pub const SRTP_SESSION_ENCRYPTION_KEY_LEN: usize = 32;
pub const SRTP_SESSION_SALT_LEN: usize = 14;
pub const SRTP_SESSION_AUTHENTICATION_KEY_LEN: usize = 20;
pub const SRTP_AUTHENTICATION_TAG_LEN: usize = 10;
const AES_BLOCK_LEN: usize = 16;
pub(crate) const RTP_FIXED_HEADER_LEN: usize = 12;
pub(crate) const RTP_VERSION: u8 = 2;
pub(crate) const RTP_VERSION_SHIFT: u8 = 6;
const RTP_VERSION_MASK: u8 = 0b11;
const RTP_CSRC_COUNT_MASK: u8 = 0x0f;
const RTP_EXTENSION_FLAG: u8 = 1 << 4;
const RTP_PADDING_FLAG: u8 = 1 << 5;
const RTP_EXTENSION_HEADER_LEN: usize = 4;
const RTP_EXTENSION_LENGTH_OFFSET: usize = 2;
const RTP_EXTENSION_WORD_BYTES: usize = 4;
const RTP_SEQUENCE_OFFSET: usize = 2;
const RTP_SSRC_OFFSET: usize = 8;
const RTP_TIMESTAMP_OFFSET: usize = 4;
const SSRC_LEN: usize = 4;
const RTP_SEQUENCE_LEN: usize = 2;
const RTP_TIMESTAMP_LEN: usize = 4;
pub(crate) const RTP_MARKER_MASK: u8 = 1 << 7;
const RTP_PAYLOAD_TYPE_MASK: u8 = RTP_MARKER_MASK - 1;
const RTP_RTCP_MUX_PACKET_TYPE_MIN: u8 = 192;
const RTP_RTCP_MUX_PACKET_TYPE_MAX: u8 = 223;
const RTP_MUX_PACKET_TYPE_OFFSET: usize = 1;
const RTP_MUX_CLASSIFICATION_BYTES: usize = RTP_MUX_PACKET_TYPE_OFFSET + size_of::<u8>();
const SRTP_PACKET_INDEX_LEN: usize = 6;
const SRTP_REPLAY_WINDOW_WORDS: usize = 4;
const SRTP_REPLAY_WINDOW_BITS: u64 = (SRTP_REPLAY_WINDOW_WORDS * u64::BITS as usize) as u64;
const SRTCP_REPLAY_WINDOW_BITS: u32 = u64::BITS;
const RTP_SEQUENCE_SPACE: u32 = 1 << u16::BITS;
const RTP_SEQUENCE_HALF_SPACE: u16 = (RTP_SEQUENCE_SPACE / 2) as u16;
const SRTP_COUNTER_SSRC_OFFSET: usize = 4;
const SRTP_COUNTER_PACKET_INDEX_OFFSET: usize = 8;
const SRTP_COUNTER_BLOCK_INDEX_LEN: usize = 2;
const SRTP_KDF_LABEL_OFFSET: usize = 7;
const SRTP_KDF_BLOCK_INDEX_OFFSET: usize = AES_BLOCK_LEN - 1;
const SRTCP_UNENCRYPTED_PREFIX_LEN: usize = 8;
const RTCP_SSRC_OFFSET: usize = 4;
const SRTCP_INDEX_LEN: usize = 4;
const SRTCP_ENCRYPTED_FLAG: u32 = 1 << 31;
const SRTCP_INDEX_MASK: u32 = SRTCP_ENCRYPTED_FLAG - 1;
const APPLE_SRTCP_MAX_SEND_INDEX: u32 = SRTCP_INDEX_MASK - 1;
const RTCP_RECEIVER_REPORT_PACKET_TYPE: u8 = 201;
const RTCP_RECEIVER_REPORT_WORDS_MINUS_ONE: u16 = 1;
const RTCP_VERSION_AND_ZERO_REPORTS: u8 = RTP_VERSION << RTP_VERSION_SHIFT;
const RTCP_SOURCE_DESCRIPTION_PACKET_TYPE: u8 = 202;
const RTCP_SOURCE_DESCRIPTION_SOURCE_COUNT: u8 = 1;
const RTCP_SOURCE_DESCRIPTION_CNAME_ITEM: u8 = 1;
const RTCP_SOURCE_DESCRIPTION_END_ITEM: u8 = 0;
const RTCP_WORD_LEN: usize = 4;
const RTCP_COMMON_HEADER_LEN: usize = 4;
const RTCP_REPORT_COUNT_MASK: u8 = 0x1f;
const RTCP_PACKET_TYPE_OFFSET: usize = 1;
const RTCP_LENGTH_OFFSET: usize = 2;
const RTCP_SENDER_REPORT_PREFIX_LEN: usize = 28;
const RTCP_RECEIVER_REPORT_PREFIX_LEN: usize = 8;
const RTCP_RECEPTION_REPORT_LEN: usize = 24;
const RTCP_REPORT_SOURCE_SSRC_OFFSET: usize = 0;
const RTCP_REPORT_FRACTION_LOST_OFFSET: usize = 4;
const RTCP_REPORT_CUMULATIVE_LOST_OFFSET: usize = 5;
const RTCP_CUMULATIVE_LOSS_BYTES: usize = 3;
const RTCP_CUMULATIVE_LOSS_SIGN_BIT: u32 = 1 << 23;
const RTCP_CUMULATIVE_LOSS_SIGN_EXTENSION_MASK: u32 = 0xff00_0000;
const RTCP_REPORT_EXTENDED_HIGHEST_SEQUENCE_OFFSET: usize = 8;
const RTCP_REPORT_INTERARRIVAL_JITTER_OFFSET: usize = 12;
const RTCP_SENDER_REPORT_PACKET_TYPE: u8 = 200;
const SCREEN_SHARING_CNAME_PREFIX: &str = "freeremotedesk-";

#[derive(Clone, Copy)]
enum SrtpKdfLabel {
    RtpEncryption = 0,
    RtpAuthentication = 1,
    RtpSalt = 2,
    RtcpEncryption = 3,
    RtcpAuthentication = 4,
    RtcpSalt = 5,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SrtpSessionKeys {
    pub encryption_key: [u8; SRTP_SESSION_ENCRYPTION_KEY_LEN],
    pub salt: [u8; SRTP_SESSION_SALT_LEN],
    pub authentication_key: [u8; SRTP_SESSION_AUTHENTICATION_KEY_LEN],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SrtpPacketKind {
    Rtp,
    Rtcp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtpMuxPacketKind {
    Rtp,
    Rtcp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtpHeader {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload_offset: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtpPacket<'a> {
    pub header: RtpHeader,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtcpReceptionReport {
    pub reporter_ssrc: u32,
    pub source_ssrc: u32,
    pub fraction_lost: u8,
    pub cumulative_packets_lost: i32,
    pub extended_highest_sequence: u32,
    pub interarrival_jitter: u32,
}

/// 按 RFC 5761 的不冲突 payload-type 区间区分 RTP 与 RTCP。
pub fn classify_rtp_mux_packet(packet: &[u8]) -> Result<RtpMuxPacketKind> {
    ensure!(
        packet.len() >= RTP_MUX_CLASSIFICATION_BYTES,
        "RTP/RTCP mux 数据报头截断"
    );
    ensure!(
        rtp_version(packet[0]) == RTP_VERSION,
        "RTP/RTCP version 不是 2"
    );
    let packet_type = packet[RTP_MUX_PACKET_TYPE_OFFSET];
    if (RTP_RTCP_MUX_PACKET_TYPE_MIN..=RTP_RTCP_MUX_PACKET_TYPE_MAX).contains(&packet_type) {
        Ok(RtpMuxPacketKind::Rtcp)
    } else {
        Ok(RtpMuxPacketKind::Rtp)
    }
}

fn decode_signed_24_be(bytes: [u8; RTCP_CUMULATIVE_LOSS_BYTES]) -> i32 {
    let raw = u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);
    if raw & RTCP_CUMULATIVE_LOSS_SIGN_BIT != 0 {
        (raw | RTCP_CUMULATIVE_LOSS_SIGN_EXTENSION_MASK) as i32
    } else {
        raw as i32
    }
}

pub fn parse_rtp_header(packet: &[u8]) -> Result<RtpHeader> {
    let payload_offset = rtp_payload_offset(packet)?;
    Ok(RtpHeader {
        marker: packet[1] & RTP_MARKER_MASK != 0,
        payload_type: packet[1] & RTP_PAYLOAD_TYPE_MASK,
        sequence: u16::from_be_bytes(
            packet[RTP_SEQUENCE_OFFSET..RTP_SEQUENCE_OFFSET + RTP_SEQUENCE_LEN]
                .try_into()
                .expect("已验证 RTP 固定头长度"),
        ),
        timestamp: u32::from_be_bytes(
            packet[RTP_TIMESTAMP_OFFSET..RTP_TIMESTAMP_OFFSET + RTP_TIMESTAMP_LEN]
                .try_into()
                .expect("已验证 RTP 固定头长度"),
        ),
        ssrc: read_ssrc(packet, RTP_SSRC_OFFSET),
        payload_offset,
    })
}

pub fn parse_rtp_packet(packet: &[u8]) -> Result<RtpPacket<'_>> {
    validate_rtp_packet(packet)?;
    let header = parse_rtp_header(packet)?;
    let padding_length = if packet[0] & RTP_PADDING_FLAG != 0 {
        usize::from(*packet.last().context("带 padding 的 RTP 数据报为空")?)
    } else {
        0
    };
    ensure!(
        padding_length <= packet.len().saturating_sub(header.payload_offset),
        "RTP padding 长度越界"
    );
    let payload_end = packet.len() - padding_length;
    Ok(RtpPacket {
        header,
        payload: &packet[header.payload_offset..payload_end],
    })
}

pub fn parse_rtcp_reception_reports(compound: &[u8]) -> Result<Vec<RtcpReceptionReport>> {
    let mut reports = Vec::new();
    let mut packet_offset = 0usize;
    while packet_offset < compound.len() {
        let remaining = &compound[packet_offset..];
        ensure!(
            remaining.len() >= RTCP_COMMON_HEADER_LEN,
            "RTCP compound packet 的公共头截断"
        );
        ensure!(
            rtp_version(remaining[0]) == RTP_VERSION,
            "RTCP version 不是 2"
        );
        let words_minus_one = u16::from_be_bytes(
            remaining[RTCP_LENGTH_OFFSET..RTCP_LENGTH_OFFSET + 2]
                .try_into()
                .expect("已验证 RTCP 公共头长度"),
        );
        let packet_len = (usize::from(words_minus_one) + 1)
            .checked_mul(RTCP_WORD_LEN)
            .context("RTCP packet 长度溢出")?;
        ensure!(
            packet_len >= RTCP_COMMON_HEADER_LEN && packet_len <= remaining.len(),
            "RTCP packet 声明长度越界"
        );
        let packet = &remaining[..packet_len];
        let report_count = usize::from(packet[0] & RTCP_REPORT_COUNT_MASK);
        let report_start = match packet[RTCP_PACKET_TYPE_OFFSET] {
            RTCP_SENDER_REPORT_PACKET_TYPE => Some(RTCP_SENDER_REPORT_PREFIX_LEN),
            RTCP_RECEIVER_REPORT_PACKET_TYPE => Some(RTCP_RECEIVER_REPORT_PREFIX_LEN),
            _ => None,
        };
        if let Some(report_start) = report_start {
            ensure!(packet.len() >= report_start, "RTCP report 固定字段截断");
            let reports_len = report_count
                .checked_mul(RTCP_RECEPTION_REPORT_LEN)
                .context("RTCP report block 长度溢出")?;
            ensure!(
                report_start + reports_len <= packet.len(),
                "RTCP reception report block 截断"
            );
            let reporter_ssrc = read_ssrc(packet, RTCP_SSRC_OFFSET);
            for block in packet[report_start..report_start + reports_len]
                .chunks_exact(RTCP_RECEPTION_REPORT_LEN)
            {
                let cumulative_packets_lost = decode_signed_24_be(
                    block[RTCP_REPORT_CUMULATIVE_LOST_OFFSET
                        ..RTCP_REPORT_CUMULATIVE_LOST_OFFSET + RTCP_CUMULATIVE_LOSS_BYTES]
                        .try_into()
                        .expect("RTCP report block 长度固定"),
                );
                reports.push(RtcpReceptionReport {
                    reporter_ssrc,
                    source_ssrc: read_ssrc(block, RTCP_REPORT_SOURCE_SSRC_OFFSET),
                    fraction_lost: block[RTCP_REPORT_FRACTION_LOST_OFFSET],
                    cumulative_packets_lost,
                    extended_highest_sequence: u32::from_be_bytes(
                        block[RTCP_REPORT_EXTENDED_HIGHEST_SEQUENCE_OFFSET
                            ..RTCP_REPORT_EXTENDED_HIGHEST_SEQUENCE_OFFSET + 4]
                            .try_into()
                            .expect("RTCP report block 长度固定"),
                    ),
                    interarrival_jitter: u32::from_be_bytes(
                        block[RTCP_REPORT_INTERARRIVAL_JITTER_OFFSET
                            ..RTCP_REPORT_INTERARRIVAL_JITTER_OFFSET + 4]
                            .try_into()
                            .expect("RTCP report block 长度固定"),
                    ),
                });
            }
        }
        packet_offset += packet_len;
    }
    ensure!(
        packet_offset == compound.len(),
        "RTCP compound packet 边界不一致"
    );
    Ok(reports)
}

pub fn derive_session_keys(material: &SrtpMasterMaterial, kind: SrtpPacketKind) -> SrtpSessionKeys {
    let (encryption_label, authentication_label, salt_label) = match kind {
        SrtpPacketKind::Rtp => (
            SrtpKdfLabel::RtpEncryption,
            SrtpKdfLabel::RtpAuthentication,
            SrtpKdfLabel::RtpSalt,
        ),
        SrtpPacketKind::Rtcp => (
            SrtpKdfLabel::RtcpEncryption,
            SrtpKdfLabel::RtcpAuthentication,
            SrtpKdfLabel::RtcpSalt,
        ),
    };
    SrtpSessionKeys {
        encryption_key: derive_key(
            &material.master_key,
            &material.master_salt,
            encryption_label,
        ),
        salt: derive_key(&material.master_key, &material.master_salt, salt_label),
        authentication_key: derive_key(
            &material.master_key,
            &material.master_salt,
            authentication_label,
        ),
    }
}

pub fn crypt_rtp_packet_in_place(
    packet: &mut [u8],
    keys: &SrtpSessionKeys,
    rollover_counter: u32,
) -> Result<()> {
    let payload_offset = rtp_payload_offset(packet)?;
    let sequence = u16::from_be_bytes(
        packet[RTP_SEQUENCE_OFFSET..RTP_SEQUENCE_OFFSET + RTP_SEQUENCE_LEN]
            .try_into()
            .expect("已验证 RTP 固定头长度"),
    );
    let ssrc = read_ssrc(packet, RTP_SSRC_OFFSET);
    let packet_index = (u64::from(rollover_counter) << u16::BITS) | u64::from(sequence);
    let counter = session_counter(&keys.salt, ssrc, packet_index)?;
    crypt_aes_counter(&mut packet[payload_offset..], &keys.encryption_key, counter);
    Ok(())
}

pub fn protect_rtp_packet(
    packet: &[u8],
    keys: &SrtpSessionKeys,
    rollover_counter: u32,
) -> Result<Vec<u8>> {
    rtp_payload_offset(packet)?;
    let mut protected = packet.to_vec();
    crypt_rtp_packet_in_place(&mut protected, keys, rollover_counter)?;
    let tag = authentication_tag_with_suffix(
        &protected,
        &rollover_counter.to_be_bytes(),
        &keys.authentication_key,
    );
    protected.extend_from_slice(&tag);
    Ok(protected)
}

pub struct SrtpReceiver {
    keys: SrtpSessionKeys,
    initial_rollover_counter: u32,
    highest_packet_index: Option<u64>,
    replay_window: SrtpReplayWindow,
}

#[derive(Default)]
struct SrtpReplayWindow {
    words: [u64; SRTP_REPLAY_WINDOW_WORDS],
}

impl SrtpReplayWindow {
    fn contains(&self, age: u64) -> bool {
        debug_assert!(age < SRTP_REPLAY_WINDOW_BITS);
        let word = (age / u64::from(u64::BITS)) as usize;
        let bit = (age % u64::from(u64::BITS)) as u32;
        self.words[word] & (1u64 << bit) != 0
    }

    fn mark(&mut self, age: u64) {
        debug_assert!(age < SRTP_REPLAY_WINDOW_BITS);
        let word = (age / u64::from(u64::BITS)) as usize;
        let bit = (age % u64::from(u64::BITS)) as u32;
        self.words[word] |= 1u64 << bit;
    }

    fn advance_and_mark_newest(&mut self, advance: u64) {
        if advance >= SRTP_REPLAY_WINDOW_BITS {
            self.words.fill(0);
        } else if advance != 0 {
            let previous = self.words;
            self.words.fill(0);
            let word_shift = (advance / u64::from(u64::BITS)) as usize;
            let bit_shift = (advance % u64::from(u64::BITS)) as u32;
            for (source, value) in previous.into_iter().enumerate() {
                let destination = source + word_shift;
                if destination >= SRTP_REPLAY_WINDOW_WORDS {
                    break;
                }
                self.words[destination] |= value << bit_shift;
                if bit_shift != 0 && destination + 1 < SRTP_REPLAY_WINDOW_WORDS {
                    self.words[destination + 1] |= value >> (u64::BITS - bit_shift);
                }
            }
        }
        self.mark(0);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SecurePacketDiscardKind {
    MalformedPacket,
    AuthenticationFailed,
}

#[derive(Debug)]
struct SecurePacketDiscardError {
    kind: SecurePacketDiscardKind,
    diagnostic: String,
}

impl std::fmt::Display for SecurePacketDiscardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.diagnostic)
    }
}

impl std::error::Error for SecurePacketDiscardError {}

fn secure_packet_discard_error(
    kind: SecurePacketDiscardKind,
    error: anyhow::Error,
) -> anyhow::Error {
    SecurePacketDiscardError {
        kind,
        diagnostic: format!("{error:#}"),
    }
    .into()
}

pub(crate) fn secure_packet_discard_kind(error: &anyhow::Error) -> Option<SecurePacketDiscardKind> {
    error
        .downcast_ref::<SecurePacketDiscardError>()
        .map(|error| error.kind)
}

impl SrtpReceiver {
    pub fn new(keys: SrtpSessionKeys) -> Self {
        Self::with_initial_rollover_counter(keys, 0)
    }

    fn with_initial_rollover_counter(keys: SrtpSessionKeys, rollover_counter: u32) -> Self {
        Self {
            keys,
            initial_rollover_counter: rollover_counter,
            highest_packet_index: None,
            replay_window: SrtpReplayWindow::default(),
        }
    }

    /// 打开一个 SRTP 数据报。
    ///
    /// `Ok(None)` 表示数据报已通过认证，但属于重放或早于接收窗口；UDP
    /// 接收端应丢弃它而不终止整个媒体会话。认证或格式错误仍返回 `Err`。
    pub fn open(&mut self, packet: &[u8]) -> Result<Option<Vec<u8>>> {
        if packet.len() < RTP_FIXED_HEADER_LEN + SRTP_AUTHENTICATION_TAG_LEN {
            return Err(secure_packet_discard_error(
                SecurePacketDiscardKind::MalformedPacket,
                anyhow::anyhow!("SRTP 数据报短于 RTP 固定头和认证标签"),
            ));
        }
        let authenticated_len = packet.len() - SRTP_AUTHENTICATION_TAG_LEN;
        if let Err(error) = rtp_payload_offset(&packet[..authenticated_len]) {
            return Err(secure_packet_discard_error(
                SecurePacketDiscardKind::MalformedPacket,
                error,
            ));
        }
        let sequence = u16::from_be_bytes(
            packet[RTP_SEQUENCE_OFFSET..RTP_SEQUENCE_OFFSET + RTP_SEQUENCE_LEN]
                .try_into()
                .expect("已验证 RTP 固定头长度"),
        );
        let rollover_counter = self.estimate_rollover_counter(sequence)?;
        if let Err(error) = verify_authentication_tag_with_suffix(
            packet,
            authenticated_len,
            &rollover_counter.to_be_bytes(),
            &self.keys.authentication_key,
        ) {
            return Err(secure_packet_discard_error(
                SecurePacketDiscardKind::AuthenticationFailed,
                error,
            ));
        }
        let packet_index = (u64::from(rollover_counter) << u16::BITS) | u64::from(sequence);
        if self.is_replay_or_too_old(packet_index) {
            return Ok(None);
        }

        let mut plaintext = packet[..authenticated_len].to_vec();
        crypt_rtp_packet_in_place(&mut plaintext, &self.keys, rollover_counter)?;
        if let Err(error) = validate_rtp_packet(&plaintext) {
            return Err(secure_packet_discard_error(
                SecurePacketDiscardKind::MalformedPacket,
                error,
            ));
        }
        self.accept_packet_index(packet_index);
        Ok(Some(plaintext))
    }

    fn estimate_rollover_counter(&self, sequence: u16) -> Result<u32> {
        let Some(highest_packet_index) = self.highest_packet_index else {
            return Ok(self.initial_rollover_counter);
        };
        let highest_sequence = highest_packet_index as u16;
        let highest_rollover_counter = u32::try_from(highest_packet_index >> u16::BITS)
            .context("SRTP rollover counter 超过 u32")?;
        if highest_sequence < RTP_SEQUENCE_HALF_SPACE
            && sequence > highest_sequence.saturating_add(RTP_SEQUENCE_HALF_SPACE)
        {
            return Ok(highest_rollover_counter.saturating_sub(1));
        }
        if highest_sequence >= RTP_SEQUENCE_HALF_SPACE
            && sequence < highest_sequence - RTP_SEQUENCE_HALF_SPACE
        {
            return highest_rollover_counter
                .checked_add(1)
                .context("SRTP rollover counter 溢出");
        }
        Ok(highest_rollover_counter)
    }

    fn is_replay_or_too_old(&self, packet_index: u64) -> bool {
        let Some(highest_packet_index) = self.highest_packet_index else {
            return false;
        };
        if packet_index > highest_packet_index {
            return false;
        }
        let age = highest_packet_index - packet_index;
        age >= SRTP_REPLAY_WINDOW_BITS || self.replay_window.contains(age)
    }

    fn accept_packet_index(&mut self, packet_index: u64) {
        match self.highest_packet_index {
            None => {
                self.highest_packet_index = Some(packet_index);
                self.replay_window.mark(0);
            }
            Some(highest_packet_index) if packet_index > highest_packet_index => {
                let advance = packet_index - highest_packet_index;
                self.replay_window.advance_and_mark_newest(advance);
                self.highest_packet_index = Some(packet_index);
            }
            Some(highest_packet_index) => {
                let age = highest_packet_index - packet_index;
                self.replay_window.mark(age);
            }
        }
    }
}

pub struct SrtcpSender {
    keys: SrtpSessionKeys,
    next_index: u32,
}

pub struct SrtcpReceiver {
    keys: SrtpSessionKeys,
    highest_index: Option<u32>,
    replay_window: u64,
}

impl SrtcpReceiver {
    pub fn new(keys: SrtpSessionKeys) -> Self {
        Self {
            keys,
            highest_index: None,
            replay_window: 0,
        }
    }

    pub fn open(&mut self, packet: &[u8]) -> Result<Option<Vec<u8>>> {
        if packet.len()
            < SRTCP_UNENCRYPTED_PREFIX_LEN + SRTCP_INDEX_LEN + SRTP_AUTHENTICATION_TAG_LEN
        {
            return Err(secure_packet_discard_error(
                SecurePacketDiscardKind::MalformedPacket,
                anyhow::anyhow!("SRTCP 数据报短于明文前缀、索引和认证标签"),
            ));
        }
        let authenticated_len = packet.len() - SRTP_AUTHENTICATION_TAG_LEN;
        if let Err(error) =
            verify_authentication_tag(packet, authenticated_len, &self.keys.authentication_key)
        {
            return Err(secure_packet_discard_error(
                SecurePacketDiscardKind::AuthenticationFailed,
                error,
            ));
        }
        let index_offset = authenticated_len - SRTCP_INDEX_LEN;
        let tagged_index = u32::from_be_bytes(
            packet[index_offset..authenticated_len]
                .try_into()
                .expect("已验证 SRTCP 索引长度"),
        );
        if tagged_index & SRTCP_ENCRYPTED_FLAG == 0 {
            return Err(secure_packet_discard_error(
                SecurePacketDiscardKind::MalformedPacket,
                anyhow::anyhow!("当前 cipher suite 只接受加密 SRTCP"),
            ));
        }
        let index = tagged_index & SRTCP_INDEX_MASK;
        if self.is_replay_or_too_old(index) {
            return Ok(None);
        }

        let ssrc = read_ssrc(packet, RTCP_SSRC_OFFSET);
        let counter = session_counter(&self.keys.salt, ssrc, u64::from(index))?;
        let mut plaintext = packet[..index_offset].to_vec();
        crypt_aes_counter(
            &mut plaintext[SRTCP_UNENCRYPTED_PREFIX_LEN..],
            &self.keys.encryption_key,
            counter,
        );
        if let Err(error) = validate_rtcp_packet(&plaintext) {
            return Err(secure_packet_discard_error(
                SecurePacketDiscardKind::MalformedPacket,
                error,
            ));
        }
        self.accept_index(index);
        Ok(Some(plaintext))
    }

    fn is_replay_or_too_old(&self, index: u32) -> bool {
        let Some(highest_index) = self.highest_index else {
            return false;
        };
        if index > highest_index {
            return false;
        }
        let age = highest_index - index;
        age >= SRTCP_REPLAY_WINDOW_BITS || self.replay_window & (1u64 << age) != 0
    }

    fn accept_index(&mut self, index: u32) {
        match self.highest_index {
            None => {
                self.highest_index = Some(index);
                self.replay_window = 1;
            }
            Some(highest_index) if index > highest_index => {
                let advance = index - highest_index;
                self.replay_window = if advance >= SRTCP_REPLAY_WINDOW_BITS {
                    1
                } else {
                    (self.replay_window << advance) | 1
                };
                self.highest_index = Some(index);
            }
            Some(highest_index) => {
                let age = highest_index - index;
                self.replay_window |= 1u64 << age;
            }
        }
    }
}

impl SrtcpSender {
    pub fn new(keys: SrtpSessionKeys) -> Self {
        // 当前 AVConference 在零初始化的发送上下文上先递增再使用索引。
        const FIRST_APPLE_SRTCP_SEND_INDEX: u32 = 1;
        Self {
            keys,
            next_index: FIRST_APPLE_SRTCP_SEND_INDEX,
        }
    }

    pub fn protect(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        validate_rtcp_packet(packet)?;
        ensure!(
            self.next_index <= APPLE_SRTCP_MAX_SEND_INDEX,
            "SRTCP 发送索引已耗尽"
        );
        let index = self.next_index;
        self.next_index = self
            .next_index
            .checked_add(1)
            .context("SRTCP 发送索引溢出")?;
        let ssrc = read_ssrc(packet, RTCP_SSRC_OFFSET);
        let counter = session_counter(&self.keys.salt, ssrc, u64::from(index))?;
        let mut protected = packet.to_vec();
        crypt_aes_counter(
            &mut protected[SRTCP_UNENCRYPTED_PREFIX_LEN..],
            &self.keys.encryption_key,
            counter,
        );
        protected.extend_from_slice(&(SRTCP_ENCRYPTED_FLAG | index).to_be_bytes());
        append_authentication_tag(&mut protected, &self.keys.authentication_key);
        Ok(protected)
    }
}

type HmacSha1 = Hmac<Sha1>;

fn append_authentication_tag(
    packet: &mut Vec<u8>,
    authentication_key: &[u8; SRTP_SESSION_AUTHENTICATION_KEY_LEN],
) {
    let tag = authentication_tag(packet, authentication_key);
    packet.extend_from_slice(&tag);
}

fn verify_authentication_tag(
    packet: &[u8],
    authenticated_len: usize,
    authentication_key: &[u8; SRTP_SESSION_AUTHENTICATION_KEY_LEN],
) -> Result<()> {
    ensure!(
        packet.len() == authenticated_len + SRTP_AUTHENTICATION_TAG_LEN,
        "SRTP 认证边界不一致"
    );
    let expected = authentication_tag(&packet[..authenticated_len], authentication_key);
    let received = &packet[authenticated_len..];
    let difference = expected
        .iter()
        .zip(received)
        .fold(0u8, |difference, (expected, received)| {
            difference | (expected ^ received)
        });
    ensure!(difference == 0, "SRTP HMAC-SHA1-80 认证失败");
    Ok(())
}

fn authentication_tag(
    packet: &[u8],
    authentication_key: &[u8; SRTP_SESSION_AUTHENTICATION_KEY_LEN],
) -> [u8; SRTP_AUTHENTICATION_TAG_LEN] {
    authentication_tag_with_suffix(packet, &[], authentication_key)
}

fn authentication_tag_with_suffix(
    packet: &[u8],
    suffix: &[u8],
    authentication_key: &[u8; SRTP_SESSION_AUTHENTICATION_KEY_LEN],
) -> [u8; SRTP_AUTHENTICATION_TAG_LEN] {
    let mut mac = <HmacSha1 as Mac>::new_from_slice(authentication_key)
        .expect("HMAC-SHA1 会话认证密钥长度固定");
    mac.update(packet);
    mac.update(suffix);
    let full_tag = mac.finalize().into_bytes();
    let mut tag = [0u8; SRTP_AUTHENTICATION_TAG_LEN];
    tag.copy_from_slice(&full_tag[..SRTP_AUTHENTICATION_TAG_LEN]);
    tag
}

fn verify_authentication_tag_with_suffix(
    packet: &[u8],
    authenticated_len: usize,
    suffix: &[u8],
    authentication_key: &[u8; SRTP_SESSION_AUTHENTICATION_KEY_LEN],
) -> Result<()> {
    ensure!(
        packet.len() == authenticated_len + SRTP_AUTHENTICATION_TAG_LEN,
        "SRTP 认证边界不一致"
    );
    let expected =
        authentication_tag_with_suffix(&packet[..authenticated_len], suffix, authentication_key);
    let received = &packet[authenticated_len..];
    let difference = expected
        .iter()
        .zip(received)
        .fold(0u8, |difference, (expected, received)| {
            difference | (expected ^ received)
        });
    ensure!(difference == 0, "SRTP HMAC-SHA1-80 认证失败");
    Ok(())
}

/// 构造 Apple `_RTPSendRTCP` 同构的 RR + SDES/CNAME compound packet。
pub fn build_compound_rtcp_receiver_report(local_ssrc: u32) -> Vec<u8> {
    let mut compound = vec![0u8; SRTCP_UNENCRYPTED_PREFIX_LEN];
    compound[0] = RTCP_VERSION_AND_ZERO_REPORTS;
    compound[1] = RTCP_RECEIVER_REPORT_PACKET_TYPE;
    compound[2..4].copy_from_slice(&RTCP_RECEIVER_REPORT_WORDS_MINUS_ONE.to_be_bytes());
    compound[RTCP_SSRC_OFFSET..RTCP_SSRC_OFFSET + SSRC_LEN]
        .copy_from_slice(&local_ssrc.to_be_bytes());

    let cname = format!("{SCREEN_SHARING_CNAME_PREFIX}{local_ssrc:08x}");
    let cname_len = u8::try_from(cname.len()).expect("固定前缀加十六进制 SSRC 可放入 SDES 长度");
    let sdes_start = compound.len();
    compound.extend_from_slice(&[
        RTCP_VERSION_AND_ZERO_REPORTS | RTCP_SOURCE_DESCRIPTION_SOURCE_COUNT,
        RTCP_SOURCE_DESCRIPTION_PACKET_TYPE,
        0,
        0,
    ]);
    compound.extend_from_slice(&local_ssrc.to_be_bytes());
    compound.push(RTCP_SOURCE_DESCRIPTION_CNAME_ITEM);
    compound.push(cname_len);
    compound.extend_from_slice(cname.as_bytes());
    compound.push(RTCP_SOURCE_DESCRIPTION_END_ITEM);
    while !(compound.len() - sdes_start).is_multiple_of(RTCP_WORD_LEN) {
        compound.push(0);
    }
    let sdes_words_minus_one = (compound.len() - sdes_start) / RTCP_WORD_LEN - 1;
    let sdes_words_minus_one =
        u16::try_from(sdes_words_minus_one).expect("固定 SDES packet 长度可放入 u16");
    compound[sdes_start + 2..sdes_start + RTCP_COMMON_HEADER_LEN]
        .copy_from_slice(&sdes_words_minus_one.to_be_bytes());
    compound
}

fn derive_key<const OUTPUT_LEN: usize>(
    master_key: &[u8; SRTP_SESSION_ENCRYPTION_KEY_LEN],
    master_salt: &[u8; SRTP_SESSION_SALT_LEN],
    label: SrtpKdfLabel,
) -> [u8; OUTPUT_LEN] {
    let cipher = Aes256::new_from_slice(master_key).expect("AES-256 主密钥长度固定");
    let mut result = [0u8; OUTPUT_LEN];
    for (block_index, output) in result.chunks_mut(AES_BLOCK_LEN).enumerate() {
        let mut input = [0u8; AES_BLOCK_LEN];
        input[..SRTP_SESSION_SALT_LEN].copy_from_slice(master_salt);
        input[SRTP_KDF_LABEL_OFFSET] ^= label as u8;
        input[SRTP_KDF_BLOCK_INDEX_OFFSET] =
            u8::try_from(block_index).expect("当前会话密钥长度不会耗尽 KDF block index");
        let mut block = input.into();
        cipher.encrypt_block(&mut block);
        output.copy_from_slice(&block[..output.len()]);
    }
    result
}

fn session_counter(
    session_salt: &[u8; SRTP_SESSION_SALT_LEN],
    ssrc: u32,
    packet_index: u64,
) -> Result<[u8; AES_BLOCK_LEN]> {
    ensure!(
        packet_index < (1u64 << (SRTP_PACKET_INDEX_LEN * u8::BITS as usize)),
        "SRTP 包索引超过 48 位"
    );
    let mut counter = [0u8; AES_BLOCK_LEN];
    counter[..SRTP_SESSION_SALT_LEN].copy_from_slice(session_salt);
    xor_at(&mut counter, SRTP_COUNTER_SSRC_OFFSET, &ssrc.to_be_bytes());
    let packet_index_bytes = packet_index.to_be_bytes();
    xor_at(
        &mut counter,
        SRTP_COUNTER_PACKET_INDEX_OFFSET,
        &packet_index_bytes[packet_index_bytes.len() - SRTP_PACKET_INDEX_LEN..],
    );
    debug_assert!(counter[AES_BLOCK_LEN - SRTP_COUNTER_BLOCK_INDEX_LEN..]
        .iter()
        .all(|byte| *byte == 0));
    Ok(counter)
}

fn xor_at(destination: &mut [u8], offset: usize, source: &[u8]) {
    for (target, value) in destination[offset..offset + source.len()]
        .iter_mut()
        .zip(source)
    {
        *target ^= *value;
    }
}

fn crypt_aes_counter(
    payload: &mut [u8],
    encryption_key: &[u8; SRTP_SESSION_ENCRYPTION_KEY_LEN],
    mut counter: [u8; AES_BLOCK_LEN],
) {
    let cipher = Aes256::new_from_slice(encryption_key).expect("AES-256 会话密钥长度固定");
    for payload_block in payload.chunks_mut(AES_BLOCK_LEN) {
        let mut key_stream = counter.into();
        cipher.encrypt_block(&mut key_stream);
        for (byte, mask) in payload_block.iter_mut().zip(key_stream.iter()) {
            *byte ^= *mask;
        }
        increment_counter(&mut counter);
    }
}

fn increment_counter(counter: &mut [u8; AES_BLOCK_LEN]) {
    for byte in counter.iter_mut().rev() {
        let (next, carried) = byte.overflowing_add(1);
        *byte = next;
        if !carried {
            break;
        }
    }
}

fn rtp_payload_offset(packet: &[u8]) -> Result<usize> {
    ensure!(packet.len() >= RTP_FIXED_HEADER_LEN, "RTP 固定头截断");
    ensure!(rtp_version(packet[0]) == RTP_VERSION, "RTP version 不是 2");
    let csrc_count = usize::from(packet[0] & RTP_CSRC_COUNT_MASK);
    let csrc_bytes = csrc_count
        .checked_mul(SSRC_LEN)
        .context("RTP CSRC 长度溢出")?;
    let mut offset = RTP_FIXED_HEADER_LEN
        .checked_add(csrc_bytes)
        .context("RTP 头长度溢出")?;
    ensure!(offset <= packet.len(), "RTP CSRC 列表截断");
    if packet[0] & RTP_EXTENSION_FLAG != 0 {
        let extension_header_end = offset
            .checked_add(RTP_EXTENSION_HEADER_LEN)
            .context("RTP extension 头长度溢出")?;
        ensure!(extension_header_end <= packet.len(), "RTP extension 头截断");
        let extension_words = u16::from_be_bytes(
            packet[offset + RTP_EXTENSION_LENGTH_OFFSET..offset + RTP_EXTENSION_LENGTH_OFFSET + 2]
                .try_into()
                .expect("已验证 RTP extension 头长度"),
        );
        let extension_bytes = usize::from(extension_words)
            .checked_mul(RTP_EXTENSION_WORD_BYTES)
            .context("RTP extension 数据长度溢出")?;
        offset = extension_header_end
            .checked_add(extension_bytes)
            .context("RTP extension 总长度溢出")?;
        ensure!(offset <= packet.len(), "RTP extension 数据截断");
    }
    Ok(offset)
}

fn validate_rtp_packet(packet: &[u8]) -> Result<()> {
    let payload_offset = rtp_payload_offset(packet)?;
    if packet[0] & RTP_PADDING_FLAG != 0 {
        let padding_length = usize::from(*packet.last().context("带 padding 的 RTP 数据报为空")?);
        ensure!(padding_length != 0, "RTP padding 长度不能为零");
        ensure!(
            padding_length <= packet.len().saturating_sub(payload_offset),
            "RTP padding 长度越界"
        );
    }
    Ok(())
}

fn validate_rtcp_packet(packet: &[u8]) -> Result<()> {
    ensure!(
        packet.len() >= SRTCP_UNENCRYPTED_PREFIX_LEN,
        "RTCP 数据报短于公共头和 SSRC"
    );
    let mut offset = 0usize;
    while offset < packet.len() {
        let remaining = &packet[offset..];
        ensure!(
            remaining.len() >= RTCP_COMMON_HEADER_LEN,
            "RTCP compound packet 的公共头截断"
        );
        ensure!(
            rtp_version(remaining[0]) == RTP_VERSION,
            "RTCP version 不是 2"
        );
        let words_minus_one = u16::from_be_bytes(
            remaining[RTCP_LENGTH_OFFSET..RTCP_LENGTH_OFFSET + size_of::<u16>()]
                .try_into()
                .expect("已验证 RTCP 公共头长度"),
        );
        let rtcp_packet_len = (usize::from(words_minus_one) + 1)
            .checked_mul(RTCP_WORD_LEN)
            .context("RTCP packet 长度溢出")?;
        ensure!(
            rtcp_packet_len >= RTCP_COMMON_HEADER_LEN && rtcp_packet_len <= remaining.len(),
            "RTCP packet 声明长度越界"
        );
        offset += rtcp_packet_len;
    }
    ensure!(offset == packet.len(), "RTCP compound packet 边界不一致");
    Ok(())
}

const fn rtp_version(first_header_byte: u8) -> u8 {
    (first_header_byte >> RTP_VERSION_SHIFT) & RTP_VERSION_MASK
}

fn read_ssrc(packet: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(
        packet[offset..offset + SSRC_LEN]
            .try_into()
            .expect("调用前已验证 SSRC 范围"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SSRC: u32 = 0x1122_3344;

    fn test_material() -> SrtpMasterMaterial {
        let mut master_key = [0u8; 32];
        for (value, byte) in master_key.iter_mut().enumerate() {
            *byte = value as u8;
        }
        let mut master_salt = [0u8; 14];
        for (value, byte) in master_salt.iter_mut().enumerate() {
            *byte = 0x10 + value as u8;
        }
        SrtpMasterMaterial {
            master_key,
            master_salt,
        }
    }

    fn protected_test_rtp(
        keys: &SrtpSessionKeys,
        sequence: u16,
        rollover_counter: u32,
    ) -> (Vec<u8>, Vec<u8>) {
        let mut plaintext = vec![RTP_VERSION << RTP_VERSION_SHIFT, 101];
        plaintext.extend_from_slice(&sequence.to_be_bytes());
        plaintext.extend_from_slice(&u32::from(sequence).wrapping_mul(960).to_be_bytes());
        plaintext.extend_from_slice(&TEST_SSRC.to_be_bytes());
        plaintext.extend_from_slice(b"replay-window");
        let protected = protect_rtp_packet(&plaintext, keys, rollover_counter).unwrap();
        (plaintext, protected)
    }

    #[test]
    fn apple_suite_five_kdf_uses_distinct_rtp_and_rtcp_labels() {
        // 期望值由独立 Python cryptography AES-ECB 计算，并逐指令对照
        // AVConference `_MakeSessionKey` 的 salt[7] XOR label/block-index 布局。
        let material = test_material();
        let rtp = derive_session_keys(&material, SrtpPacketKind::Rtp);
        assert_eq!(
            rtp.encryption_key,
            hex_array::<32>("4f3a1ce39ab7f0bf88b65ef0efbf3e42cf91496b5da3c4e9b15aa8071f2574f6")
        );
        assert_eq!(rtp.salt, hex_array::<14>("7e6f916d02536c2c2c0f06a57bd1"));
        assert_eq!(
            rtp.authentication_key,
            hex_array::<20>("564354455979058abf8d82d45ffddafe3e8df9b3")
        );

        let rtcp = derive_session_keys(&material, SrtpPacketKind::Rtcp);
        assert_eq!(
            rtcp.encryption_key,
            hex_array::<32>("70a1c38b3589a6ad39614d96fc8a3aad532ffa918310e2ffcb2d2af54da0f22d")
        );
        assert_eq!(rtcp.salt, hex_array::<14>("8f53fc2163ed648181b15156b06d"));
        assert_eq!(
            rtcp.authentication_key,
            hex_array::<20>("6c3a48e3520a99ae57eb0495553e9742c69482e3")
        );
    }

    #[test]
    fn rtp_keeps_header_plain_and_crypts_payload_with_roc_sequence_index() {
        const RTP_VERSION_AND_FLAGS: u8 = 2 << 6;
        const RTP_PAYLOAD_TYPE: u8 = 101;
        const RTP_SEQUENCE: u16 = 0x99aa;
        const RTP_TIMESTAMP: u32 = 0x0102_0304;
        const RTP_ROLLOVER_COUNTER: u32 = 0x5566_7788;
        let header = [
            RTP_VERSION_AND_FLAGS,
            RTP_PAYLOAD_TYPE,
            RTP_SEQUENCE.to_be_bytes()[0],
            RTP_SEQUENCE.to_be_bytes()[1],
            RTP_TIMESTAMP.to_be_bytes()[0],
            RTP_TIMESTAMP.to_be_bytes()[1],
            RTP_TIMESTAMP.to_be_bytes()[2],
            RTP_TIMESTAMP.to_be_bytes()[3],
            TEST_SSRC.to_be_bytes()[0],
            TEST_SSRC.to_be_bytes()[1],
            TEST_SSRC.to_be_bytes()[2],
            TEST_SSRC.to_be_bytes()[3],
        ];
        let plaintext = b"known Apple SRTP payload";
        let mut packet = header.to_vec();
        packet.extend_from_slice(plaintext);
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtp);

        crypt_rtp_packet_in_place(&mut packet, &keys, RTP_ROLLOVER_COUNTER).unwrap();
        assert_eq!(&packet[..header.len()], &header);
        assert_eq!(
            &packet[header.len()..],
            &hex_array::<24>("44e44a8a4585daf58027769a864e0a01293e76f27c350b05")
        );
        crypt_rtp_packet_in_place(&mut packet, &keys, RTP_ROLLOVER_COUNTER).unwrap();
        assert_eq!(&packet[header.len()..], plaintext);
    }

    #[test]
    fn srtp_authenticates_ciphertext_with_roc_and_drops_authenticated_replay() {
        const RTP_VERSION_AND_FLAGS: u8 = 2 << 6;
        const RTP_PAYLOAD_TYPE: u8 = 101;
        const RTP_SEQUENCE: u16 = 0x99aa;
        const RTP_TIMESTAMP: u32 = 0x0102_0304;
        const RTP_ROLLOVER_COUNTER: u32 = 0x5566_7788;
        let mut plaintext = vec![RTP_VERSION_AND_FLAGS, RTP_PAYLOAD_TYPE];
        plaintext.extend_from_slice(&RTP_SEQUENCE.to_be_bytes());
        plaintext.extend_from_slice(&RTP_TIMESTAMP.to_be_bytes());
        plaintext.extend_from_slice(&TEST_SSRC.to_be_bytes());
        plaintext.extend_from_slice(b"known Apple SRTP payload");
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtp);

        let protected = protect_rtp_packet(&plaintext, &keys, RTP_ROLLOVER_COUNTER).unwrap();
        assert_eq!(
            &protected[protected.len() - SRTP_AUTHENTICATION_TAG_LEN..],
            &hex_array::<10>("40139f98be26c100131a")
        );
        let mut receiver = SrtpReceiver::with_initial_rollover_counter(keys, RTP_ROLLOVER_COUNTER);
        assert_eq!(receiver.open(&protected).unwrap(), Some(plaintext));
        assert_eq!(receiver.open(&protected).unwrap(), None);

        let mut tampered = protected;
        *tampered.last_mut().unwrap() ^= 1;
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtp);
        let mut receiver = SrtpReceiver::with_initial_rollover_counter(keys, RTP_ROLLOVER_COUNTER);
        assert!(receiver.open(&tampered).is_err());
    }

    #[test]
    fn srtp_receiver_rejects_authenticated_malformed_packet_without_advancing_replay_state() {
        const RTP_SEQUENCE: u16 = 5;
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtp);
        let mut malformed = vec![(RTP_VERSION << RTP_VERSION_SHIFT) | RTP_PADDING_FLAG, 101];
        malformed.extend_from_slice(&RTP_SEQUENCE.to_be_bytes());
        malformed.extend_from_slice(&960u32.to_be_bytes());
        malformed.extend_from_slice(&TEST_SSRC.to_be_bytes());
        malformed.extend_from_slice(&[0xaa, 3]);
        let malformed = protect_rtp_packet(&malformed, &keys, 0).unwrap();

        let mut valid = vec![RTP_VERSION << RTP_VERSION_SHIFT, 101];
        valid.extend_from_slice(&RTP_SEQUENCE.to_be_bytes());
        valid.extend_from_slice(&960u32.to_be_bytes());
        valid.extend_from_slice(&TEST_SSRC.to_be_bytes());
        valid.extend_from_slice(b"valid");
        let protected_valid = protect_rtp_packet(&valid, &keys, 0).unwrap();

        let mut receiver = SrtpReceiver::new(keys);
        assert!(receiver.open(&malformed).is_err());
        assert_eq!(receiver.open(&protected_valid).unwrap(), Some(valid));
    }

    #[test]
    fn srtp_receiver_accepts_out_of_order_packets_through_age_255_once() {
        const HIGHEST_SEQUENCE: u16 = 1_000;
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtp);
        let (highest_plaintext, highest) = protected_test_rtp(&keys, HIGHEST_SEQUENCE, 0);
        let mut receiver = SrtpReceiver::new(keys.clone());
        assert_eq!(receiver.open(&highest).unwrap(), Some(highest_plaintext));

        for age in [63u16, 64, 255] {
            let sequence = HIGHEST_SEQUENCE - age;
            let (plaintext, packet) = protected_test_rtp(&keys, sequence, 0);
            assert_eq!(receiver.open(&packet).unwrap(), Some(plaintext));
            assert_eq!(receiver.open(&packet).unwrap(), None);
        }

        let (_, age_256) = protected_test_rtp(&keys, HIGHEST_SEQUENCE - 256, 0);
        assert_eq!(receiver.open(&age_256).unwrap(), None);
    }

    #[test]
    fn srtp_receiver_resets_replay_bits_after_forward_advance_of_full_window() {
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtp);
        let (first_plaintext, first) = protected_test_rtp(&keys, 10, 0);
        let (advanced_plaintext, advanced) = protected_test_rtp(&keys, 300, 0);
        let (edge_plaintext, edge) = protected_test_rtp(&keys, 45, 0);
        let mut receiver = SrtpReceiver::new(keys);

        assert_eq!(receiver.open(&first).unwrap(), Some(first_plaintext));
        assert_eq!(receiver.open(&advanced).unwrap(), Some(advanced_plaintext));
        assert_eq!(receiver.open(&edge).unwrap(), Some(edge_plaintext));
        assert_eq!(receiver.open(&edge).unwrap(), None);
        assert_eq!(receiver.open(&first).unwrap(), None);
    }

    #[test]
    fn srtp_receiver_tracks_256_packet_window_across_roc_wrap() {
        const INITIAL_ROC: u32 = 7;
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtp);
        let (before_wrap_plaintext, before_wrap) =
            protected_test_rtp(&keys, u16::MAX - 5, INITIAL_ROC);
        let (after_wrap_plaintext, after_wrap) = protected_test_rtp(&keys, 5, INITIAL_ROC + 1);
        let (delayed_plaintext, delayed) = protected_test_rtp(&keys, u16::MAX - 4, INITIAL_ROC);
        let (_, too_old) = protected_test_rtp(&keys, 65_285, INITIAL_ROC);
        let mut receiver = SrtpReceiver::with_initial_rollover_counter(keys, INITIAL_ROC);

        assert_eq!(
            receiver.open(&before_wrap).unwrap(),
            Some(before_wrap_plaintext)
        );
        assert_eq!(
            receiver.open(&after_wrap).unwrap(),
            Some(after_wrap_plaintext)
        );
        assert_eq!(receiver.open(&delayed).unwrap(), Some(delayed_plaintext));
        assert_eq!(receiver.open(&delayed).unwrap(), None);
        assert_eq!(receiver.open(&too_old).unwrap(), None);
    }

    #[test]
    fn srtp_authentication_failure_does_not_advance_replay_window() {
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtp);
        let (_, mut unauthenticated_highest) = protected_test_rtp(&keys, 1_000, 0);
        *unauthenticated_highest.last_mut().unwrap() ^= 1;
        let (older_plaintext, older) = protected_test_rtp(&keys, 744, 0);
        let mut receiver = SrtpReceiver::new(keys);

        assert!(receiver.open(&unauthenticated_highest).is_err());
        assert_eq!(receiver.open(&older).unwrap(), Some(older_plaintext));
        assert_eq!(receiver.open(&older).unwrap(), None);
    }

    #[test]
    fn srtp_roc_zero_fixture_for_apple_offline_verifier_is_stable() {
        const RTP_VERSION_AND_FLAGS: u8 = RTP_VERSION << RTP_VERSION_SHIFT;
        const RTP_PAYLOAD_TYPE: u8 = 101;
        const RTP_SEQUENCE: u16 = 0x1234;
        const RTP_TIMESTAMP: u32 = 0x0102_0304;
        const RTP_ROLLOVER_COUNTER: u32 = 0;
        const PLAINTEXT_PAYLOAD: &[u8] = b"FreeRemoteDesk SRTP ROC zero";
        const EXPECTED_PROTECTED_PACKET_HEX: &str =
            "806512340102030411223344cc183721897ab7586d8a366468d24598c4137beec52224ccc5cf2488b553cee132448d4cb4f2";

        let mut plaintext = vec![RTP_VERSION_AND_FLAGS, RTP_PAYLOAD_TYPE];
        plaintext.extend_from_slice(&RTP_SEQUENCE.to_be_bytes());
        plaintext.extend_from_slice(&RTP_TIMESTAMP.to_be_bytes());
        plaintext.extend_from_slice(&TEST_SSRC.to_be_bytes());
        plaintext.extend_from_slice(PLAINTEXT_PAYLOAD);
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtp);

        let protected = protect_rtp_packet(&plaintext, &keys, RTP_ROLLOVER_COUNTER).unwrap();
        assert_eq!(protected, hex_array::<50>(EXPECTED_PROTECTED_PACKET_HEX));
    }

    #[test]
    fn srtcp_preserves_common_header_encrypts_body_and_appends_encrypted_index() {
        const RTCP_VERSION_AND_REPORT_COUNT: u8 = 2 << 6;
        const RTCP_RECEIVER_REPORT: u8 = 201;
        const RTCP_LENGTH_WORDS_MINUS_ONE: u16 = 3;
        let mut report = vec![RTCP_VERSION_AND_REPORT_COUNT, RTCP_RECEIVER_REPORT];
        report.extend_from_slice(&RTCP_LENGTH_WORDS_MINUS_ONE.to_be_bytes());
        report.extend_from_slice(&TEST_SSRC.to_be_bytes());
        report.extend_from_slice(&hex_array::<8>("0102030405060708"));
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtcp);
        let mut sender = SrtcpSender::new(keys.clone());

        let protected = sender.protect(&report).unwrap();
        assert_eq!(&protected[..8], &report[..8]);
        assert_eq!(&protected[8..16], &hex_array::<8>("99fab37a35acad52"));
        assert_eq!(&protected[16..20], &0x8000_0001u32.to_be_bytes());
        assert_eq!(&protected[20..], &hex_array::<10>("f9471020dc5900d4af36"));
        let mut receiver = SrtcpReceiver::new(keys.clone());
        assert_eq!(receiver.open(&protected).unwrap(), Some(report));

        let mut tampered = protected;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(receiver.open(&tampered).is_err());
    }

    #[test]
    fn srtcp_receiver_authenticates_before_replay_and_advances_only_after_validation() {
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtcp);
        let report = build_compound_rtcp_receiver_report(TEST_SSRC);
        let mut sender = SrtcpSender::new(keys.clone());
        let protected = sender.protect(&report).unwrap();
        let mut receiver = SrtcpReceiver::new(keys.clone());

        let authenticated_len = protected.len() - SRTP_AUTHENTICATION_TAG_LEN;
        let mut malformed = protected.clone();
        malformed[0] = 1 << RTP_VERSION_SHIFT;
        let replacement_tag =
            authentication_tag(&malformed[..authenticated_len], &keys.authentication_key);
        malformed[authenticated_len..].copy_from_slice(&replacement_tag);
        assert!(receiver.open(&malformed).is_err());

        let mut bad_tag = protected.clone();
        *bad_tag.last_mut().unwrap() ^= 1;
        assert!(receiver.open(&bad_tag).is_err());

        assert_eq!(receiver.open(&protected).unwrap(), Some(report));
        assert!(receiver.open(&bad_tag).is_err());
        assert_eq!(receiver.open(&protected).unwrap(), None);
    }

    #[test]
    fn srtcp_receiver_uses_a_sixty_four_packet_replay_window() {
        const PACKET_COUNT: usize = 66;
        let keys = derive_session_keys(&test_material(), SrtpPacketKind::Rtcp);
        let report = build_compound_rtcp_receiver_report(TEST_SSRC);
        let mut sender = SrtcpSender::new(keys.clone());
        let packets: Vec<_> = (0..PACKET_COUNT)
            .map(|_| sender.protect(&report).unwrap())
            .collect();
        let mut receiver = SrtcpReceiver::new(keys);

        assert_eq!(receiver.open(&packets[65]).unwrap(), Some(report.clone()));
        assert_eq!(receiver.open(&packets[2]).unwrap(), Some(report));
        assert_eq!(receiver.open(&packets[1]).unwrap(), None);
        assert_eq!(receiver.open(&packets[2]).unwrap(), None);
    }

    #[test]
    fn initial_receiver_report_is_compound_with_sdes_cname() {
        let compound = build_compound_rtcp_receiver_report(TEST_SSRC);
        const RECEIVER_REPORT_LEN: usize = 8;
        assert_eq!(&compound[..2], &[0x80, 201]);
        assert_eq!(&compound[4..RECEIVER_REPORT_LEN], &TEST_SSRC.to_be_bytes());
        assert_eq!(
            &compound[RECEIVER_REPORT_LEN..RECEIVER_REPORT_LEN + 2],
            &[0x81, 202]
        );
        assert_eq!(compound.len() % RTCP_WORD_LEN, 0);
        assert_eq!(
            compound[RECEIVER_REPORT_LEN + 8],
            RTCP_SOURCE_DESCRIPTION_CNAME_ITEM
        );
        assert!(compound
            .windows(SCREEN_SHARING_CNAME_PREFIX.len())
            .any(|window| window == SCREEN_SHARING_CNAME_PREFIX.as_bytes()));
    }

    #[test]
    fn parses_receiver_report_block_for_the_outbound_media_ssrc() {
        const REPORTER_SSRC: u32 = 0xaabb_ccdd;
        const OUTBOUND_MEDIA_SSRC: u32 = 0x1020_3040;
        const EXTENDED_HIGHEST_SEQUENCE: u32 = 0x0001_0020;
        const INTERARRIVAL_JITTER: u32 = 47;
        let mut receiver_report = vec![0x81, RTCP_RECEIVER_REPORT_PACKET_TYPE, 0, 7];
        receiver_report.extend_from_slice(&REPORTER_SSRC.to_be_bytes());
        receiver_report.extend_from_slice(&OUTBOUND_MEDIA_SSRC.to_be_bytes());
        receiver_report.push(3);
        receiver_report.extend_from_slice(&[0, 0, 2]);
        receiver_report.extend_from_slice(&EXTENDED_HIGHEST_SEQUENCE.to_be_bytes());
        receiver_report.extend_from_slice(&INTERARRIVAL_JITTER.to_be_bytes());
        receiver_report.extend_from_slice(&0u32.to_be_bytes());
        receiver_report.extend_from_slice(&0u32.to_be_bytes());

        let reports = parse_rtcp_reception_reports(&receiver_report).unwrap();

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].reporter_ssrc, REPORTER_SSRC);
        assert_eq!(reports[0].source_ssrc, OUTBOUND_MEDIA_SSRC);
        assert_eq!(reports[0].fraction_lost, 3);
        assert_eq!(reports[0].cumulative_packets_lost, 2);
        assert_eq!(
            reports[0].extended_highest_sequence,
            EXTENDED_HIGHEST_SEQUENCE
        );
        assert_eq!(reports[0].interarrival_jitter, INTERARRIVAL_JITTER);
    }

    #[test]
    fn rtcp_cumulative_loss_decodes_all_signed_24_bit_boundaries() {
        assert_eq!(decode_signed_24_be([0x00, 0x00, 0x00]), 0);
        assert_eq!(decode_signed_24_be([0x7f, 0xff, 0xff]), 8_388_607);
        assert_eq!(decode_signed_24_be([0xff, 0xff, 0xff]), -1);
        assert_eq!(decode_signed_24_be([0x80, 0x00, 0x00]), -8_388_608);
    }

    #[test]
    fn rtp_mux_classifier_validates_version_and_exact_rtcp_range() {
        assert_eq!(
            classify_rtp_mux_packet(&[0x80, 191]).unwrap(),
            RtpMuxPacketKind::Rtp
        );
        assert_eq!(
            classify_rtp_mux_packet(&[0x80, 192]).unwrap(),
            RtpMuxPacketKind::Rtcp
        );
        assert_eq!(
            classify_rtp_mux_packet(&[0x80, 223]).unwrap(),
            RtpMuxPacketKind::Rtcp
        );
        assert_eq!(
            classify_rtp_mux_packet(&[0x80, 224]).unwrap(),
            RtpMuxPacketKind::Rtp
        );
        assert!(classify_rtp_mux_packet(&[]).is_err());
        assert!(classify_rtp_mux_packet(&[0x80]).is_err());
        assert!(classify_rtp_mux_packet(&[0x40, 200]).is_err());
        assert!(classify_rtp_mux_packet(&[0xc0, 100]).is_err());
    }

    #[test]
    fn malformed_rtp_and_rtcp_are_rejected_before_crypto() {
        let material = test_material();
        let rtp = derive_session_keys(&material, SrtpPacketKind::Rtp);
        let rtcp = derive_session_keys(&material, SrtpPacketKind::Rtcp);
        assert!(crypt_rtp_packet_in_place(&mut [0u8; 11], &rtp, 0).is_err());
        assert!(SrtcpReceiver::new(rtcp).open(&[0u8; 11]).is_err());
    }

    fn hex_array<const LENGTH: usize>(text: &str) -> [u8; LENGTH] {
        assert_eq!(text.len(), LENGTH * 2);
        let mut bytes = [0u8; LENGTH];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap();
        }
        bytes
    }
}
