//! VNC Authentication（RFC 6143 §7.2.2）：DES-ECB 挑战-响应。
//!
//! 流程：服务器下发 16 字节随机挑战 -> 客户端用密码派生的 DES 密钥
//! 以 ECB 模式加密挑战 -> 回传 16 字节应答。
//!
//! 密码派生规则（VNC 传统约定）：
//! 1. 密码截断或补零到正好 8 字节（所以标准 VNC 密码只有前 8 位有效）；
//! 2. 每个字节按位反转。这是因为 VNC 的 DES 实现遵循 LSB-first 的
//!    密钥位约定，而标准 DES（FIPS 46）是 MSB-first，二者恰好互为镜像。

use des::cipher::generic_array::GenericArray;
use des::cipher::{BlockEncrypt, KeyInit};
use des::Des;

pub const VNC_DES_KEY_BYTES: usize = 8;
pub const VNC_AUTH_CHALLENGE_BYTES: usize = 16;
pub const DES_BLOCK_BYTES: usize = 8;

pub type VncAuthChallenge = [u8; VNC_AUTH_CHALLENGE_BYTES];
pub type VncAuthResponse = [u8; VNC_AUTH_CHALLENGE_BYTES];

const _: [(); 0] = [(); VNC_AUTH_CHALLENGE_BYTES % DES_BLOCK_BYTES];

/// 密码派生 DES 密钥：截断/补零到 8 字节，每字节按位反转
pub fn vnc_des_key(password: &str) -> [u8; VNC_DES_KEY_BYTES] {
    let pw = password.as_bytes();
    let mut key = [0u8; VNC_DES_KEY_BYTES];
    for (i, k) in key.iter_mut().enumerate() {
        *k = if i < pw.len() {
            pw[i].reverse_bits()
        } else {
            0
        };
    }
    key
}

pub fn vnc_des_challenge_response(challenge: &VncAuthChallenge, password: &str) -> VncAuthResponse {
    let key = vnc_des_key(password);
    let cipher = Des::new_from_slice(&key).expect("DES 密钥固定为 8 字节");

    let mut out = [0u8; VNC_AUTH_CHALLENGE_BYTES];
    for (i, chunk) in challenge.chunks_exact(DES_BLOCK_BYTES).enumerate() {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        let output_start = i * DES_BLOCK_BYTES;
        out[output_start..output_start + DES_BLOCK_BYTES].copy_from_slice(&block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use des::cipher::{BlockDecrypt, KeyInit};

    /// 密钥派生向量（来自 Vidar Holen 对 RealVNC d3des.c 的分析）：
    /// 密码 "COW" 按位反转后得到密钥 C2 F2 EA 00 00 00 00 00。
    /// （'C'=0x43 反转=0xC2，'O'=0x4F 反转=0xF2，'W'=0x57 反转=0xEA）
    #[test]
    fn key_derivation_vector() {
        assert_eq!(vnc_des_key("COW"), [0xC2, 0xF2, 0xEA, 0, 0, 0, 0, 0]);
    }

    /// ECB 逐块往返：用派生密钥解密应答应还原出原始挑战
    #[test]
    fn ecb_round_trip() {
        let challenge: VncAuthChallenge = core::array::from_fn(|i| i as u8 ^ 0xA5);
        let resp = vnc_des_challenge_response(&challenge, "PassW0rd");
        let decipher = Des::new_from_slice(&vnc_des_key("PassW0rd")).unwrap();
        let mut back = [0u8; VNC_AUTH_CHALLENGE_BYTES];
        for (i, chunk) in resp.chunks_exact(DES_BLOCK_BYTES).enumerate() {
            let mut block = GenericArray::clone_from_slice(chunk);
            decipher.decrypt_block(&mut block);
            let output_start = i * DES_BLOCK_BYTES;
            back[output_start..output_start + DES_BLOCK_BYTES].copy_from_slice(&block);
        }
        assert_eq!(back, challenge);
    }
}
