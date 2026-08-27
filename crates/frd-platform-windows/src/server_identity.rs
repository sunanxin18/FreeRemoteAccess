use std::path::{Path, PathBuf};

use frd_core::{Endpoint, ProtocolId};
use frd_platform_api::{PlatformError, ServerIdentityStore};
use sha2::{Digest, Sha256};

const RECORD_MAGIC: &[u8; 8] = b"FRDPIN01";

pub struct DpapiServerIdentityStore {
    root: PathBuf,
}

impl DpapiServerIdentityStore {
    pub fn current_user_default() -> Result<Self, PlatformError> {
        let local_app_data = std::env::var_os("LOCALAPPDATA").ok_or(PlatformError::Unavailable)?;
        Ok(Self::at_path(
            PathBuf::from(local_app_data)
                .join("FreeRemoteDesk")
                .join("server-identity-pins"),
        ))
    }

    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        Self { root: path.into() }
    }

    fn record_path(&self, protocol: &ProtocolId, endpoint: &Endpoint) -> PathBuf {
        let mut hash = Sha256::new();
        hash.update(protocol.as_str().as_bytes());
        hash.update([0]);
        hash.update(endpoint.host().as_bytes());
        hash.update([0]);
        hash.update(endpoint.port().to_le_bytes());
        let name = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.root.join(format!("{name}.pin"))
    }
}

impl ServerIdentityStore for DpapiServerIdentityStore {
    fn load_pin(
        &self,
        protocol: &ProtocolId,
        endpoint: &Endpoint,
    ) -> Result<Option<[u8; 32]>, PlatformError> {
        let path = self.record_path(protocol, endpoint);
        let encrypted = match std::fs::read(path) {
            Ok(encrypted) => encrypted,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(PlatformError::StorageFailed),
        };
        let mut plaintext = unprotect_current_user(&encrypted)?;
        let decoded = decode_record(&plaintext, protocol, endpoint);
        wipe_bytes(&mut plaintext);
        decoded.map(Some)
    }

    fn store_pin(
        &self,
        protocol: &ProtocolId,
        endpoint: &Endpoint,
        pin: [u8; 32],
    ) -> Result<(), PlatformError> {
        if let Some(existing) = self.load_pin(protocol, endpoint)? {
            return if existing == pin {
                Ok(())
            } else {
                Err(PlatformError::ServerIdentityPinMismatch)
            };
        }

        std::fs::create_dir_all(&self.root).map_err(|_| PlatformError::StorageFailed)?;
        let mut plaintext = encode_record(protocol, endpoint, pin)?;
        let encrypted = protect_current_user(&plaintext);
        wipe_bytes(&mut plaintext);
        let encrypted = encrypted?;
        let path = self.record_path(protocol, endpoint);
        let temporary = temporary_path(&path);
        std::fs::write(&temporary, encrypted).map_err(|_| PlatformError::StorageFailed)?;
        std::fs::rename(&temporary, &path).map_err(|_| PlatformError::StorageFailed)
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    PathBuf::from(temporary)
}

fn wipe_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY：byte 是此自有缓冲区中唯一借用的有效元素。
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

fn encode_record(
    protocol: &ProtocolId,
    endpoint: &Endpoint,
    pin: [u8; 32],
) -> Result<Vec<u8>, PlatformError> {
    let protocol_len =
        u32::try_from(protocol.as_str().len()).map_err(|_| PlatformError::StorageFailed)?;
    let host_len =
        u32::try_from(endpoint.host().len()).map_err(|_| PlatformError::StorageFailed)?;
    let mut record = Vec::with_capacity(
        RECORD_MAGIC.len()
            + 4
            + 4
            + 2
            + protocol.as_str().len()
            + endpoint.host().len()
            + pin.len(),
    );
    record.extend_from_slice(RECORD_MAGIC);
    record.extend_from_slice(&protocol_len.to_le_bytes());
    record.extend_from_slice(&host_len.to_le_bytes());
    record.extend_from_slice(&endpoint.port().to_le_bytes());
    record.extend_from_slice(protocol.as_str().as_bytes());
    record.extend_from_slice(endpoint.host().as_bytes());
    record.extend_from_slice(&pin);
    Ok(record)
}

fn decode_record(
    record: &[u8],
    protocol: &ProtocolId,
    endpoint: &Endpoint,
) -> Result<[u8; 32], PlatformError> {
    const HEADER_LEN: usize = 8 + 4 + 4 + 2;
    if record.len() < HEADER_LEN + 32 || &record[..8] != RECORD_MAGIC {
        return Err(PlatformError::StorageFailed);
    }
    let protocol_len = u32::from_le_bytes(
        record[8..12]
            .try_into()
            .map_err(|_| PlatformError::StorageFailed)?,
    ) as usize;
    let host_len = u32::from_le_bytes(
        record[12..16]
            .try_into()
            .map_err(|_| PlatformError::StorageFailed)?,
    ) as usize;
    let port = u16::from_le_bytes(
        record[16..18]
            .try_into()
            .map_err(|_| PlatformError::StorageFailed)?,
    );
    let protocol_end = HEADER_LEN
        .checked_add(protocol_len)
        .ok_or(PlatformError::StorageFailed)?;
    let host_end = protocol_end
        .checked_add(host_len)
        .ok_or(PlatformError::StorageFailed)?;
    let record_end = host_end
        .checked_add(32)
        .ok_or(PlatformError::StorageFailed)?;
    if record_end != record.len()
        || &record[HEADER_LEN..protocol_end] != protocol.as_str().as_bytes()
        || &record[protocol_end..host_end] != endpoint.host().as_bytes()
        || port != endpoint.port()
    {
        return Err(PlatformError::StorageFailed);
    }
    record[host_end..record_end]
        .try_into()
        .map_err(|_| PlatformError::StorageFailed)
}

#[cfg(windows)]
fn protect_current_user(plaintext: &[u8]) -> Result<Vec<u8>, PlatformError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input_len = u32::try_from(plaintext.len()).map_err(|_| PlatformError::StorageFailed)?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: plaintext.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY：调用期间 input 引用有效明文，output 由 DPAPI 初始化。
    let success = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 || output.pbData.is_null() {
        return Err(PlatformError::StorageFailed);
    }
    // SAFETY：DPAPI 返回了包含 output.cbData 字节的已初始化缓冲区。
    let protected =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    // SAFETY：DPAPI 规定返回缓冲区必须由 LocalFree 释放。
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(protected)
}

#[cfg(windows)]
fn unprotect_current_user(encrypted: &[u8]) -> Result<Vec<u8>, PlatformError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input_len = u32::try_from(encrypted.len()).map_err(|_| PlatformError::StorageFailed)?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: encrypted.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY：调用期间 input 引用有效密文，output 由 DPAPI 初始化。
    let success = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 || output.pbData.is_null() {
        return Err(PlatformError::StorageFailed);
    }
    // SAFETY：DPAPI 返回了包含 output.cbData 字节的已初始化缓冲区。
    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    // 解密后的 LocalAlloc 缓冲区在释放前清零。
    for index in 0..output.cbData as usize {
        // SAFETY：index 位于 DPAPI 输出分配范围内。
        unsafe { std::ptr::write_volatile(output.pbData.add(index), 0) };
    }
    // SAFETY：DPAPI 规定返回缓冲区必须由 LocalFree 释放。
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(plaintext)
}

#[cfg(not(windows))]
fn protect_current_user(_: &[u8]) -> Result<Vec<u8>, PlatformError> {
    Err(PlatformError::Unavailable)
}

#[cfg(not(windows))]
fn unprotect_current_user(_: &[u8]) -> Result<Vec<u8>, PlatformError> {
    Err(PlatformError::Unavailable)
}

#[cfg(test)]
mod tests {
    use frd_core::{Endpoint, ProtocolId};
    use frd_platform_api::{PlatformError, ServerIdentityStore};
    use tempfile::tempdir;

    use super::DpapiServerIdentityStore;

    #[cfg(windows)]
    #[test]
    fn current_user_dpapi_pin_roundtrip_keeps_the_record_protected_at_rest() {
        let directory = tempdir().expect("temporary pin directory");
        let store = DpapiServerIdentityStore::at_path(directory.path().join("pins"));
        let protocol = ProtocolId::rdp();
        let endpoint = Endpoint::new("host.invalid", 3389).expect("valid endpoint");
        let pin = [0x5a; 32];

        store
            .store_pin(&protocol, &endpoint, pin)
            .expect("current-user DPAPI protects the pin");

        assert_eq!(store.load_pin(&protocol, &endpoint), Ok(Some(pin)));
        let stored = std::fs::read(
            std::fs::read_dir(directory.path().join("pins"))
                .expect("pin directory exists")
                .next()
                .expect("one pin record")
                .expect("valid pin entry")
                .path(),
        )
        .expect("encrypted record is readable");
        assert!(!stored
            .windows(endpoint.host().len())
            .any(|window| window == endpoint.host().as_bytes()));
        assert!(!stored.windows(pin.len()).any(|window| window == pin));
    }

    #[cfg(windows)]
    #[test]
    fn a_different_fingerprint_for_the_same_endpoint_and_protocol_is_rejected() {
        let directory = tempdir().expect("temporary pin directory");
        let store = DpapiServerIdentityStore::at_path(directory.path().join("pins"));
        let protocol = ProtocolId::rdp();
        let endpoint = Endpoint::new("host.invalid", 3389).expect("valid endpoint");

        store
            .store_pin(&protocol, &endpoint, [0x11; 32])
            .expect("first pin is stored");

        assert_eq!(
            store.store_pin(&protocol, &endpoint, [0x22; 32]),
            Err(PlatformError::ServerIdentityPinMismatch)
        );
        assert_eq!(store.load_pin(&protocol, &endpoint), Ok(Some([0x11; 32])));
    }
}
