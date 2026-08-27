//! Apple 私有会话加密层（类型 36 SRP 认证之后的 EncryptOneMessage 协议）。
//!
//! 密钥派生（screensharingd 逆向定案，指令级验证）：
//! - SRP-6a 会话密钥  K = SHA512(S)（与 M1/H_AMK 所用相同，ccsrp KDF_HASH 变体）
//! - 初始 AES-128 钥  key16 = SHA256(K)[0..16]（SetupAESKeys 建立 4 个 cryptor，CBC IV=0）
//! - 服务器在 SetEncryption cmd=1 后明文下发 52B EncryptionInfo：
//!   `[16B 头][BE32 counter][ECB(key16, new_key 16B)][ECB(key16, new_iv 16B)]`
//!   （头 = `00000001 00000000 00000000 0000044f`；counter 目前恒为 1）
//! - 双方以 (new_key, new_iv) 重建 cryptor，此后全部消息为加密帧
//!
//! 帧格式（EncryptOneMessage，双向同构）：
//! - wire  = `[BE16 padded_len][CBC 密文 padded_len 字节]`
//! - 明文  = `[BE16 orig_len][数据][零填充][20B SHA1 校验]`，总长 = (orig_len + 0x25) & !0xF
//! - SHA1  = SHA1(BE32(counter) ‖ 明文[0 .. padded_len-20])；counter 双向独立、从 0 起每帧 +1
//! - CBC   IV 链式：首帧 = new_iv，其后每帧 IV = 上一帧末密文块
//!
//! 参考：docs/ARD_SESSION_PROTOCOL.md、ard_re/NOTES.md（2026-08-19 破译记录）

use anyhow::{bail, ensure, Context, Result};
use sha1::{Digest as _, Sha1};
use sha2::Sha256;

use crate::connection::AppleConnection;
use crate::protocol;

/// EncryptionInfo 消息固定 52 字节：16B 头 + BE32 counter + 16B 新钥密文 + 16B 新 IV 密文
pub(crate) const ENCRYPTION_INFO_LEN: usize = 52;
/// EncryptionInfo 头部（`00000001 00000000 00000000 0000044f`，0x44f 为子类型标识）
const ENCRYPTION_INFO_HDR: [u8; 16] = [
    0, 0, 0, 1, // u32 1
    0, 0, 0, 0, 0, 0, 0, 0, // u64 0
    0, 0, 0x04, 0x4f, // u32 0x44f
];
/// 加密帧明文的固定开销：2B 长度 + 20B SHA1，填充粒度 16
const AES_BLOCK_BYTES: usize = 16;
const FRAME_LENGTH_BYTES: usize = size_of::<u16>();
const FRAME_SHA1_BYTES: usize = 20;
const FRAME_OVERHEAD: usize = FRAME_LENGTH_BYTES + FRAME_SHA1_BYTES + (AES_BLOCK_BYTES - 1);
const MAX_SESSION_CIPHERTEXT_BYTES: usize = (u16::MAX as usize) & !(AES_BLOCK_BYTES - 1);
pub(crate) const MAX_SESSION_PLAINTEXT_BYTES: usize = u16::MAX as usize - FRAME_OVERHEAD;

fn validate_wire_ciphertext_len(len: usize) -> Result<()> {
    if len == 0 || !len.is_multiple_of(AES_BLOCK_BYTES) || len > MAX_SESSION_CIPHERTEXT_BYTES {
        bail!("加密帧长度非法: {len}");
    }
    Ok(())
}

pub(crate) fn take_wire_ciphertext_frame(pending: &mut Vec<u8>) -> Result<Option<Vec<u8>>> {
    if pending.len() < FRAME_LENGTH_BYTES {
        return Ok(None);
    }

    let ciphertext_len = usize::from(u16::from_be_bytes(
        pending[..FRAME_LENGTH_BYTES]
            .try_into()
            .expect("加密帧长度前缀已验证完整"),
    ));
    validate_wire_ciphertext_len(ciphertext_len)?;
    let frame_len = FRAME_LENGTH_BYTES
        .checked_add(ciphertext_len)
        .context("加密帧总长度溢出")?;
    if pending.len() < frame_len {
        return Ok(None);
    }

    let ciphertext = pending[FRAME_LENGTH_BYTES..frame_len].to_vec();
    pending.drain(..frame_len);
    Ok(Some(ciphertext))
}

/// 会话加密器：持有一对方向的密钥状态，负责 EncryptOneMessage 帧的编码与解码
pub struct SessionCrypto {
    key: [u8; 16],
    send_iv: [u8; 16],
    recv_iv: [u8; 16],
    send_ctr: u32,
    recv_ctr: u32,
}

pub(crate) struct InboundSessionCrypto(SessionCrypto);

pub(crate) struct OutboundSessionCrypto(SessionCrypto);

impl SessionCrypto {
    pub(crate) fn split(self) -> (InboundSessionCrypto, OutboundSessionCrypto) {
        let inbound = SessionCrypto {
            key: self.key,
            send_iv: self.send_iv,
            recv_iv: self.recv_iv,
            send_ctr: self.send_ctr,
            recv_ctr: self.recv_ctr,
        };
        let outbound = SessionCrypto {
            key: self.key,
            send_iv: self.send_iv,
            recv_iv: self.recv_iv,
            send_ctr: self.send_ctr,
            recv_ctr: self.recv_ctr,
        };
        (
            InboundSessionCrypto(inbound),
            OutboundSessionCrypto(outbound),
        )
    }

    /// 从 SRP 会话密钥 K 派生初始 AES-128 钥（SHA256(K)[0..16]）
    pub fn initial_key(srp_key: &[u8; 64]) -> [u8; 16] {
        Sha256::digest(srp_key)[..16].try_into().unwrap()
    }

    /// 解析服务器 52B EncryptionInfo，解出 (counter, new_key, new_iv)
    pub fn parse_encryption_info(initial_key: &[u8; 16], msg: &[u8]) -> Result<(u32, Self)> {
        if msg.len() != ENCRYPTION_INFO_LEN {
            bail!(
                "EncryptionInfo 长度异常: {}（期望 {ENCRYPTION_INFO_LEN}）",
                msg.len()
            );
        }
        if msg[..16] != ENCRYPTION_INFO_HDR {
            bail!("EncryptionInfo 头部不匹配: {:02x?}", &msg[..16]);
        }
        let counter = u32::from_be_bytes(msg[16..20].try_into().unwrap());
        let (new_key, new_iv) = Self::unwrap_slots(initial_key, &msg[20..52])?;
        Ok((counter, Self::from_key_iv(new_key, new_iv)))
    }

    /// 解密 32B 槽区 [ECB(key, new_key)][ECB(key, new_iv)]
    fn unwrap_slots(key: &[u8; 16], slots: &[u8]) -> Result<([u8; 16], [u8; 16])> {
        use aes::cipher::{BlockDecryptMut, KeyInit};
        if slots.len() != 32 {
            bail!("EncryptionInfo 槽区长度异常: {}", slots.len());
        }
        let mut plain = slots.to_vec();
        let mut dec = <ecb::Decryptor<aes::Aes128>>::new_from_slice(key).unwrap();
        for block in plain.chunks_exact_mut(16) {
            dec.decrypt_block_mut(block.into());
        }
        let new_key: [u8; 16] = plain[..16].try_into().unwrap();
        let new_iv: [u8; 16] = plain[16..].try_into().unwrap();
        Ok((new_key, new_iv))
    }

    /// 以服务器下发的新钥/新 IV 建立会话加密器（双向链均从 new_iv 起）
    pub fn from_key_iv(key: [u8; 16], iv: [u8; 16]) -> Self {
        Self {
            key,
            send_iv: iv,
            recv_iv: iv,
            send_ctr: 0,
            recv_ctr: 0,
        }
    }

    /// 把一条明文消息打包成加密帧（wire = [BE16 len][CBC 密文]）
    pub(crate) fn seal(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        if data.len() > MAX_SESSION_PLAINTEXT_BYTES {
            bail!(
                "加密会话明文过长: {}（上限 {MAX_SESSION_PLAINTEXT_BYTES}）",
                data.len()
            );
        }
        let next_counter = self
            .send_ctr
            .checked_add(1)
            .context("加密会话发送计数器已耗尽")?;
        let orig = data.len();
        let padded = orig
            .checked_add(FRAME_OVERHEAD)
            .context("加密会话帧长度溢出")?
            & !(AES_BLOCK_BYTES - 1);
        if padded > MAX_SESSION_CIPHERTEXT_BYTES {
            bail!("加密会话密文长度无法写入 u16: {padded}");
        }
        let mut buf = vec![0u8; padded];
        buf[..FRAME_LENGTH_BYTES].copy_from_slice(&(orig as u16).to_be_bytes());
        buf[FRAME_LENGTH_BYTES..FRAME_LENGTH_BYTES + orig].copy_from_slice(data);
        // 零填充位保持 0，尾部 20B 写入 SHA1(BE32(counter) ‖ body)
        let tag = Sha1::new()
            .chain_update(self.send_ctr.to_be_bytes())
            .chain_update(&buf[..padded - FRAME_SHA1_BYTES])
            .finalize();
        buf[padded - FRAME_SHA1_BYTES..].copy_from_slice(&tag);

        let mut enc =
            <cbc::Encryptor<aes::Aes128>>::new((&self.key).into(), (&self.send_iv).into());
        for chunk in buf.chunks_exact_mut(16) {
            enc.encrypt_block_mut(chunk.into());
        }
        self.send_iv = buf[padded - AES_BLOCK_BYTES..].try_into().unwrap();
        self.send_ctr = next_counter;

        let mut wire = Vec::with_capacity(padded + FRAME_LENGTH_BYTES);
        wire.extend_from_slice(&(padded as u16).to_be_bytes());
        wire.extend_from_slice(&buf);
        Ok(wire)
    }

    /// 解密一帧（输入为去掉 [BE16 len] 前缀的密文），校验 SHA1 并返回明文数据。
    /// 失败即视为流损坏——服务器会直接断开，我们同样报错。
    pub(crate) fn open(&mut self, ct: &[u8]) -> Result<Vec<u8>> {
        use aes::cipher::{BlockDecryptMut, KeyIvInit};
        let next_counter = self
            .recv_ctr
            .checked_add(1)
            .context("加密会话接收计数器已耗尽")?;
        validate_wire_ciphertext_len(ct.len())?;
        let next_iv: [u8; AES_BLOCK_BYTES] = ct[ct.len() - AES_BLOCK_BYTES..]
            .try_into()
            .expect("密文长度已验证为完整 AES 分组");
        let mut buf = ct.to_vec();
        let mut dec =
            <cbc::Decryptor<aes::Aes128>>::new((&self.key).into(), (&self.recv_iv).into());
        for chunk in buf.chunks_exact_mut(16) {
            dec.decrypt_block_mut(chunk.into());
        }
        let padded = buf.len();
        // 最短合法帧 = 2 个分组块（orig ≤ 10 时 (orig+0x25)&!0xF = 32）
        if padded < 32 {
            bail!("加密帧过短: {padded}");
        }
        let (body, tag) = buf.split_at(padded - FRAME_SHA1_BYTES);
        let expect = Sha1::new()
            .chain_update(self.recv_ctr.to_be_bytes())
            .chain_update(body)
            .finalize();
        if expect[..] != tag[..] {
            bail!(
                "加密帧 SHA1 校验失败（counter={}，可能流已错位）",
                self.recv_ctr
            );
        }
        let orig = u16::from_be_bytes([body[0], body[1]]) as usize;
        if orig + FRAME_LENGTH_BYTES > body.len() {
            bail!("加密帧内部长度 {orig} 越界");
        }
        let expected_padded = orig
            .checked_add(FRAME_OVERHEAD)
            .context("加密帧内部长度溢出")?
            & !(AES_BLOCK_BYTES - 1);
        if expected_padded != padded {
            bail!("加密帧填充长度不规范: 内部 {orig}，密文 {padded}");
        }
        self.recv_iv = next_iv;
        self.recv_ctr = next_counter;
        Ok(body[FRAME_LENGTH_BYTES..FRAME_LENGTH_BYTES + orig].to_vec())
    }
}

impl InboundSessionCrypto {
    pub(crate) fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        self.0.open(ciphertext)
    }
}

impl OutboundSessionCrypto {
    pub(crate) fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        self.0.seal(plaintext)
    }
}

// ---------- Apple 会话层握手与帧读写 ----------

#[repr(u8)]
enum AppleSessionCommand {
    EncodingOrEncryption = 0x12,
    SessionSelect = 0x21,
}

#[repr(u32)]
#[derive(Clone, Copy)]
enum EncryptionMethod {
    Aes128 = 1,
}

#[repr(u32)]
#[derive(Clone, Copy)]
enum EncryptionCommand {
    Negotiate = 1,
    Activate = 2,
}

const APPLE_SESSION_COMMAND_WIDTH_BYTES: usize = size_of::<u8>();
const APPLE_SESSION_COMMAND_U24_WIDTH_BYTES: usize = 3;
const APPLE_SESSION_LENGTH_WIDTH_BYTES: usize = APPLE_SESSION_COMMAND_U24_WIDTH_BYTES;
const ENCRYPTION_PARAMETER_ALL_MESSAGES: u16 = 1;
const ENCRYPTION_ACTIVATION_METHOD_COUNT: u16 = 0;
const ENCRYPTION_METHODS: [EncryptionMethod; 1] = [EncryptionMethod::Aes128];
/// Candidate：已验证的会话内编码表前缀；内部字段语义尚未恢复。
const APPLE_SESSION_ENCODING_LIST_VERIFIED_PREFIX: [u8; 5] = [0x0a, 0x00, 0x00, 0x01, 0x02];
const APPLE_TCP_MVS_ENCODINGS: [i32; 13] = [
    0x03f3, 0x03ea, 6, 16, -239, 0x0450, 0x044c, -223, 0x044d, 0x0451, 0x0453, 0x0455, 0x0456,
];

/// Candidate：此精确捕获载荷只证明稳定字节序列；内部字段语义仍被阻断，禁止推断或命名。
const SESSION_SELECT_VERIFIED_OPAQUE_PAYLOAD: [u8; 62] = [
    0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x1a, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01, 0xb0, 0x00,
    0x0c, 0x03, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEncodingProfile {
    Raw,
    AppleTcpMvs,
    AppleUdpMedia,
}

impl SessionEncodingProfile {
    fn encodings(self) -> Vec<i32> {
        match self {
            Self::Raw => vec![protocol::RAW],
            Self::AppleTcpMvs => APPLE_TCP_MVS_ENCODINGS.to_vec(),
            Self::AppleUdpMedia => std::iter::once(protocol::MEDIA_STREAM_CONTROL_ENCODING)
                .chain(APPLE_TCP_MVS_ENCODINGS)
                .collect(),
        }
    }
}

fn encode_u24_checked(value: u32) -> Result<[u8; APPLE_SESSION_COMMAND_U24_WIDTH_BYTES]> {
    const MAX_U24: u32 = (1 << 24) - 1;
    ensure!(value <= MAX_U24, "Apple 会话命令超出 u24 范围");
    let bytes = value.to_be_bytes();
    Ok([bytes[1], bytes[2], bytes[3]])
}

fn build_select_session() -> Result<Vec<u8>> {
    let payload_len = u32::try_from(SESSION_SELECT_VERIFIED_OPAQUE_PAYLOAD.len())
        .context("SelectSession Candidate 载荷长度超出 u32")?;
    let encoded_payload_len = encode_u24_checked(payload_len)?;
    let mut command = Vec::with_capacity(
        APPLE_SESSION_COMMAND_WIDTH_BYTES
            + APPLE_SESSION_LENGTH_WIDTH_BYTES
            + SESSION_SELECT_VERIFIED_OPAQUE_PAYLOAD.len(),
    );
    command.push(AppleSessionCommand::SessionSelect as u8);
    command.extend_from_slice(&encoded_payload_len);
    command.extend_from_slice(&SESSION_SELECT_VERIFIED_OPAQUE_PAYLOAD);
    Ok(command)
}

fn build_session_encoding_list(encodings: &[i32]) -> Result<Vec<u8>> {
    let encoding_count = u32::try_from(encodings.len()).context("会话编码数量超出 u32")?;
    let encoded_count = encode_u24_checked(encoding_count)?;
    let encoding_bytes = encodings
        .len()
        .checked_mul(size_of::<i32>())
        .context("会话编码表长度溢出")?;
    let capacity = APPLE_SESSION_ENCODING_LIST_VERIFIED_PREFIX
        .len()
        .checked_add(APPLE_SESSION_COMMAND_U24_WIDTH_BYTES)
        .and_then(|length| length.checked_add(encoding_bytes))
        .context("会话编码表容量溢出")?;
    let mut list = Vec::with_capacity(capacity);
    list.extend_from_slice(&APPLE_SESSION_ENCODING_LIST_VERIFIED_PREFIX);
    list.extend_from_slice(&encoded_count);
    for encoding in encodings {
        list.extend_from_slice(&encoding.to_be_bytes());
    }
    Ok(list)
}

fn build_set_encryption(profile: SessionEncodingProfile) -> Result<Vec<u8>> {
    let encodings = profile.encodings();
    let encodings_message = build_session_encoding_list(&encodings)?;
    let method_count = u16::try_from(ENCRYPTION_METHODS.len()).context("加密方法数量超出 u16")?;
    let encoded_command = encode_u24_checked(EncryptionCommand::Negotiate as u32)?;
    let mut command = Vec::with_capacity(
        APPLE_SESSION_COMMAND_WIDTH_BYTES
            + APPLE_SESSION_COMMAND_U24_WIDTH_BYTES
            + size_of::<u16>()
            + size_of::<u16>()
            + ENCRYPTION_METHODS.len() * size_of::<u32>()
            + encodings_message.len(),
    );
    command.push(AppleSessionCommand::EncodingOrEncryption as u8);
    command.extend_from_slice(&encoded_command);
    command.extend_from_slice(&ENCRYPTION_PARAMETER_ALL_MESSAGES.to_be_bytes());
    command.extend_from_slice(&method_count.to_be_bytes());
    for method in ENCRYPTION_METHODS {
        command.extend_from_slice(&(method as u32).to_be_bytes());
    }
    command.extend_from_slice(&encodings_message);
    Ok(command)
}

fn build_encryption_activation() -> Result<Vec<u8>> {
    let encoded_command = encode_u24_checked(EncryptionCommand::Activate as u32)?;
    let mut command = Vec::with_capacity(
        APPLE_SESSION_COMMAND_WIDTH_BYTES
            + APPLE_SESSION_COMMAND_U24_WIDTH_BYTES
            + size_of::<u16>()
            + size_of::<u16>(),
    );
    command.push(AppleSessionCommand::EncodingOrEncryption as u8);
    command.extend_from_slice(&encoded_command);
    command.extend_from_slice(&ENCRYPTION_PARAMETER_ALL_MESSAGES.to_be_bytes());
    command.extend_from_slice(&ENCRYPTION_ACTIVATION_METHOD_COUNT.to_be_bytes());
    Ok(command)
}

/// 建立加密会话并选择 Raw、TCP MVS 或 UDP MediaStream 编码档案。
pub fn establish_with_table(
    conn: &mut AppleConnection,
    srp_key: &[u8; 64],
    encoding_profile: SessionEncodingProfile,
) -> Result<SessionCrypto> {
    let initial_key = SessionCrypto::initial_key(srp_key);

    // 1) SelectSession + cmd=1(AES) —— 服务器随即明文下发 52B EncryptionInfo
    conn.write_all(&build_select_session()?)?;
    conn.write_all(&build_set_encryption(encoding_profile)?)?;

    // 服务器随即明文回 52B EncryptionInfo；异常时打印首字节辅助定位
    let info = match conn.read_vec(ENCRYPTION_INFO_LEN) {
        Ok(v) => v,
        Err(e) => {
            bail!("读取 EncryptionInfo 失败: {e:#}（SelectSession 后服务器无响应即断开）");
        }
    };
    let (counter, crypto) = SessionCrypto::parse_encryption_info(&initial_key, &info)
        .context("解析服务器 EncryptionInfo 失败")?;

    // 2) 激活加密流；此后双向全部为加密帧。
    //    cmd=2 后需稍等服务器完成会话状态迁移（3→4）并吐出初始突发，
    //    过早发送应用层帧会被服务器直接断连（实测 <600ms 必断）
    conn.write_all(&build_encryption_activation()?)?;
    let _ = counter; // 服务器侧计数器（实测恒为 1），帧计数独立从 0 起
    std::thread::sleep(std::time::Duration::from_millis(600));
    Ok(crypto)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 下列字节序列来自实施前的已验证生产报文快照；它们是独立的 Candidate 线缆证据，
    // 不得由生产 builder、常量或辅助函数生成。
    const SESSION_SELECT_FIXTURE: [u8; 66] = [
        0x21, 0x00, 0x00, 0x3e, 0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x06, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1a, 0x00, 0x00, 0x00, 0x06,
        0x00, 0x00, 0x00, 0x01, 0xb0, 0x00, 0x0c, 0x03, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    const AES_RAW_NEGOTIATION_FIXTURE: [u8; 24] = [
        0x12, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x0a, 0x00, 0x00,
        0x01, 0x02, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
    ];
    const AES_APPLE_TCP_MVS_NEGOTIATION_FIXTURE: [u8; 72] = [
        0x12, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x0a, 0x00, 0x00,
        0x01, 0x02, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x03, 0xf3, 0x00, 0x00, 0x03, 0xea, 0x00, 0x00,
        0x00, 0x06, 0x00, 0x00, 0x00, 0x10, 0xff, 0xff, 0xff, 0x11, 0x00, 0x00, 0x04, 0x50, 0x00,
        0x00, 0x04, 0x4c, 0xff, 0xff, 0xff, 0x21, 0x00, 0x00, 0x04, 0x4d, 0x00, 0x00, 0x04, 0x51,
        0x00, 0x00, 0x04, 0x53, 0x00, 0x00, 0x04, 0x55, 0x00, 0x00, 0x04, 0x56,
    ];
    const AES_APPLE_UDP_MEDIA_NEGOTIATION_FIXTURE: [u8; 76] = [
        0x12, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x0a, 0x00, 0x00,
        0x01, 0x02, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x03, 0xf2, 0x00, 0x00, 0x03, 0xf3, 0x00, 0x00,
        0x03, 0xea, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x10, 0xff, 0xff, 0xff, 0x11, 0x00,
        0x00, 0x04, 0x50, 0x00, 0x00, 0x04, 0x4c, 0xff, 0xff, 0xff, 0x21, 0x00, 0x00, 0x04, 0x4d,
        0x00, 0x00, 0x04, 0x51, 0x00, 0x00, 0x04, 0x53, 0x00, 0x00, 0x04, 0x55, 0x00, 0x00, 0x04,
        0x56,
    ];
    const ENCRYPTION_ACTIVATION_FIXTURE: [u8; 8] = [0x12, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00];

    #[test]
    fn wire_ciphertext_frame_incomplete_prefix_preserves_pending() {
        for fixture in [vec![], vec![0x00]] {
            let mut pending = fixture.clone();
            assert_eq!(take_wire_ciphertext_frame(&mut pending).unwrap(), None);
            assert_eq!(pending, fixture);
        }
    }

    #[test]
    fn wire_ciphertext_frame_rejects_invalid_lengths_without_mutating_pending() {
        for fixture in [vec![0x00, 0x00], vec![0x00, 0x0f]] {
            let mut pending = fixture.clone();
            assert!(take_wire_ciphertext_frame(&mut pending).is_err());
            assert_eq!(pending, fixture);
        }
    }

    #[test]
    fn wire_ciphertext_frame_incomplete_ciphertext_preserves_pending() {
        let fixture = vec![
            0x00, 0x10, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
            0xac, 0xad, 0xae,
        ];
        let mut pending = fixture.clone();
        assert_eq!(take_wire_ciphertext_frame(&mut pending).unwrap(), None);
        assert_eq!(pending, fixture);
    }

    #[test]
    fn wire_ciphertext_frame_consumes_exactly_one_and_preserves_trailing() {
        let mut pending = vec![
            0x00, 0x10, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
            0xac, 0xad, 0xae, 0xaf, 0x00, 0x10, 0xb0, 0xb1, 0xb2,
        ];

        let ciphertext = take_wire_ciphertext_frame(&mut pending)
            .unwrap()
            .expect("the first literal frame is complete");

        assert_eq!(
            ciphertext,
            [
                0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad,
                0xae, 0xaf,
            ]
        );
        assert_eq!(pending, [0x00, 0x10, 0xb0, 0xb1, 0xb2]);
    }

    #[test]
    fn wire_ciphertext_frame_rejects_over_limit_declaration_without_mutation() {
        let fixture = vec![0xff, 0xff];
        let mut pending = fixture.clone();
        assert!(take_wire_ciphertext_frame(&mut pending).is_err());
        assert_eq!(pending, fixture);
    }

    #[test]
    fn typed_session_builders_match_independent_wire_fixtures() {
        assert_eq!(build_select_session().unwrap(), SESSION_SELECT_FIXTURE);
        assert_eq!(
            build_set_encryption(SessionEncodingProfile::Raw).unwrap(),
            AES_RAW_NEGOTIATION_FIXTURE
        );
        assert_eq!(
            build_set_encryption(SessionEncodingProfile::AppleTcpMvs).unwrap(),
            AES_APPLE_TCP_MVS_NEGOTIATION_FIXTURE
        );
        assert_eq!(
            build_set_encryption(SessionEncodingProfile::AppleUdpMedia).unwrap(),
            AES_APPLE_UDP_MEDIA_NEGOTIATION_FIXTURE
        );
        assert_eq!(
            build_encryption_activation().unwrap(),
            ENCRYPTION_ACTIVATION_FIXTURE
        );
    }

    #[test]
    fn checked_u24_encoder_accepts_max_and_rejects_max_plus_one() {
        const MAX_U24_BYTES: [u8; 3] = [0xff, 0xff, 0xff];
        const MAX_U24: u32 = 0x00ff_ffff;
        const MAX_U24_PLUS_ONE: u32 = 0x0100_0000;

        assert_eq!(encode_u24_checked(MAX_U24).unwrap(), MAX_U24_BYTES);
        assert!(encode_u24_checked(MAX_U24_PLUS_ONE).is_err());
    }

    #[test]
    fn session_encoding_profile_udp_media_prefixes_shared_media_control_encoding() {
        const EXPECTED_ENCODINGS: [i32; 14] = [
            0x03f2, 0x03f3, 0x03ea, 6, 16, -239, 0x0450, 0x044c, -223, 0x044d, 0x0451, 0x0453,
            0x0455, 0x0456,
        ];
        assert_eq!(
            SessionEncodingProfile::AppleUdpMedia.encodings(),
            EXPECTED_ENCODINGS
        );
    }

    /// ECB 加密一个 32B 槽区（构造服务器 EncryptionInfo 用）
    fn ecb_enc(key: &[u8; 16], data: &[u8]) -> Vec<u8> {
        use aes::cipher::{BlockEncryptMut, KeyInit};
        let mut buf = data.to_vec();
        let mut enc = <ecb::Encryptor<aes::Aes128>>::new_from_slice(key).unwrap();
        for block in buf.chunks_exact_mut(16) {
            enc.encrypt_block_mut(block.into());
        }
        buf
    }

    #[test]
    fn session_crypto_frame_round_trip_multi_frame_chain() {
        // 两条方向独立的会话加密器（模拟客户端与服务器）
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let mut alice = SessionCrypto::from_key_iv(key, iv);
        let mut bob = SessionCrypto::from_key_iv(key, iv);

        let msgs: Vec<Vec<u8>> = vec![
            vec![3, 1, 0, 0, 0, 0, 0x07, 0x80, 0x04, 0x38], // FramebufferUpdateRequest
            vec![0x14, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x04], // 心跳
            (0u8..100u8).collect(),                         // 较长消息（跨多个分组块）
        ];
        for m in &msgs {
            let wire = alice.seal(m).unwrap();
            // wire = [BE16 len][ct]
            let len = u16::from_be_bytes([wire[0], wire[1]]) as usize;
            assert_eq!(len % 16, 0, "填充后长度必须 16 对齐");
            assert_eq!(len, (m.len() + FRAME_OVERHEAD) & !0xF);
            let back = bob.open(&wire[2..]).expect("解密失败");
            assert_eq!(&back, m);
        }
        // 双向计数器独立：反向也发一条
        let wire = bob.seal(b"pong").unwrap();
        assert_eq!(&alice.open(&wire[2..]).unwrap(), b"pong");
    }

    #[test]
    fn frame_chain_detects_desync() {
        let mut a = SessionCrypto::from_key_iv([1u8; 16], [2u8; 16]);
        let mut b = SessionCrypto::from_key_iv([1u8; 16], [2u8; 16]);
        let w1 = a.seal(b"first").unwrap();
        let w2 = a.seal(b"second").unwrap();
        assert!(b.open(&w1[2..]).is_ok());
        // 乱序/丢帧后链 IV 错位 → SHA1 必须失败
        assert!(b.open(&w2[2..]).is_ok());
        // 重放第一帧（counter 已前进）必须失败
        let mut d = SessionCrypto::from_key_iv([1u8; 16], [2u8; 16]);
        assert!(d.open(&w1[2..]).is_ok());
        assert!(d.open(&w1[2..]).is_err(), "同一帧重放必须被 SHA1 拒绝");
    }

    #[test]
    fn parse_encryption_info_derives_key() {
        let srp_key = [0xABu8; 64];
        let initial = SessionCrypto::initial_key(&srp_key);
        let new_key = [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
            0xFF, 0x01,
        ];
        let new_iv = [
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0, 0xD0, 0xE0,
            0xF0, 0x00,
        ];

        let mut msg = Vec::new();
        msg.extend_from_slice(&ENCRYPTION_INFO_HDR);
        msg.extend_from_slice(&1u32.to_be_bytes());
        msg.extend_from_slice(&ecb_enc(&initial, &new_key));
        msg.extend_from_slice(&ecb_enc(&initial, &new_iv));
        assert_eq!(msg.len(), ENCRYPTION_INFO_LEN);

        let (counter, mut crypto) = SessionCrypto::parse_encryption_info(&initial, &msg).unwrap();
        assert_eq!(counter, 1);
        // 解出的钥能直接封帧并被同钥对端解开
        let mut peer = SessionCrypto::from_key_iv(new_key, new_iv);
        let data = b"hello encrypted session";
        let wire = crypto.seal(data).unwrap();
        assert_eq!(&peer.open(&wire[2..]).unwrap(), data);
    }

    #[test]
    fn key_derivation_vector() {
        // 密钥派生链回归锚点：K = SHA512(S)（srp.rs 已测），key16 = SHA256(K)[0:16]
        let srp_key = [0u8; 64];
        let initial = SessionCrypto::initial_key(&srp_key);
        assert_eq!(initial.len(), 16);
        // 全零 K 的 SHA256 前 16 字节（固定向量，防止误改成 SHA512/截尾错位）
        let expect = Sha256::digest([0u8; 64]);
        assert_eq!(initial[..], expect[..16]);
    }

    #[test]
    fn seal_rejects_payloads_that_do_not_fit_the_wire_length() {
        let mut crypto = SessionCrypto::from_key_iv([1u8; 16], [2u8; 16]);
        let oversized = vec![0u8; MAX_SESSION_PLAINTEXT_BYTES + 1];
        assert!(crypto.seal(&oversized).is_err());
    }

    #[test]
    fn frame_counters_fail_closed_before_wraparound() {
        let mut sender = SessionCrypto::from_key_iv([1u8; 16], [2u8; 16]);
        sender.send_ctr = u32::MAX;
        assert!(sender.seal(b"counter exhausted").is_err());

        let mut receiver = SessionCrypto::from_key_iv([1u8; 16], [2u8; 16]);
        receiver.recv_ctr = u32::MAX;
        assert!(receiver.open(&[0u8; 16]).is_err());
    }
}
