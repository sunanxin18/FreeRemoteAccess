//! ARD 认证（安全类型 36）：SRP-6a over RFC 5054 4096 组（SHA-512 体系）。
//!
//! 字节布局与数学配方（2026-08-18 对 macOS 26.6.1 真机打通 + H_AMK 闭环验证，
//! 详见 docs/ARD_PROTOCOL.md §5.0 与 ard_re/NOTES.md）：
//!
//! ```text
//! 消息信封（客户端）: [u8 36][u32 L][负载]，负载 = [u32 L-4][TLV 项]
//! 消息信封（服务器）: [u32 L][u32 L-4][TLV 项]
//! TLV 项: %s=[u16 BE len][数据]  %o=[u8 len][数据]  %m=[u16][数据]  %q=8B BE  %c=1B
//!
//! C→S step1 : s("") + s(用户名) + s("") + o("")        ← 用户名在第二个字段
//! S→C 挑战  : c(cmd) + m(素数p) + m(g=5) + o(盐32B) + m(B) + q(PBKDF2迭代数) + s(选项串)
//! C→S step2 : m(A) + o(M1) + s(选项串原样回传) + o(随机16B nonce)
//! S→C 响应  : [0x40][64B H_AMK][23B 尾部]；随后 RFB u32 SecurityResult（0 = 成功）
//! ```
//!
//! 密码学（corecrypto `ccsrp_ctx_init` 三参默认选项 = SRP6a + KDF=HASH、无跳零填充）：
//! ```text
//! dk  = PBKDF2-HMAC-SHA512(密码, 盐, 迭代数, dkLen=128)
//! x   = SHA512(盐 ‖ SHA512(":" ‖ dk))                 ← 用户名不参与（noUsernameInX）
//! k   = SHA512(N₅₁₂ ‖ g₅₁₂)、u = SHA512(A₅₁₂ ‖ B₅₁₂)  ← 全部 512 字节左填充参与哈希
//! S   = (B − k·g^x)^(a + u·x) mod N、K = SHA512(S₅₁₂)
//! M1  = SHA512(H(N)⊕H(g) ‖ SHA512("") ‖ 盐 ‖ A₅₁₂ ‖ B₅₁₂ ‖ K)   ← 用户名是空串
//! H_AMK = SHA512(A₅₁₂ ‖ M1 ‖ K)                        ← 服务器证明，必须校验
//! ```
//!
//! 注意：OD 查询失败时服务器会返回**诱饵挑战**（假盐 + 假 verifier，防用户名枚举），
//! 表现为 step2 后收到 `u32 1`。凭据是 Mac 真实本地账号。

use anyhow::{bail, ensure, Context, Result};
use num_bigint::BigUint;
use sha2::{Digest, Sha512};

use super::client::RfbConn;
use super::protocol;

pub(crate) const APPLE_SRP_PADDED_BYTES: usize = 512;
pub(crate) const APPLE_SRP_GENERATOR: u8 = 5;
pub(crate) const APPLE_SRP_NONCE_BYTES: usize = 16;
pub(crate) const APPLE_SRP_PROOF_BYTES: usize = 64;
pub(crate) const APPLE_SRP_RESPONSE_TAG: u8 = 0x40;
/// Apple 已捕获但尚未建立字段语义的响应尾部；只校验其精确宽度。
pub(crate) const APPLE_SRP_RESPONSE_OPAQUE_TAIL_BYTES: usize = 23;
pub(crate) const APPLE_SRP_FRAME_LENGTH_BYTES: usize = size_of::<u32>();
pub(crate) const APPLE_SRP_TLV_LENGTH_BYTES: usize = size_of::<u32>();
pub(crate) const APPLE_SRP_NESTED_FRAME_TYPE_BYTES: usize = size_of::<u32>();
pub(crate) const APPLE_SRP_NESTED_CHILD_LENGTH_BYTES: usize = size_of::<u16>();
pub(crate) const APPLE_SRP_NESTED_FRAME_HEADER_BYTES: usize =
    APPLE_SRP_NESTED_FRAME_TYPE_BYTES + APPLE_SRP_NESTED_CHILD_LENGTH_BYTES;
pub(crate) const APPLE_SRP_RESPONSE_ITEMS_BYTES: usize =
    size_of::<u8>() + APPLE_SRP_PROOF_BYTES + APPLE_SRP_RESPONSE_OPAQUE_TAIL_BYTES;
pub(crate) const APPLE_SRP_RESPONSE_BODY_BYTES: usize =
    APPLE_SRP_TLV_LENGTH_BYTES + APPLE_SRP_RESPONSE_ITEMS_BYTES;
pub(crate) const APPLE_SRP_RESPONSE_SUCCESS_LENGTH: usize = APPLE_SRP_RESPONSE_BODY_BYTES;
pub(crate) const APPLE_SRP_NESTED_RESPONSE_FRAME_BYTES: usize =
    APPLE_SRP_NESTED_FRAME_HEADER_BYTES + APPLE_SRP_RESPONSE_BODY_BYTES;
pub(crate) const APPLE_SRP_RESPONSE_FAILURE_DISCRIMINATOR: u32 = 1;
pub(crate) const APPLE_SRP_CHALLENGE_MIN_BYTES: usize = 64;
pub(crate) const APPLE_SRP_CHALLENGE_MAX_BYTES: usize = 8192;

const _: () = {
    assert!(APPLE_SRP_RESPONSE_ITEMS_BYTES == 88);
    assert!(APPLE_SRP_RESPONSE_BODY_BYTES == 92);
    assert!(APPLE_SRP_RESPONSE_SUCCESS_LENGTH == APPLE_SRP_RESPONSE_BODY_BYTES);
    assert!(APPLE_SRP_NESTED_RESPONSE_FRAME_BYTES == 98);
};

const APPLE_SRP_SALT_BYTES: usize = 32;
const APPLE_SRP_MIN_PBKDF2_ITERATIONS: u64 = 1;
const APPLE_SRP_MAX_PBKDF2_ITERATIONS: u64 = 10_000_000;

/// RFC 5054 Appendix A 的 4096-bit 素数；Apple 类型 36 不接受协商其他组。
const RFC5054_4096_PRIME_HEX: &[u8] = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74",
    "020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F143",
    "74FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7",
    "EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163B",
    "F0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F35620855",
    "2BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE",
    "3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817",
    "183995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33",
    "A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864",
    "D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2",
    "08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A92108011A723C12A787E6D7",
    "88719A10BDBA5B2699C327186AF4E23C1A946834B6150BDA2583E9CA2AD44CE8",
    "DBBBC2DB04DE8EF92E8EFC141FBECAA6287C59474E6BC05D99B2964FA090C3A2",
    "233BA186515BE7ED1F612970CEE2D7AFB81BDD762170481CD0069127D5B05AA9",
    "93B4EA988D8FDDC186FFB7DC90A6C08F4DF435C934063199FFFFFFFFFFFFFFFF"
)
.as_bytes();

pub(crate) fn apple_srp_prime() -> BigUint {
    BigUint::parse_bytes(RFC5054_4096_PRIME_HEX, 16)
        .expect("内置 RFC 5054 4096-bit 素数必须是有效十六进制")
}

/// corecrypto 固定宽度大端编码。远端大数超宽时返回错误，绝不发生减法下溢。
pub(crate) fn encode_srp_value_padded(value: &BigUint) -> Result<Vec<u8>> {
    let encoded = value.to_bytes_be();
    ensure!(
        encoded.len() <= APPLE_SRP_PADDED_BYTES,
        "SRP 大数超过 Apple 4096-bit 组宽度: {} 字节",
        encoded.len()
    );
    let mut padded = vec![0u8; APPLE_SRP_PADDED_BYTES - encoded.len()];
    padded.extend_from_slice(&encoded);
    Ok(padded)
}

#[cfg(test)]
fn b512(value: &BigUint) -> Vec<u8> {
    encode_srp_value_padded(value).unwrap()
}

fn validate_pbkdf2_iterations(raw: u64) -> Result<u32> {
    ensure!(
        (APPLE_SRP_MIN_PBKDF2_ITERATIONS..=APPLE_SRP_MAX_PBKDF2_ITERATIONS).contains(&raw),
        "SRP PBKDF2 迭代数异常: {raw}"
    );
    u32::try_from(raw).context("SRP PBKDF2 迭代数超过客户端表示范围")
}

fn validate_apple_srp_group(
    prime: &BigUint,
    generator: &BigUint,
    server_public: &BigUint,
) -> Result<()> {
    let expected_prime = apple_srp_prime();
    ensure!(
        prime == &expected_prime,
        "SRP 素数组不是 RFC 5054 4096-bit 组"
    );
    ensure!(
        generator == &BigUint::from(APPLE_SRP_GENERATOR),
        "SRP 生成元不是 Apple 4096-bit 组的 g=5"
    );
    ensure!(
        server_public > &BigUint::from(0u8) && server_public < prime,
        "SRP 服务器公钥 B 不在合法范围 1..N-1"
    );
    Ok(())
}

fn sha512(parts: &[&[u8]]) -> Vec<u8> {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().to_vec()
}

// ---------- TLV 项构造（%s / %o / %m） ----------

/// 认证帧的 u16 长度字段。转换必须在分配或写入对应帧前完成。
pub(crate) fn checked_u16_frame_length(length: usize, field: &str) -> Result<[u8; 2]> {
    Ok(u16::try_from(length)
        .with_context(|| format!("{field}长度超过 u16 表示范围"))?
        .to_be_bytes())
}

/// 认证帧的 u32 外层长度字段。转换必须在分配或写入对应帧前完成。
pub(crate) fn checked_u32_frame_length(length: usize, field: &str) -> Result<[u8; 4]> {
    Ok(u32::try_from(length)
        .with_context(|| format!("{field}长度超过 u32 表示范围"))?
        .to_be_bytes())
}

/// 已拥有的 SRP TLV 项构造器，负责 %s（u16）和 %o（u8）长度字段。
pub(crate) struct SrpTlvBuilder {
    items: Vec<u8>,
}

impl SrpTlvBuilder {
    pub(crate) fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub(crate) fn push_sized_u8(&mut self, data: &[u8]) -> Result<()> {
        let length = u8::try_from(data.len()).context("SRP %o 项长度超过 u8 表示范围")?;
        let reserve = data
            .len()
            .checked_add(size_of::<u8>())
            .context("SRP %o 项长度计算溢出")?;
        self.items
            .try_reserve(reserve)
            .context("SRP %o 项分配失败")?;
        self.items.push(length);
        self.items.extend_from_slice(data);
        Ok(())
    }

    pub(crate) fn push_sized_u16(&mut self, data: &[u8]) -> Result<()> {
        let length = checked_u16_frame_length(data.len(), "SRP %s 项")?;
        let reserve = data
            .len()
            .checked_add(size_of::<u16>())
            .context("SRP %s 项长度计算溢出")?;
        self.items
            .try_reserve(reserve)
            .context("SRP %s 项分配失败")?;
        self.items.extend_from_slice(&length);
        self.items.extend_from_slice(data);
        Ok(())
    }

    /// TLV 负载 = [u32 项总长][项…]
    pub(crate) fn finish(self) -> Result<Vec<u8>> {
        let outer_length = checked_u32_frame_length(self.items.len(), "SRP TLV 外层")?;
        let reserve = self
            .items
            .len()
            .checked_add(APPLE_SRP_TLV_LENGTH_BYTES)
            .context("SRP TLV 外层长度计算溢出")?;
        let mut payload = Vec::new();
        payload
            .try_reserve(reserve)
            .context("SRP TLV 外层分配失败")?;
        payload.extend_from_slice(&outer_length);
        payload.extend_from_slice(&self.items);
        Ok(payload)
    }
}

pub(crate) fn initial_auth_payload(username: &str) -> Result<Vec<u8>> {
    let mut builder = SrpTlvBuilder::new();
    builder.push_sized_u16(b"")?;
    builder.push_sized_u16(username.as_bytes())?;
    builder.push_sized_u16(b"")?;
    builder.push_sized_u8(b"")?;
    builder.finish()
}

fn prepend_u32_frame_length(payload: Vec<u8>, field: &str) -> Result<Vec<u8>> {
    let outer_length = checked_u32_frame_length(payload.len(), field)?;
    let reserve = payload
        .len()
        .checked_add(APPLE_SRP_FRAME_LENGTH_BYTES)
        .context("SRP 外层帧长度计算溢出")?;
    let mut frame = Vec::new();
    frame.try_reserve(reserve).context("SRP 外层帧分配失败")?;
    frame.extend_from_slice(&outer_length);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn parse_srp_tlv_body<'a>(body: &'a [u8], label: &str) -> Result<&'a [u8]> {
    ensure!(
        body.len() >= APPLE_SRP_TLV_LENGTH_BYTES,
        "{label}过短，缺少 TLV 长度字段"
    );
    let (declared_length, items) = body.split_at(APPLE_SRP_TLV_LENGTH_BYTES);
    let declared_length = usize::try_from(u32::from_be_bytes(
        declared_length
            .try_into()
            .context("SRP TLV 长度字段宽度内部错误")?,
    ))
    .context("SRP TLV 长度无法表示为 usize")?;
    ensure!(
        declared_length == items.len(),
        "{label} TLV 头校验失败: 声明 {declared_length}，实得 {}",
        items.len()
    );
    Ok(items)
}

fn parse_srp_challenge_frame(body: &[u8]) -> Result<&[u8]> {
    ensure!(
        (APPLE_SRP_CHALLENGE_MIN_BYTES..=APPLE_SRP_CHALLENGE_MAX_BYTES).contains(&body.len()),
        "SRP 挑战长度异常: {}",
        body.len()
    );
    parse_srp_tlv_body(body, "SRP 挑战")
}

#[derive(Debug)]
pub(crate) struct SrpResponse<'a> {
    pub(crate) proof: &'a [u8; APPLE_SRP_PROOF_BYTES],
    pub(crate) opaque_tail: &'a [u8; APPLE_SRP_RESPONSE_OPAQUE_TAIL_BYTES],
}

pub(crate) fn parse_srp_response_items(items: &[u8]) -> Result<SrpResponse<'_>> {
    ensure!(
        items.len() == APPLE_SRP_RESPONSE_ITEMS_BYTES,
        "SRP 响应项长度异常: {}",
        items.len()
    );
    let (tag, proof_and_tail) = items.split_first().context("SRP 响应缺少 tag")?;
    ensure!(
        *tag == APPLE_SRP_RESPONSE_TAG,
        "SRP 响应格式异常（H_AMK 长度位 = 0x{tag:02x}）"
    );
    let (proof, opaque_tail) = proof_and_tail.split_at(APPLE_SRP_PROOF_BYTES);
    Ok(SrpResponse {
        proof: proof.try_into().context("SRP 响应 H_AMK 长度异常")?,
        opaque_tail: opaque_tail.try_into().context("SRP 响应未知尾部长度异常")?,
    })
}

fn parse_srp_response_body(body: &[u8]) -> Result<SrpResponse<'_>> {
    ensure!(
        body.len() == APPLE_SRP_RESPONSE_BODY_BYTES,
        "SRP 响应长度异常: {}",
        body.len()
    );
    parse_srp_response_items(parse_srp_tlv_body(body, "SRP 响应")?)
}

/// 解析后的 SRP 挑战
#[derive(Clone)]
pub struct SrpChallenge {
    pub prime: BigUint,
    pub generator: BigUint,
    pub salt: Vec<u8>,
    pub server_pub: BigUint, // B
    pub pbkdf2_iterations: u32,
    /// 选项串（如 "mda=SHA-512,replay_detection,…"），step2 必须原样回传
    pub options: String,
}

/// 从 TLV 项缓冲中解析挑战（项格式 "%c%m%m%o%m%q%s"）
pub(crate) fn parse_challenge(items: &[u8]) -> Result<SrpChallenge> {
    let mut p = 0usize;
    let take_u16 = |p: &mut usize| -> Result<u16> {
        if *p + 2 > items.len() {
            bail!("SRP 挑战过短（u16@{p}）");
        }
        let v = u16::from_be_bytes([items[*p], items[*p + 1]]);
        *p += 2;
        Ok(v)
    };
    let take = |p: &mut usize, n: usize| -> Result<&[u8]> {
        if *p + n > items.len() {
            bail!("SRP 挑战过短（{n}B@{p}）");
        }
        let s = &items[*p..*p + n];
        *p += n;
        Ok(s)
    };

    let _cmd = take(&mut p, 1)?; // %c
    let n = usize::from(take_u16(&mut p)?); // %m 素数
    let prime = BigUint::from_bytes_be(take(&mut p, n)?);
    let n = usize::from(take_u16(&mut p)?); // %m g
    let generator = BigUint::from_bytes_be(take(&mut p, n)?);
    let n = usize::from(take(&mut p, 1)?[0]); // %o 盐
    let salt = take(&mut p, n)?.to_vec();
    let n = usize::from(take_u16(&mut p)?); // %m B
    let server_pub = BigUint::from_bytes_be(take(&mut p, n)?);
    // %q 迭代数为 8 字节 BE（实测 0x2625a = 156250，恰为 PBKDF2 迭代数）
    let iterations = validate_pbkdf2_iterations(u64::from_be_bytes(
        take(&mut p, size_of::<u64>())?.try_into()?,
    ))?;
    let n = usize::from(take_u16(&mut p)?); // %s 选项串
    let options = String::from_utf8_lossy(take(&mut p, n)?).into_owned();
    if p != items.len() {
        bail!("SRP 挑战解析后剩余 {} 字节（格式不匹配）", items.len() - p);
    }
    ensure!(salt.len() == APPLE_SRP_SALT_BYTES, "SRP 盐长度不是 32 字节");
    validate_apple_srp_group(&prime, &generator, &server_pub)?;
    Ok(SrpChallenge {
        prime,
        generator,
        salt,
        server_pub,
        pbkdf2_iterations: iterations,
        options,
    })
}

/// SRP 数学核心（与服务端 corecrypto 逐项对应）。返回 (A, M1, K)。
pub fn srp_compute(
    chal: &SrpChallenge,
    password: &str,
    mut random64: [u8; 64],
) -> Result<(
    BigUint,
    [u8; APPLE_SRP_PROOF_BYTES],
    [u8; APPLE_SRP_PROOF_BYTES],
)> {
    use pbkdf2::pbkdf2_hmac;

    let n = &chal.prime;
    let g = &chal.generator;
    validate_apple_srp_group(n, g, &chal.server_pub)?;

    // dk = PBKDF2(密码, 盐, 迭代数, 128)；x = SHA512(盐 ‖ SHA512(":" ‖ dk))
    let mut dk = [0u8; 128];
    pbkdf2_hmac::<Sha512>(
        password.as_bytes(),
        &chal.salt,
        chal.pbkdf2_iterations,
        &mut dk,
    );
    let x = BigUint::from_bytes_be(&sha512(&[&chal.salt, &sha512(&[b":", &dk])]));

    // a 私钥随机、A = g^a
    let a = BigUint::from_bytes_le(&random64) % n;
    random64.fill(0); // 抹除私钥材料
    let pub_a = g.modpow(&a, n);

    // k = SHA512(N‖g)、u = SHA512(A‖B)（全 512B 左填充）
    let n_padded = encode_srp_value_padded(n)?;
    let g_padded = encode_srp_value_padded(g)?;
    let pub_a_padded = encode_srp_value_padded(&pub_a)?;
    let server_pub_padded = encode_srp_value_padded(&chal.server_pub)?;
    let k = BigUint::from_bytes_be(&sha512(&[&n_padded, &g_padded]));
    let u = BigUint::from_bytes_be(&sha512(&[&pub_a_padded, &server_pub_padded]));

    // S = (B − k·g^x)^(a + u·x) mod N、K = SHA512(S₅₁₂)
    let gx = g.modpow(&x, n);
    let base = (&chal.server_pub + n - (&k * &gx % n)) % n;
    let s = base.modpow(&(&a + &u * &x), n);
    let shared_secret_padded = encode_srp_value_padded(&s)?;
    let key: [u8; APPLE_SRP_PROOF_BYTES] = sha512(&[&shared_secret_padded])
        .as_slice()
        .try_into()
        .context("SHA-512 输出长度内部错误")?;

    // M1 = SHA512(H(N)⊕H(g) ‖ SHA512("") ‖ 盐 ‖ A₅₁₂ ‖ B₅₁₂ ‖ K)
    let hn = sha512(&[&n_padded]);
    let hg = sha512(&[&g_padded]);
    let xor: Vec<u8> = hn.iter().zip(&hg).map(|(i, j)| i ^ j).collect();
    let m1: [u8; APPLE_SRP_PROOF_BYTES] = sha512(&[
        &xor,
        &sha512(&[b""]),
        &chal.salt,
        &pub_a_padded,
        &server_pub_padded,
        &key,
    ])
    .as_slice()
    .try_into()
    .context("SHA-512 输出长度内部错误")?;

    Ok((pub_a, m1, key))
}

/// 服务器证明 H_AMK = SHA512(A₅₁₂ ‖ M1 ‖ K)
pub(crate) fn expected_hamk(
    pub_a: &BigUint,
    m1: &[u8; APPLE_SRP_PROOF_BYTES],
    key: &[u8; APPLE_SRP_PROOF_BYTES],
) -> Result<[u8; APPLE_SRP_PROOF_BYTES]> {
    sha512(&[&encode_srp_value_padded(pub_a)?, m1, key])
        .as_slice()
        .try_into()
        .context("SHA-512 输出长度内部错误")
}

/// 执行类型 36 认证。成功后连接处于 ClientInit 前状态（SecurityResult 已消费）。
/// 返回 SRP 会话密钥 K = SHA512(S)——会话加密层的密钥种子（见 session.rs）。
pub fn authenticate(
    conn: &mut RfbConn,
    username: &str,
    password: &str,
) -> Result<[u8; APPLE_SRP_PROOF_BYTES]> {
    // step1：[36][u32][TLV]；用户名在第二个 %s 字段（服务器用它查 OD 记录）
    let payload = initial_auth_payload(username)?;
    let outer_length = checked_u32_frame_length(payload.len(), "SRP step1 外层帧")?;
    let step1_length = payload
        .len()
        .checked_add(size_of::<u8>() + APPLE_SRP_FRAME_LENGTH_BYTES)
        .context("SRP step1 帧长度计算溢出")?;
    let mut step1 = Vec::new();
    step1
        .try_reserve(step1_length)
        .context("SRP step1 帧分配失败")?;
    step1.push(protocol::security::APPLE_SRP);
    step1.extend_from_slice(&outer_length);
    step1.extend_from_slice(&payload);
    conn.write_all(&step1)?;

    // 挑战：[u32 L][u32 L-4][项…]
    let total = usize::try_from(conn.read_u32()?).context("SRP 挑战长度无法表示为 usize")?;
    if !(APPLE_SRP_CHALLENGE_MIN_BYTES..=APPLE_SRP_CHALLENGE_MAX_BYTES).contains(&total) {
        bail!("SRP 挑战长度异常: {total}");
    }
    let body = conn.read_vec(total)?;
    let challenge_items = parse_srp_challenge_frame(&body)?;
    let chal =
        parse_challenge(challenge_items).context("SRP 挑战解析失败（服务器版本可能不兼容）")?;

    // SRP 数学
    let mut rnd = [0u8; 64];
    getrandom::getrandom(&mut rnd).map_err(|e| anyhow::anyhow!("系统随机数失败: {e}"))?;
    let (pub_a, m1, key) = srp_compute(&chal, password, rnd)?;

    // step2：state 9 的消息不带 [36] 前缀；M1 紧跟 A，选项串原样回传，末尾 16B nonce
    let mut nonce = [0u8; APPLE_SRP_NONCE_BYTES];
    getrandom::getrandom(&mut nonce).map_err(|e| anyhow::anyhow!("系统随机数失败: {e}"))?;
    let mut builder = SrpTlvBuilder::new();
    builder.push_sized_u16(&encode_srp_value_padded(&pub_a)?)?; // %m 定长 512B
    builder.push_sized_u8(&m1)?;
    builder.push_sized_u16(chal.options.as_bytes())?;
    builder.push_sized_u8(&nonce)?;
    let step2 = prepend_u32_frame_length(builder.finish()?, "SRP step2 外层帧")?;
    conn.write_all(&step2)?;

    // 响应：成功 = [u32 92][u32 88][0x40][64B H_AMK][23B]，失败直接是 u32 1
    let first = conn.read_u32()?;
    if first == APPLE_SRP_RESPONSE_FAILURE_DISCRIMINATOR {
        bail!("SRP 认证失败（凭据错误，或账号无屏幕共享权限）");
    }
    if usize::try_from(first).context("SRP 响应长度无法表示为 usize")?
        != APPLE_SRP_RESPONSE_SUCCESS_LENGTH
    {
        bail!("SRP 响应长度异常: {first}");
    }
    let response_body = conn.read_vec(APPLE_SRP_RESPONSE_SUCCESS_LENGTH)?;
    let response = parse_srp_response_body(&response_body)?;
    let _opaque_tail = response.opaque_tail;
    if response.proof != &expected_hamk(&pub_a, &m1, &key)? {
        // 能走到这一步说明 M1 已被服务器接受，H_AMK 不一致只可能是中间人
        bail!("SRP 服务器证明校验失败（疑似中间人攻击）");
    }

    // RFB SecurityResult
    if conn.read_u32()? != protocol::RFB_SECURITY_RESULT_OK {
        bail!("SRP 认证被服务器拒绝（SecurityResult != 0）");
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vnc::client;
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
    fn shared_srp_wire_owner_relationships_match_independent_literals() {
        assert_eq!(APPLE_SRP_PADDED_BYTES, 512);
        assert_eq!(APPLE_SRP_NONCE_BYTES, 16);
        assert_eq!(APPLE_SRP_PROOF_BYTES, 64);
        assert_eq!(APPLE_SRP_RESPONSE_TAG, 0x40);
        assert_eq!(APPLE_SRP_RESPONSE_OPAQUE_TAIL_BYTES, 23);
        assert_eq!(APPLE_SRP_RESPONSE_ITEMS_BYTES, 88);
        assert_eq!(APPLE_SRP_RESPONSE_BODY_BYTES, 92);
        assert_eq!(APPLE_SRP_RESPONSE_SUCCESS_LENGTH, 92);
        assert_eq!(APPLE_SRP_NESTED_RESPONSE_FRAME_BYTES, 98);
        assert_eq!(APPLE_SRP_CHALLENGE_MIN_BYTES, 64);
        assert_eq!(APPLE_SRP_CHALLENGE_MAX_BYTES, 8192);
        assert_eq!(APPLE_SRP_TLV_LENGTH_BYTES, 4);
        assert_eq!(APPLE_SRP_NESTED_FRAME_HEADER_BYTES, 6);
    }

    #[test]
    fn srp_challenge_frame_rejects_truncation_wrong_length_and_out_of_bounds() {
        let mut minimum = vec![0u8; 64];
        minimum[..4].copy_from_slice(&60u32.to_be_bytes());
        assert_eq!(parse_srp_challenge_frame(&minimum).unwrap().len(), 60);

        let mut maximum = vec![0u8; 8192];
        maximum[..4].copy_from_slice(&8188u32.to_be_bytes());
        assert_eq!(parse_srp_challenge_frame(&maximum).unwrap().len(), 8188);

        assert!(parse_srp_challenge_frame(&[0u8; 63]).is_err());
        assert!(parse_srp_challenge_frame(&vec![0u8; 8193]).is_err());

        let mut wrong_inner = vec![0u8; 64];
        wrong_inner[..4].copy_from_slice(&59u32.to_be_bytes());
        assert!(parse_srp_challenge_frame(&wrong_inner).is_err());
    }

    #[test]
    fn srp_response_layout_accepts_independent_literal_success_fixture() {
        let mut body = vec![0, 0, 0, 88, 0x40];
        body.extend_from_slice(&[0x5a; 64]);
        body.extend_from_slice(&[0xa5; 23]);

        let response = parse_srp_response_body(&body).unwrap();
        assert_eq!(response.proof, &[0x5a; 64]);
        assert_eq!(response.opaque_tail, &[0xa5; 23]);
    }

    #[test]
    fn srp_response_layout_rejects_wrong_inner_declared_length() {
        let mut body = vec![0, 0, 0, 87, 0x40];
        body.extend_from_slice(&[0x5a; 64]);
        body.extend_from_slice(&[0xa5; 23]);

        let error = parse_srp_response_body(&body).unwrap_err();
        assert!(error.to_string().contains("TLV"), "{error:#}");
    }

    #[test]
    fn srp_response_layout_rejects_truncation_and_wrong_tag() {
        assert!(parse_srp_response_body(&[0u8; 91]).is_err());

        let mut body = vec![0, 0, 0, 88, 0x41];
        body.extend_from_slice(&[0x5a; 64]);
        body.extend_from_slice(&[0xa5; 23]);
        assert!(parse_srp_response_body(&body).is_err());
    }

    #[test]
    fn oversized_srp_username_fails_before_socket_write() {
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

    #[test]
    fn srp_tlv_builder_rejects_oversized_u8_item() {
        let mut builder = SrpTlvBuilder::new();
        let data = vec![0u8; usize::from(u8::MAX) + 1];
        assert!(builder.push_sized_u8(&data).is_err());
    }

    #[test]
    fn srp_tlv_builder_rejects_oversized_u16_item() {
        let mut builder = SrpTlvBuilder::new();
        let data = vec![0u8; usize::from(u16::MAX) + 1];
        assert!(builder.push_sized_u16(&data).is_err());
    }

    #[test]
    fn srp_tlv_builder_encodes_exact_u8_and_u16_boundaries() {
        let mut builder = SrpTlvBuilder::new();
        let u8_max = vec![0xa5; usize::from(u8::MAX)];
        let u16_max = vec![0x5a; usize::from(u16::MAX)];
        builder.push_sized_u8(&u8_max).unwrap();
        builder.push_sized_u16(&u16_max).unwrap();

        let payload = builder.finish().unwrap();
        assert_eq!(payload[4], u8::MAX);
        assert_eq!(&payload[260..262], &u16::MAX.to_be_bytes());
    }

    #[test]
    fn srp_tlv_builder_finish_writes_checked_outer_length() {
        let mut builder = SrpTlvBuilder::new();
        builder.push_sized_u16(b"abc").unwrap();
        builder.push_sized_u8(b"de").unwrap();

        assert_eq!(
            builder.finish().unwrap(),
            vec![0, 0, 0, 8, 0, 3, b'a', b'b', b'c', 2, b'd', b'e']
        );
    }

    #[test]
    fn fixed_width_srp_encoding_rejects_oversized_values() {
        let oversized = BigUint::from_bytes_be(&vec![0xff; APPLE_SRP_PADDED_BYTES + 1]);
        assert!(encode_srp_value_padded(&oversized).is_err());
    }

    #[test]
    fn pbkdf2_iteration_validation_happens_before_integer_narrowing() {
        let wrapped_to_one = u64::from(u32::MAX) + 2;
        assert!(validate_pbkdf2_iterations(wrapped_to_one).is_err());
        assert_eq!(validate_pbkdf2_iterations(156_250).unwrap(), 156_250);
    }

    #[test]
    fn apple_srp_group_validation_rejects_untrusted_parameters() {
        let wrong_prime = BigUint::from(23u8);
        let expected_generator = BigUint::from(APPLE_SRP_GENERATOR);
        let plausible_server_public = BigUint::from(7u8);
        assert!(validate_apple_srp_group(
            &wrong_prime,
            &expected_generator,
            &plausible_server_public
        )
        .is_err());
    }

    /// 端到端：mock 一个 type-36 服务器，按同一套配方扮演服务端（生成 verifier、B、
    /// 验证 M1、回 H_AMK），验证客户端全流程。
    #[test]
    fn srp_auth_round_trip() {
        // 认证入口必须钉死 Apple 实际使用的 RFC 5054 4096-bit 组。
        let n = apple_srp_prime();
        let g: BigUint = 5u32.into();
        let salt = [0x11u8; 32];
        let iters = 100u32; // 测试少迭代
        let user = "test-user";
        let pass = "test-password";
        let options =
            "mda=SHA-512,replay_detection,conf+int=ChaCha20-Poly1305,kdf=SALTED-SHA512-PBKDF2";

        // 服务端 verifier 生成（与客户端 x 公式一致）
        let mut dk = [0u8; 128];
        pbkdf2::pbkdf2_hmac::<Sha512>(pass.as_bytes(), &salt, iters, &mut dk);
        let x = BigUint::from_bytes_be(&sha512(&[&salt, &sha512(&[b":", &dk])]));
        let v = g.modpow(&x, &n);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();

            // step1：[36][u32 L][u32 L-4][项…]
            let mut hdr = [0u8; 5];
            s.read_exact(&mut hdr).unwrap();
            assert_eq!(hdr[0], 36);
            let len = usize::try_from(u32::from_be_bytes(hdr[1..5].try_into().unwrap())).unwrap();
            let mut body = vec![0u8; len];
            s.read_exact(&mut body).unwrap();
            assert_eq!(
                usize::try_from(u32::from_be_bytes(body[..4].try_into().unwrap())).unwrap(),
                len - 4
            );
            // 项：u16(0) u16(3)"test-user" u16(0) u8(0) —— 用户名在第二个字段
            let mut expect = vec![0u8, 0];
            expect.extend_from_slice(&u16::try_from(user.len()).unwrap().to_be_bytes());
            expect.extend_from_slice(user.as_bytes());
            expect.extend_from_slice(&[0, 0, 0]);
            assert_eq!(&body[4..], &expect, "step1 项内容");

            // 挑战：[u32 L][u32 L-4][c|m p|m g|o 盐|m B|q 迭代|s 选项]
            let b_srv = BigUint::from(1145141919810u64); // 服务器私钥（任意）
            let k = BigUint::from_bytes_be(&sha512(&[&b512(&n), &b512(&g)]));
            let big_b = (&k * &v + g.modpow(&b_srv, &n)) % &n;
            let mut items = vec![0u8]; // %c cmd
            items.extend_from_slice(&fixture_sized_u16(&b512(&n)));
            items.extend_from_slice(&fixture_sized_u16(&[5u8])); // g 最简表示
            items.extend_from_slice(&fixture_sized_u8(&salt));
            items.extend_from_slice(&fixture_sized_u16(&b512(&big_b)));
            items.extend_from_slice(&(iters as u64).to_be_bytes());
            items.extend_from_slice(&fixture_sized_u16(options.as_bytes()));
            let items_len = u32::try_from(items.len()).unwrap();
            let mut msg = items_len.checked_add(4).unwrap().to_be_bytes().to_vec();
            msg.extend_from_slice(&items_len.to_be_bytes());
            msg.extend_from_slice(&items);
            s.write_all(&msg).unwrap();

            // step2：[u32 L][u32 L-4][m A|o M1|s 选项|o nonce]
            let mut h4 = [0u8; 4];
            s.read_exact(&mut h4).unwrap();
            let len = usize::try_from(u32::from_be_bytes(h4)).unwrap();
            let mut body = vec![0u8; len];
            s.read_exact(&mut body).unwrap();
            assert_eq!(
                usize::try_from(u32::from_be_bytes(body[..4].try_into().unwrap())).unwrap(),
                len - 4
            );
            let mut p = 4usize;
            let alen = usize::from(u16::from_be_bytes([body[p], body[p + 1]]));
            p += 2;
            let big_a = BigUint::from_bytes_be(&body[p..p + alen]);
            p += alen;
            assert_eq!(body[p], 64); // %o M1
            let m1 = &body[p + 1..p + 65];
            p += 65;
            let olen = usize::from(u16::from_be_bytes([body[p], body[p + 1]]));
            p += 2;
            assert_eq!(&body[p..p + olen], options.as_bytes(), "选项串回传");
            p += olen;
            assert_eq!(body[p], 16, "尾部 nonce"); // %o nonce

            // 服务端验证：u、S、K、M1'（S = (A · v^u)^b）
            let u = BigUint::from_bytes_be(&sha512(&[&b512(&big_a), &b512(&big_b)]));
            let s_srv = (&big_a * &v.modpow(&u, &n) % &n).modpow(&b_srv, &n);
            let key = sha512(&[&b512(&s_srv)]);
            let hn = sha512(&[&b512(&n)]);
            let hg = sha512(&[&b512(&g)]);
            let xor: Vec<u8> = hn.iter().zip(&hg).map(|(i, j)| i ^ j).collect();
            let m1_srv = sha512(&[
                &xor,
                &sha512(&[b""]),
                &salt,
                &b512(&big_a),
                &b512(&big_b),
                &key,
            ]);
            assert_eq!(m1, &m1_srv[..], "M1 不匹配");

            // 响应：[u32 92][u32 88][0x40][H_AMK][23B]
            let hamk = sha512(&[&b512(&big_a), &m1_srv, &key]);
            let mut resp = 92u32.to_be_bytes().to_vec();
            resp.extend_from_slice(&88u32.to_be_bytes());
            resp.push(0x40);
            resp.extend_from_slice(&hamk);
            resp.extend_from_slice(&[0u8; 23]);
            s.write_all(&resp).unwrap();
            s.write_all(&0u32.to_be_bytes()).unwrap(); // SecurityResult
        });

        let stream = std::net::TcpStream::connect(addr).unwrap();
        let mut conn = client::RfbConn::new(stream);
        authenticate(&mut conn, user, pass).unwrap();
        server.join().unwrap();
    }
}
