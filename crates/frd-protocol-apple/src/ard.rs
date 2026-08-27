//! ARD 认证（安全类型 30）：DH 密钥交换 + MD5 派生 + AES-128-ECB 凭据块。
//!
//! 字节布局（2026-08 对 macOS 26.6.1 实测 + 2017 Tenable 抓包交叉确认，
//! 详见 docs/ARD_PROTOCOL.md §4）：
//!
//! ```text
//! 服务器 → 客户端:  u16 g | u16 keyLen | 模数[keyLen] | 服务器公钥[keyLen]
//! 客户端 → 服务器:  AES-128-ECB(MD5(共享密钥), 用户名[64]||密码[64]) | 客户端公钥[keyLen]
//! 服务器 → 客户端:  u32 结果（0 = 成功）
//! ```
//!
//! - DH 参数由服务器下发：macOS 26 实测为 g=5、4096-bit（RFC 5054 的 4096 组模数），
//!   客户端必须使用线上值而非硬编码；
//! - 共享密钥与客户端公钥都要**定长大端表示**（左侧补零到 keyLen 字节）；
//! - 凭据块固定 128 字节（用户名/密码各 64 字节、NUL 结尾、余零填充），
//!   AES-128-ECB 无 IV 无填充，恰好 8 个分组；
//! - 凭据是 **Mac 的真实本地账号**（需有屏幕共享权限），不是 VNC 密码。

use anyhow::{bail, Result};
use md5::Digest;
use num_bigint::BigUint;

use crate::connection::AppleConnection;
use crate::protocol::RFB_SECURITY_RESULT_OK;

pub const ARD_DH_KEY_MIN_BYTES: usize = 64;
pub const ARD_DH_KEY_MAX_BYTES: usize = 1024;
pub const ARD_PRIVATE_EXPONENT_BYTES: usize = 64;
pub const ARD_CREDENTIAL_FIELD_BYTES: usize = 64;
pub const ARD_CREDENTIAL_BLOB_BYTES: usize = 128;
pub const AES_128_BLOCK_BYTES: usize = 16;

const ARD_USERNAME_FIELD_OFFSET: usize = 0;
const ARD_PASSWORD_FIELD_OFFSET: usize = ARD_CREDENTIAL_FIELD_BYTES;
const _: [(); ARD_CREDENTIAL_BLOB_BYTES] = [(); 2 * ARD_CREDENTIAL_FIELD_BYTES];
const _: [(); 0] = [(); ARD_CREDENTIAL_BLOB_BYTES % AES_128_BLOCK_BYTES];

/// 左侧补零到 n 字节（DH 值的定长表示，前导零参与 MD5）
fn pad_left(bytes: &[u8], n: usize) -> Vec<u8> {
    debug_assert!(bytes.len() <= n);
    let mut out = vec![0u8; n - bytes.len()];
    out.extend_from_slice(bytes);
    out
}

/// 把用户名/密码写进 64 字节槽位（NUL 结尾、余零填充）
fn put_field(slot: &mut [u8], s: &str) -> Result<()> {
    let b = s.as_bytes();
    if b.len() >= slot.len() {
        bail!("ARD 认证字段超长（{} >= {} 字节）", b.len(), slot.len());
    }
    slot[..b.len()].copy_from_slice(b);
    // 其余保持为 0
    Ok(())
}

/// 执行类型 30 认证。成功后连接处于 ClientInit 前状态（与类型 2 一致）。
pub fn authenticate(conn: &mut AppleConnection, username: &str, password: &str) -> Result<()> {
    // 1. 读服务器 DH 材料
    let g = conn.read_u16()? as u32;
    let key_len = conn.read_u16()? as usize;
    // 合理范围：1024-bit ~ 8192-bit；太小的服务器不合法，太大的会被用来打内存
    if !(ARD_DH_KEY_MIN_BYTES..=ARD_DH_KEY_MAX_BYTES).contains(&key_len) {
        bail!("ARD 认证材料长度异常: keyLen={key_len} 字节");
    }
    let modulus = BigUint::from_bytes_be(&conn.read_vec(key_len)?);
    let srv_pub = BigUint::from_bytes_be(&conn.read_vec(key_len)?);
    if modulus <= BigUint::from(1u32) {
        bail!("ARD 认证收到非法模数");
    }
    let g = BigUint::from(g);

    // 2. 客户端密钥对与共享密钥（私钥指数取 512-bit 随机数，与 nmap 参考实现一致）
    let mut rnd = [0u8; ARD_PRIVATE_EXPONENT_BYTES];
    getrandom::getrandom(&mut rnd).map_err(|e| anyhow::anyhow!("系统随机数失败: {e}"))?;
    let secret = BigUint::from_bytes_le(&rnd) % &modulus;
    let cli_pub = g.modpow(&secret, &modulus);
    let shared = srv_pub.modpow(&secret, &modulus);

    // 3. AES 密钥 = MD5(共享密钥定长表示)
    let shared_bytes = pad_left(&shared.to_bytes_be(), key_len);
    let aes_key: [u8; AES_128_BLOCK_BYTES] = md5::Md5::digest(&shared_bytes).into();

    // 4. 凭据块 128B：用户名[64] + 密码[64]
    //    （2026-08-18 经 macOS 26.6.1 服务器日志验证：解密与字段解析均正确）
    let mut blob = [0u8; ARD_CREDENTIAL_BLOB_BYTES];
    put_field(
        &mut blob[ARD_USERNAME_FIELD_OFFSET..ARD_PASSWORD_FIELD_OFFSET],
        username,
    )?;
    put_field(
        &mut blob[ARD_PASSWORD_FIELD_OFFSET..ARD_CREDENTIAL_BLOB_BYTES],
        password,
    )?;

    // 5. AES-128-ECB（无填充，整块加密 8 个分组）
    use aes::cipher::{BlockEncryptMut, KeyInit};
    let mut enc = <ecb::Encryptor<aes::Aes128>>::new((&aes_key).into());
    for block in blob.chunks_exact_mut(AES_128_BLOCK_BYTES) {
        enc.encrypt_block_mut(block.into());
    }

    // 6. 密文在前、公钥在后（反汇编 0x100014367-0x1000143a3 确认的服务器读序）
    conn.write_all(&blob)?;
    conn.write_all(&pad_left(&cli_pub.to_bytes_be(), key_len))?;

    // 7. u32 结果
    if conn.read_u32()? != RFB_SECURITY_RESULT_OK {
        bail!("ARD 认证失败（检查用户名/密码，以及该账号是否被允许使用屏幕共享）");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::AppleConnection;
    use aes::cipher::{BlockDecryptMut, KeyInit};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// 端到端：mock 一个 type-30 服务器（发送 DH 材料、按同样算法解密凭据块），
    /// 验证客户端能通过认证。用小号假模数即可——DH 等式 g^(cs) 对任意 p 成立。
    #[test]
    fn ard_auth_round_trip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            // 服务器材料：128 字节假模数、g=5
            let modulus = BigUint::parse_bytes(
                b"FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE65381FFFFFFFFFFFFFFFF",
                16,
            ).unwrap();
            let key_len = 128usize;
            let g: BigUint = 5u32.into();
            let b_secret = BigUint::from(1145141919810u64); // 服务器私钥（任意）
            let srv_pub = g.modpow(&b_secret, &modulus);
            let mut m = Vec::new();
            m.extend_from_slice(&5u16.to_be_bytes());
            m.extend_from_slice(&(key_len as u16).to_be_bytes());
            m.extend_from_slice(&pad_left(&modulus.to_bytes_be(), key_len));
            m.extend_from_slice(&pad_left(&srv_pub.to_bytes_be(), key_len));
            s.write_all(&m).unwrap();

            // 读客户端响应：128B 密文 + keyLen 公钥
            let mut resp = vec![0u8; 128 + key_len];
            s.read_exact(&mut resp).unwrap();
            let (ct, cli_pub) = resp.split_at(128);
            let cli_pub = BigUint::from_bytes_be(cli_pub);

            // 服务器侧推导共享密钥并解密
            let shared = cli_pub.modpow(&b_secret, &modulus);
            let key: [u8; 16] = md5::Md5::digest(pad_left(&shared.to_bytes_be(), key_len)).into();
            let mut dec = <ecb::Decryptor<aes::Aes128>>::new_from_slice(&key).unwrap();
            let mut plain = ct.to_vec();
            for block in plain.chunks_exact_mut(16) {
                dec.decrypt_block_mut(block.into());
            }
            let user = plain[..64].split(|&c| c == 0).next().unwrap().to_vec();
            let pass = plain[64..].split(|&c| c == 0).next().unwrap().to_vec();
            assert_eq!(user, b"test-user".to_vec());
            assert_eq!(pass, b"test-password".to_vec());
            s.write_all(&0u32.to_be_bytes()).unwrap();
        });

        // 直接复用 RfbConn 驱动客户端逻辑（跳过版本协商，只测 type-30 部分）
        let stream = std::net::TcpStream::connect(addr).unwrap();
        let mut conn = AppleConnection::new(stream);
        authenticate(&mut conn, "test-user", "test-password").unwrap();
        server.join().unwrap();
    }
}
