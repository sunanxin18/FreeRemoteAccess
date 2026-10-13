//! 冻结原版输出格式；只做序列化/索引，没有 decoder 数值公式。
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub type Records = BTreeMap<[u8; 32], Vec<u8>>;
pub fn i16_bytes(data: &[i16]) -> Vec<u8> {
    data.iter().flat_map(|x| x.to_le_bytes()).collect()
}
pub fn i16s(bytes: &[u8]) -> Vec<i16> {
    assert_eq!(bytes.len() % 2, 0);
    bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}
pub fn key(op: u8, parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update([op]);
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}
pub fn encode(records: &Records) -> Vec<u8> {
    let mut out = b"FRDORC01".to_vec();
    for (key, value) in records {
        out.extend(key);
        out.extend((value.len() as u32).to_le_bytes());
        out.extend(value);
    }
    out
}
pub fn decode(mut data: &[u8]) -> Records {
    assert_eq!(&data[..8], b"FRDORC01");
    data = &data[8..];
    let mut out = Records::new();
    while !data.is_empty() {
        let key = data[..32].try_into().unwrap();
        let len = u32::from_le_bytes(data[32..36].try_into().unwrap()) as usize;
        assert!(out.insert(key, data[36..36 + len].to_vec()).is_none());
        data = &data[36 + len..];
    }
    out
}
