//! ARD 认证（安全类型 33）：RSA-SRP 混合（Apple 客户端的原生默认路径）。
//!
//! 字节布局（2026-08-18 通过 MITM 捕获 Apple Screen Sharing 客户端真实会话 +
//! screensharingd 反汇编双重确认，详见 docs/ARD_PROTOCOL.md §5.0）：
//!
//! ```text
//! C→S 选型   [0x21][u32 10][01 00 "RSA1"][u16 0][u16 0]          ← v0：请求公钥
//! S→C 公钥   [u32 klen+7][u16 1][u16 0][u16 klen][SPKI DER][opaque] ← RSA-2048
//! C→S v2帧   [u32 L][01 00 "RSA1"][u16 2][u16 ctlen][RSA-PKCS1v1.5(step1 TLV)]
//! S→C 挑战   [u32 L][u32 2][u16 M][u32 M-4][TLV 项]              ← 与类型 36 完全同构
//! C→S step2  [u32 L][01 00 "RSA1"][u16 2][u16 M2][u32 M2-4][项]  ← 明文（不加密）
//! S→C 响应   [u32 98][u32 2][u16 92][u32 88][0x40][64B H_AMK][23B]
//! ```
//!
//! - RSA 填充为 **PKCS#1 v1.5**（反汇编 `SecKeyDecrypt(key, 1, …)`，
//!   kSecPaddingPKCS1），加密的是类型 36 的 step1 TLV（用户名仅在此密文内）；
//! - 其后 SRP 数学、TLV 项、H_AMK 校验与类型 36 完全一致（复用 `srp.rs`）；
//! - 服务器要求**同时广播类型 36** 才走此路径（反汇编在广播串里搜 `'$'`=0x24），
//!   否则报 "viewer requested RSA SRP but SRP was not advertised"。

use anyhow::{bail, ensure, Context, Result};
use rsa::pkcs1v15::Pkcs1v15Encrypt;
use rsa::pkcs8::DecodePublicKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPublicKey;

use super::client::RfbConn;
use super::protocol::{self, security};
use super::srp::{self, SrpChallenge};

const RSA1_MAGIC: &[u8; 4] = b"RSA1";
const RSA1_HEADER_PREFIX: [u8; 2] = [0x01, 0x00];
const RSA_PUBLIC_KEY_RESPONSE_VERSION: u16 = 1;
const RSA_PUBLIC_KEY_VERSION_BYTES: usize = size_of::<u16>();
const RSA_PUBLIC_KEY_RESERVED_BYTES: usize = size_of::<u16>();
const RSA_PUBLIC_KEY_DER_LENGTH_BYTES: usize = size_of::<u16>();
const RSA_PUBLIC_KEY_HEADER_BYTES: usize =
    RSA_PUBLIC_KEY_VERSION_BYTES + RSA_PUBLIC_KEY_RESERVED_BYTES + RSA_PUBLIC_KEY_DER_LENGTH_BYTES;
const RSA_SRP_NESTED_FRAME_TYPE: u32 = 2;
const RSA_SRP_NESTED_FRAME_MIN_BYTES: usize = 16;
const RSA_SRP_NESTED_FRAME_MAX_BYTES: usize = srp::APPLE_SRP_CHALLENGE_MAX_BYTES;
const RSA_PUBLIC_KEY_FRAME_MIN_BYTES: usize = 64;
const RSA_PUBLIC_KEY_FRAME_MAX_BYTES: usize = 1024;
const RSA_PUBLIC_KEY_MAX_BITS: usize = 8192;

#[derive(Clone, Copy)]
enum RsaSrpFrameVersion {
    PublicKeyRequest,
    EncryptedSrp,
}

impl RsaSrpFrameVersion {
    fn wire(self) -> [u8; 2] {
        match self {
            Self::PublicKeyRequest => 0u16.to_be_bytes(),
            Self::EncryptedSrp => 2u16.to_be_bytes(),
        }
    }
}

/// RSA1 帧头：[01 00]["RSA1"][u16 版本]
fn rsa1_header(version: RsaSrpFrameVersion) -> Vec<u8> {
    let mut v = Vec::with_capacity(RSA1_HEADER_PREFIX.len() + RSA1_MAGIC.len() + size_of::<u16>());
    v.extend_from_slice(&RSA1_HEADER_PREFIX);
    v.extend_from_slice(RSA1_MAGIC);
    v.extend_from_slice(&version.wire());
    v
}

fn build_public_key_request() -> Result<Vec<u8>> {
    let mut payload = rsa1_header(RsaSrpFrameVersion::PublicKeyRequest);
    payload
        .try_reserve(size_of::<u16>())
        .context("RSA-SRP 公钥请求帧分配失败")?;
    payload.extend_from_slice(&0u16.to_be_bytes());
    let outer_length = srp::checked_u32_frame_length(payload.len(), "RSA-SRP 公钥请求外层帧")?;
    let mut message = Vec::new();
    message
        .try_reserve(
            size_of::<u8>()
                .checked_add(size_of::<u32>())
                .and_then(|length| length.checked_add(payload.len()))
                .context("RSA-SRP 公钥请求帧长度计算溢出")?,
        )
        .context("RSA-SRP 公钥请求帧分配失败")?;
    message.push(security::APPLE_RSA_SRP);
    message.extend_from_slice(&outer_length);
    message.extend_from_slice(&payload);
    Ok(message)
}

fn build_rsa_srp_frame(data: &[u8]) -> Result<Vec<u8>> {
    let data_length = srp::checked_u16_frame_length(data.len(), "RSA-SRP 子帧")?;
    let mut payload = rsa1_header(RsaSrpFrameVersion::EncryptedSrp);
    let payload_tail = size_of::<u16>()
        .checked_add(data.len())
        .context("RSA-SRP 子帧长度计算溢出")?;
    payload
        .try_reserve(payload_tail)
        .context("RSA-SRP 子帧分配失败")?;
    payload.extend_from_slice(&data_length);
    payload.extend_from_slice(data);

    let outer_length = srp::checked_u32_frame_length(payload.len(), "RSA-SRP 外层帧")?;
    let mut frame = Vec::new();
    frame
        .try_reserve(
            size_of::<u32>()
                .checked_add(payload.len())
                .context("RSA-SRP 外层帧长度计算溢出")?,
        )
        .context("RSA-SRP 外层帧分配失败")?;
    frame.extend_from_slice(&outer_length);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn take_u16_field(input: &mut &[u8], field: &str) -> Result<u16> {
    ensure!(input.len() >= size_of::<u16>(), "{field}字段被截断");
    let (bytes, remaining) = input.split_at(size_of::<u16>());
    *input = remaining;
    Ok(u16::from_be_bytes(
        bytes.try_into().context("u16 字段宽度内部错误")?,
    ))
}

fn take_u32_field(input: &mut &[u8], field: &str) -> Result<u32> {
    ensure!(input.len() >= size_of::<u32>(), "{field}字段被截断");
    let (bytes, remaining) = input.split_at(size_of::<u32>());
    *input = remaining;
    Ok(u32::from_be_bytes(
        bytes.try_into().context("u32 字段宽度内部错误")?,
    ))
}

#[derive(Debug)]
struct RsaPublicKeyFrame<'a> {
    der: &'a [u8],
    /// 旧解析器允许的有界帧尾；字段语义未证实，保持 opaque。
    opaque_tail: &'a [u8],
}

fn parse_rsa_public_key_frame(frame: &[u8]) -> Result<RsaPublicKeyFrame<'_>> {
    ensure!(
        (RSA_PUBLIC_KEY_FRAME_MIN_BYTES..=RSA_PUBLIC_KEY_FRAME_MAX_BYTES).contains(&frame.len()),
        "RSA 公钥帧长度异常: {}",
        frame.len()
    );
    ensure!(
        frame.len() >= RSA_PUBLIC_KEY_HEADER_BYTES,
        "RSA 公钥帧头被截断"
    );
    let mut fields = frame;
    let version = take_u16_field(&mut fields, "RSA 公钥版本")?;
    let _reserved = take_u16_field(&mut fields, "RSA 公钥保留")?;
    let der_length = usize::from(take_u16_field(&mut fields, "RSA 公钥 DER 长度")?);
    ensure!(
        version == RSA_PUBLIC_KEY_RESPONSE_VERSION,
        "RSA 公钥帧版本异常: {version}"
    );
    ensure!(
        fields.len() >= der_length,
        "RSA 公钥长度不完整: 需要 {der_length}，实得 {}",
        fields.len()
    );
    let (der, opaque_tail) = fields.split_at(der_length);
    Ok(RsaPublicKeyFrame { der, opaque_tail })
}

fn parse_rsa_srp_nested_frame(frame: &[u8]) -> Result<&[u8]> {
    ensure!(
        (RSA_SRP_NESTED_FRAME_MIN_BYTES..=RSA_SRP_NESTED_FRAME_MAX_BYTES).contains(&frame.len()),
        "RSA-SRP 帧长度异常: {}",
        frame.len()
    );
    let mut fields = frame;
    let frame_type = take_u32_field(&mut fields, "RSA-SRP 帧类型")?;
    ensure!(
        frame_type == RSA_SRP_NESTED_FRAME_TYPE,
        "RSA-SRP 帧类型异常: {frame_type}"
    );
    let child_length = usize::from(take_u16_field(&mut fields, "RSA-SRP 子帧长度")?);
    ensure!(
        fields.len() == child_length,
        "RSA-SRP 子帧长度不精确: 声明 {child_length}，实得 {}",
        fields.len()
    );
    let tlv_length = usize::try_from(take_u32_field(&mut fields, "RSA-SRP TLV 长度")?)
        .context("RSA-SRP TLV 长度无法表示为 usize")?;
    ensure!(
        fields.len() == tlv_length,
        "RSA-SRP TLV 头校验失败: 声明 {tlv_length}，实得 {}",
        fields.len()
    );
    Ok(fields)
}

/// 执行类型 33 认证。成功后连接处于 ClientInit 前状态（SecurityResult 已消费）。
pub fn authenticate(conn: &mut RfbConn, username: &str, password: &str) -> Result<()> {
    // 先完整验证并构造用户名 TLV，确保长度错误绝不触发首个 socket 写入。
    let plain = srp::initial_auth_payload(username)?;

    // 1. 选型 + v0 公钥请求必须是一条消息：[0x21][u32 10][01 00 "RSA1"][u16 0][u16 0]
    //    （长度恰为 10 才能通过服务器对首帧的长度闸；拆开发会被外层分发器当新类型字节）
    conn.write_all(&build_public_key_request()?)?;

    // 2. 读公钥：[u32 klen+7][u16 1][u16 0][u16 klen][SPKI DER][opaque]
    let total = usize::try_from(conn.read_u32()?).context("RSA 公钥帧长度无法表示为 usize")?;
    if !(RSA_PUBLIC_KEY_FRAME_MIN_BYTES..=RSA_PUBLIC_KEY_FRAME_MAX_BYTES).contains(&total) {
        bail!("RSA 公钥帧长度异常: {total}");
    }
    let frame = conn.read_vec(total)?;
    let public_key_frame = parse_rsa_public_key_frame(&frame)?;
    let _opaque_tail = public_key_frame.opaque_tail;
    let pub_key = RsaPublicKey::from_public_key_der(public_key_frame.der)
        .context("解析服务器 RSA 公钥失败")?;
    if pub_key.n().bits() > RSA_PUBLIC_KEY_MAX_BITS {
        bail!("RSA 密钥异常地大: {} 位", pub_key.n().bits());
    }

    // 3. v2 帧：RSA-PKCS1v1.5 加密的 step1 TLV（与类型 36 相同的项布局）
    let mut rnd = rsa::rand_core::OsRng;
    let ct = pub_key
        .encrypt(&mut rnd, Pkcs1v15Encrypt, &plain)
        .map_err(|e| anyhow::anyhow!("RSA 加密失败: {e}"))?;
    conn.write_all(&build_rsa_srp_frame(&ct)?)?;

    // 4. SRP 挑战：[u32 L][u32 2][u16 M][u32 M-4][项…]
    let chal = read_srp_frame(conn)?;

    // 5. SRP 数学（与 36 完全一致）
    let mut rnd64 = [0u8; 64];
    getrandom::getrandom(&mut rnd64).map_err(|e| anyhow::anyhow!("系统随机数失败: {e}"))?;
    let (pub_a, m1, key) = srp::srp_compute(&chal, password, rnd64)?;

    // 6. step2：RSA1 v2 帧包裹的明文 TLV（项布局同 36）
    let mut nonce = [0u8; srp::APPLE_SRP_NONCE_BYTES];
    getrandom::getrandom(&mut nonce).map_err(|e| anyhow::anyhow!("系统随机数失败: {e}"))?;
    let a_bytes = srp::encode_srp_value_padded(&pub_a)?;
    let mut builder = srp::SrpTlvBuilder::new();
    builder.push_sized_u16(&a_bytes)?;
    builder.push_sized_u8(&m1)?;
    builder.push_sized_u16(chal.options.as_bytes())?;
    builder.push_sized_u8(&nonce)?;
    conn.write_all(&build_rsa_srp_frame(&builder.finish()?)?)?;

    // 7. 响应：[u32 98][u32 2][u16 92][u32 88][0x40][H_AMK][23B]
    let resp = read_srp_frame_raw(conn)?;
    let response = srp::parse_srp_response_items(&resp).context("RSA-SRP 响应格式异常")?;
    let _opaque_tail = response.opaque_tail;
    if response.proof != &srp::expected_hamk(&pub_a, &m1, &key)? {
        bail!("RSA-SRP 服务器证明校验失败（疑似中间人攻击）");
    }

    // 8. RFB SecurityResult
    if conn.read_u32()? != protocol::RFB_SECURITY_RESULT_OK {
        bail!("RSA-SRP 认证被服务器拒绝（SecurityResult != 0）");
    }
    Ok(())
}

/// 读服务器 SRP 帧：[u32 L][u32 2][u16 M][u32 M-4][项 M-4]，返回项缓冲
fn read_srp_frame_raw(conn: &mut RfbConn) -> Result<Vec<u8>> {
    let total = usize::try_from(conn.read_u32()?).context("RSA-SRP 帧长度无法表示为 usize")?;
    if !(RSA_SRP_NESTED_FRAME_MIN_BYTES..=RSA_SRP_NESTED_FRAME_MAX_BYTES).contains(&total) {
        bail!("RSA-SRP 帧长度异常: {total}");
    }
    let frame = conn.read_vec(total)?;
    Ok(parse_rsa_srp_nested_frame(&frame)?.to_vec())
}

fn read_srp_frame(conn: &mut RfbConn) -> Result<SrpChallenge> {
    let items = read_srp_frame_raw(conn)?;
    srp::parse_challenge(&items).context("RSA-SRP 挑战解析失败")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vnc::client;
    use num_bigint::BigUint;
    use sha2::{Digest, Sha512};
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::thread;
    use std::time::Duration;

    fn fixture_sized_u16(data: &[u8]) -> Vec<u8> {
        let mut item = u16::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        item.extend_from_slice(data);
        item
    }

    fn fixture_sized_u8(data: &[u8]) -> Vec<u8> {
        let mut item = vec![u8::try_from(data.len()).unwrap()];
        item.extend_from_slice(data);
        item
    }

    #[test]
    fn rsa_srp_public_key_frame_accepts_exact_independent_boundaries() {
        let mut minimum = vec![0u8; 64];
        minimum[..2].copy_from_slice(&1u16.to_be_bytes());
        minimum[4..6].copy_from_slice(&58u16.to_be_bytes());
        let parsed = parse_rsa_public_key_frame(&minimum).unwrap();
        assert_eq!(parsed.der.len(), 58);
        assert!(parsed.opaque_tail.is_empty());

        let mut maximum = vec![0u8; 1024];
        maximum[..2].copy_from_slice(&1u16.to_be_bytes());
        maximum[4..6].copy_from_slice(&1018u16.to_be_bytes());
        let parsed = parse_rsa_public_key_frame(&maximum).unwrap();
        assert_eq!(parsed.der.len(), 1018);
        assert!(parsed.opaque_tail.is_empty());
    }

    #[test]
    fn rsa_srp_public_key_frame_rejects_bounds_and_truncation() {
        assert!(parse_rsa_public_key_frame(&[0u8; 63]).is_err());
        assert!(parse_rsa_public_key_frame(&vec![0u8; 1025]).is_err());

        let mut truncated = vec![0u8; 64];
        truncated[..2].copy_from_slice(&1u16.to_be_bytes());
        truncated[4..6].copy_from_slice(&59u16.to_be_bytes());
        assert!(parse_rsa_public_key_frame(&truncated).is_err());
    }

    #[test]
    fn rsa_srp_public_key_frame_preserves_bounded_trailing_opaque_bytes() {
        let mut trailing = vec![0u8; 64];
        trailing[..2].copy_from_slice(&1u16.to_be_bytes());
        trailing[4..6].copy_from_slice(&57u16.to_be_bytes());
        trailing[63] = 0xa5;

        let parsed = parse_rsa_public_key_frame(&trailing).unwrap();
        assert_eq!(parsed.der.len(), 57);
        assert_eq!(parsed.opaque_tail, &[0xa5]);
    }

    #[test]
    fn rsa_srp_nested_frame_accepts_exact_independent_boundaries() {
        let mut minimum = vec![0u8; 16];
        minimum[..4].copy_from_slice(&2u32.to_be_bytes());
        minimum[4..6].copy_from_slice(&10u16.to_be_bytes());
        minimum[6..10].copy_from_slice(&6u32.to_be_bytes());
        assert_eq!(parse_rsa_srp_nested_frame(&minimum).unwrap().len(), 6);

        let mut maximum = vec![0u8; 8192];
        maximum[..4].copy_from_slice(&2u32.to_be_bytes());
        maximum[4..6].copy_from_slice(&8186u16.to_be_bytes());
        maximum[6..10].copy_from_slice(&8182u32.to_be_bytes());
        assert_eq!(parse_rsa_srp_nested_frame(&maximum).unwrap().len(), 8182);
    }

    #[test]
    fn rsa_srp_nested_frame_rejects_bounds_and_inexact_child_or_tlv_lengths() {
        assert!(parse_rsa_srp_nested_frame(&[0u8; 15]).is_err());
        assert!(parse_rsa_srp_nested_frame(&vec![0u8; 8193]).is_err());

        let mut truncated_child = vec![0u8; 16];
        truncated_child[..4].copy_from_slice(&2u32.to_be_bytes());
        truncated_child[4..6].copy_from_slice(&11u16.to_be_bytes());
        assert!(parse_rsa_srp_nested_frame(&truncated_child).is_err());

        let mut trailing_child = vec![0u8; 16];
        trailing_child[..4].copy_from_slice(&2u32.to_be_bytes());
        trailing_child[4..6].copy_from_slice(&9u16.to_be_bytes());
        assert!(parse_rsa_srp_nested_frame(&trailing_child).is_err());

        let mut wrong_tlv = vec![0u8; 16];
        wrong_tlv[..4].copy_from_slice(&2u32.to_be_bytes());
        wrong_tlv[4..6].copy_from_slice(&10u16.to_be_bytes());
        wrong_tlv[6..10].copy_from_slice(&5u32.to_be_bytes());
        assert!(parse_rsa_srp_nested_frame(&wrong_tlv).is_err());
    }

    #[test]
    fn oversized_srp_username_fails_before_rsa_srp_socket_write() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).unwrap();
            assert!(received.is_empty(), "超长用户名认证前不应写入任何字节");
        });

        let stream = std::net::TcpStream::connect(addr).unwrap();
        let shutdown = stream.try_clone().unwrap();
        let mut conn = client::RfbConn::new(stream);
        conn.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let username = "x".repeat(usize::from(u16::MAX) + 1);
        let error = authenticate(&mut conn, &username, "password").unwrap_err();
        assert!(error.to_string().contains("u16"), "{error:#}");
        shutdown.shutdown(Shutdown::Write).unwrap();
        drop(conn);
        server.join().unwrap();
    }

    /// 端到端：mock 一个 type-33 服务器（下发 RSA 公钥、解密 step1、跑 SRP 服务端、
    /// 验证 M1、回 H_AMK），验证客户端全流程。RSA 用 1024 位以加速测试。
    #[test]
    fn rsa_srp_auth_round_trip() {
        use rsa::pkcs8::EncodePublicKey;
        use rsa::{RsaPrivateKey, RsaPublicKey};

        let salt = [0x22u8; 32];
        let iters = 100u32;
        let user = "test-user";
        let pass = "test-password";
        let options =
            "mda=SHA-512,replay_detection,conf+int=ChaCha20-Poly1305,kdf=SALTED-SHA512-PBKDF2";
        let n = srp::apple_srp_prime();
        let g: BigUint = 5u32.into();
        let b512 = |z: &BigUint| {
            let b = z.to_bytes_be();
            let mut v = vec![0u8; 512 - b.len()];
            v.extend_from_slice(&b);
            v
        };
        let sha = |parts: &[&[u8]]| -> Vec<u8> {
            let mut h = Sha512::new();
            for p in parts {
                h.update(p);
            }
            h.finalize().to_vec()
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();

            // 首条消息 = [0x21][u32 10][v0 负载]
            let mut t = [0u8; 1];
            s.read_exact(&mut t).unwrap();
            assert_eq!(t[0], 0x21);
            let mut h = [0u8; 4];
            s.read_exact(&mut h).unwrap();
            let mut f = vec![0u8; usize::try_from(u32::from_be_bytes(h)).unwrap()];
            s.read_exact(&mut f).unwrap();
            assert_eq!(&f[..6], &[0x01, 0x00, b'R', b'S', b'A', b'1']);
            assert_eq!(&f[6..8], &0u16.to_be_bytes());

            // 下发公钥
            let priv_key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
            let pub_key = RsaPublicKey::from(&priv_key);
            let der = pub_key.to_public_key_der().unwrap();
            let der = der.as_bytes();
            let public_key_frame_length = u32::try_from(der.len().checked_add(6).unwrap()).unwrap();
            let mut msg = public_key_frame_length.to_be_bytes().to_vec();
            msg.extend_from_slice(&1u16.to_be_bytes());
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&u16::try_from(der.len()).unwrap().to_be_bytes());
            msg.extend_from_slice(der);
            s.write_all(&msg).unwrap();

            // v2 帧：RSA-PKCS1v1.5(step1 TLV)
            let mut h = [0u8; 4];
            s.read_exact(&mut h).unwrap();
            let mut f = vec![0u8; usize::try_from(u32::from_be_bytes(h)).unwrap()];
            s.read_exact(&mut f).unwrap();
            assert_eq!(&f[..6], &[0x01, 0x00, b'R', b'S', b'A', b'1']);
            assert_eq!(&f[6..8], &2u16.to_be_bytes());
            let ctlen = usize::from(u16::from_be_bytes([f[8], f[9]]));
            let plain = priv_key
                .decrypt(Pkcs1v15Encrypt, &f[10..10 + ctlen])
                .unwrap();
            // plain = [u32 10][u16 0][u16 3 "test-user"][u16 0][u8 0]
            let mut expected_plain = vec![0u8, 0];
            expected_plain.extend_from_slice(&u16::try_from(user.len()).unwrap().to_be_bytes());
            expected_plain.extend_from_slice(user.as_bytes());
            expected_plain.extend_from_slice(&[0, 0, 0]);
            assert_eq!(&plain[4..], expected_plain.as_slice());

            // 挑战（与 36 同项）
            let mut dk = [0u8; 128];
            pbkdf2::pbkdf2_hmac::<Sha512>(pass.as_bytes(), &salt, iters, &mut dk);
            let x = BigUint::from_bytes_be(&sha(&[&salt, &sha(&[b":", &dk])]));
            let v = g.modpow(&x, &n);
            let k = BigUint::from_bytes_be(&sha(&[&b512(&n), &b512(&g)]));
            let b_exp = BigUint::from(1919810114514u64);
            let big_b = (&k * &v + g.modpow(&b_exp, &n)) % &n;
            let mut items = vec![0u8];
            items.extend_from_slice(&fixture_sized_u16(&b512(&n)));
            items.extend_from_slice(&fixture_sized_u16(&[5u8]));
            items.extend_from_slice(&fixture_sized_u8(&salt));
            items.extend_from_slice(&fixture_sized_u16(&b512(&big_b)));
            items.extend_from_slice(&(iters as u64).to_be_bytes());
            items.extend_from_slice(&fixture_sized_u16(options.as_bytes()));
            let items_length = u32::try_from(items.len()).unwrap();
            let mut inner = items_length.to_be_bytes().to_vec();
            inner.extend_from_slice(&items);
            let frame_length = u32::try_from(inner.len().checked_add(6).unwrap()).unwrap();
            let mut frame = frame_length.to_be_bytes().to_vec();
            frame.extend_from_slice(&2u32.to_be_bytes());
            frame.extend_from_slice(&u16::try_from(inner.len()).unwrap().to_be_bytes());
            frame.extend_from_slice(&inner);
            s.write_all(&frame).unwrap();

            // step2（明文 RSA1 v2 帧）
            let mut h = [0u8; 4];
            s.read_exact(&mut h).unwrap();
            let mut f = vec![0u8; usize::try_from(u32::from_be_bytes(h)).unwrap()];
            s.read_exact(&mut f).unwrap();
            let m = usize::from(u16::from_be_bytes([f[8], f[9]]));
            let tlv = &f[10..10 + m];
            let items_len =
                usize::try_from(u32::from_be_bytes(tlv[0..4].try_into().unwrap())).unwrap();
            let items = &tlv[4..4 + items_len];
            let alen = usize::from(u16::from_be_bytes([items[0], items[1]]));
            let big_a = BigUint::from_bytes_be(&items[2..2 + alen]);
            let m1 = &items[2 + alen + 1..2 + alen + 65];

            // 服务端验证
            let u = BigUint::from_bytes_be(&sha(&[&b512(&big_a), &b512(&big_b)]));
            let s_srv = (&big_a * &v.modpow(&u, &n) % &n).modpow(&b_exp, &n);
            let key = sha(&[&b512(&s_srv)]);
            let hn = sha(&[&b512(&n)]);
            let hg = sha(&[&b512(&g)]);
            let xor: Vec<u8> = hn.iter().zip(&hg).map(|(i, j)| i ^ j).collect();
            let m1_srv = sha(&[
                &xor,
                &sha(&[b""]),
                &salt,
                &b512(&big_a),
                &b512(&big_b),
                &key,
            ]);
            assert_eq!(m1, &m1_srv[..], "M1 不匹配");

            // 响应
            let mut resp_items = vec![0x40u8];
            resp_items.extend_from_slice(&sha(&[&b512(&big_a), &m1_srv, &key]));
            resp_items.extend_from_slice(&[0u8; 23]);
            // 响应帧 = [u32 total][u32 2][u16 M][u32 M-4][项]
            let mut inner = 2u32.to_be_bytes().to_vec();
            inner.extend_from_slice(
                &u16::try_from(resp_items.len().checked_add(4).unwrap())
                    .unwrap()
                    .to_be_bytes(),
            );
            inner.extend_from_slice(&u32::try_from(resp_items.len()).unwrap().to_be_bytes());
            inner.extend_from_slice(&resp_items);
            let mut frame = u32::try_from(inner.len()).unwrap().to_be_bytes().to_vec();
            frame.extend_from_slice(&inner);
            s.write_all(&frame).unwrap();
            s.write_all(&0u32.to_be_bytes()).unwrap();
        });

        let stream = std::net::TcpStream::connect(addr).unwrap();
        let mut conn = client::RfbConn::new(stream);
        if let Err(e) = authenticate(&mut conn, user, pass) {
            panic!("客户端错误: {e:#}");
        }
        server.join().unwrap();
    }
}
