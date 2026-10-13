#[path = "../../../crates/frd-rdp-legacy-kernels/tests/common/snapshot_format.rs"]
#[allow(dead_code)]
mod format;
mod planar;
mod snapshots;
mod wire;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
fn fixture(path: &Path, bytes: &[u8], verify: bool, compressed: bool) {
    if verify {
        let data = fs::read(path).unwrap();
        if compressed {
            assert_eq!(
                format!("{:x}", Sha256::digest(&data)),
                fs::read_to_string(path.with_extension("sha256"))
                    .unwrap()
                    .trim()
            );
        }
        let actual = if compressed {
            let mut out = Vec::new();
            GzDecoder::new(data.as_slice())
                .read_to_end(&mut out)
                .unwrap();
            out
        } else {
            data
        };
        assert!(
            actual == bytes,
            "{} mismatch: actual_sha256={:x} expected_sha256={:x}",
            path.display(),
            Sha256::digest(&actual),
            Sha256::digest(bytes)
        );
    } else {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let data = if compressed {
            let mut gz = GzEncoder::new(Vec::new(), Compression::best());
            gz.write_all(bytes).unwrap();
            gz.finish().unwrap()
        } else {
            bytes.to_vec()
        };
        fs::write(path, &data).unwrap();
        fs::write(
            path.with_extension("sha256"),
            format!("{:x}\n", Sha256::digest(&data)),
        )
        .unwrap();
    }
    println!(
        "{} raw_bytes={} raw_sha256={:x}",
        path.display(),
        bytes.len(),
        Sha256::digest(bytes)
    );
}
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "verify".into());
    assert!(mode == "generate" || mode == "verify");
    let verify = mode == "verify";
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let k = root.join("crates/frd-rdp-legacy-kernels/tests/fixtures");
    let records = snapshots::generate();
    println!("snapshot_records={}", records.len());
    let blob = format::encode(&records);
    assert!(
        format::decode(&blob) == records,
        "snapshot serialization mismatch"
    );
    fixture(&k.join("original-leaves.bin.gz"), &blob, verify, true);
    fixture(
        &k.join("planar-original.bin"),
        &planar::generate(),
        true,
        false,
    );
    let (catalog, composition) = wire::generate();
    let mut replay = b"FRDRLG01".to_vec();
    for (index, entry) in catalog["entries"].as_array().unwrap().iter().enumerate() {
        if entry["codec"] == "rlgr" && entry["result"] == "Ok(())" {
            let bytes: Vec<u8> = entry["output_or_partial"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| u8::try_from(v.as_u64().unwrap()).unwrap())
                .collect();
            replay.extend(u32::try_from(index).unwrap().to_le_bytes());
            replay.extend(u32::try_from(bytes.len()).unwrap().to_le_bytes());
            replay.extend(bytes);
        }
    }
    fixture(
        &k.join("original-rlgr-replay.bin.gz"),
        &replay,
        verify,
        true,
    );
    fixture(
        &root.join("tests/legacy-oracle/fixtures/wire-catalog.json.gz"),
        &serde_json::to_vec_pretty(&catalog).unwrap(),
        verify,
        true,
    );
    fixture(
        &k.join("original-composition.bin"),
        &composition,
        verify,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verification_rejects_corrupt_bytes_and_wrong_original_expectation() {
        let dir = std::env::temp_dir().join(format!("frd-oracle-verify-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.bin.gz");
        fixture(&path, b"original bytes", false, true);
        fixture(&path, b"original bytes", true, true);
        assert!(
            std::panic::catch_unwind(|| fixture(&path, b"different bytes", true, true)).is_err()
        );
        let mut corrupt = fs::read(&path).unwrap();
        corrupt[0] ^= 1;
        fs::write(&path, corrupt).unwrap();
        assert!(
            std::panic::catch_unwind(|| fixture(&path, b"original bytes", true, true)).is_err()
        );
        fs::remove_file(&path).unwrap();
        fs::remove_file(path.with_extension("sha256")).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
