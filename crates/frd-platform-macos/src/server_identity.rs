use std::io::Write;
use std::path::{Path, PathBuf};

use frd_core::{Endpoint, ProtocolId};
use frd_platform_api::{PlatformError, ServerIdentityStore};
use sha2::{Digest, Sha256};

const RECORD_MAGIC: &[u8; 8] = b"FRDPIN01";

pub struct MacServerIdentityStore {
    root: PathBuf,
}

impl MacServerIdentityStore {
    pub fn current_user_default() -> Result<Self, PlatformError> {
        Ok(Self::at_path(
            super::paths::application_support()?.join("server-identity-pins"),
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

impl ServerIdentityStore for MacServerIdentityStore {
    fn load_pin(
        &self,
        protocol: &ProtocolId,
        endpoint: &Endpoint,
    ) -> Result<Option<[u8; 32]>, PlatformError> {
        let path = self.record_path(protocol, endpoint);
        let record = match std::fs::read(path) {
            Ok(record) => record,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(PlatformError::StorageFailed),
        };
        decode_record(&record, protocol, endpoint).map(Some)
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

        super::paths::create_private_directory(&self.root)?;
        let record = encode_record(protocol, endpoint, pin)?;
        let path = self.record_path(protocol, endpoint);
        match write_pin_record(&path, &record) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // 另一连接可能在首次读取之后抢先保存，绝不覆盖其指纹。
                match self.load_pin(protocol, endpoint)? {
                    Some(existing) if existing == pin => Ok(()),
                    Some(_) => Err(PlatformError::ServerIdentityPinMismatch),
                    None => Err(PlatformError::StorageFailed),
                }
            }
            Err(_) => Err(PlatformError::StorageFailed),
        }
    }
}

fn publish_pin_record(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    // 同目录硬链接原子发布完整记录；目标已存在时必定失败，不使用可覆盖的 rename。
    std::fs::hard_link(temporary, destination)
}

fn write_pin_record(destination: &Path, encoded: &[u8]) -> std::io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(
        destination
            .parent()
            .ok_or_else(|| std::io::Error::other("无父目录"))?,
    )?;
    temporary.write_all(encoded)?;
    temporary.as_file().sync_all()?;
    publish_pin_record(temporary.path(), destination)
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

#[cfg(test)]
mod tests {
    use frd_core::{Endpoint, ProtocolId};
    use frd_platform_api::{PlatformError, ServerIdentityStore};
    use tempfile::tempdir;

    use super::MacServerIdentityStore;

    #[test]
    fn pin_record_publication_never_replaces_an_existing_pin() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("target.pin");
        let candidate = directory.path().join("candidate.tmp");
        std::fs::write(&destination, b"previous certificate").unwrap();
        std::fs::write(&candidate, b"different certificate").unwrap();

        let result = super::publish_pin_record(&candidate, &destination);

        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"previous certificate"
        );
    }

    #[test]
    fn concurrent_pin_record_writers_publish_one_complete_winner_without_temporary_files() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("target.pin");
        let barrier = std::sync::Barrier::new(8);
        let results = std::thread::scope(|scope| {
            let handles = (0u8..8)
                .map(|value| {
                    let destination = &destination;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        (value, super::write_pin_record(destination, &[value; 4096]))
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let winners = results
            .iter()
            .filter(|(_, result)| result.is_ok())
            .collect::<Vec<_>>();
        assert_eq!(winners.len(), 1);
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            vec![winners[0].0; 4096]
        );
        for (_, result) in results.into_iter().filter(|(_, result)| result.is_err()) {
            assert_eq!(
                result.unwrap_err().kind(),
                std::io::ErrorKind::AlreadyExists
            );
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_different_fingerprint_for_the_same_endpoint_and_protocol_is_rejected() {
        let directory = tempdir().expect("temporary pin directory");
        let store = MacServerIdentityStore::at_path(directory.path().join("pins"));
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
    #[test]
    fn concurrent_first_connections_choose_one_complete_pin_and_reject_others() {
        let dir = tempdir().unwrap();
        let store = MacServerIdentityStore::at_path(dir.path());
        let protocol = ProtocolId::rdp();
        let endpoint = Endpoint::new("fixture.invalid", 3389).unwrap();
        let barrier = std::sync::Barrier::new(8);
        let results = std::thread::scope(|scope| {
            let jobs = (0u8..8)
                .map(|value| {
                    let (store, protocol, endpoint, barrier) =
                        (&store, &protocol, &endpoint, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        (value, store.store_pin(protocol, endpoint, [value; 32]))
                    })
                })
                .collect::<Vec<_>>();
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .collect::<Vec<_>>()
        });
        let winners = results
            .iter()
            .filter(|(_, r)| r.is_ok())
            .collect::<Vec<_>>();
        assert_eq!(winners.len(), 1);
        assert_eq!(
            store.load_pin(&protocol, &endpoint),
            Ok(Some([winners[0].0; 32]))
        );
        for (_, result) in results.into_iter().filter(|(_, r)| r.is_err()) {
            assert_eq!(result, Err(PlatformError::ServerIdentityPinMismatch));
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_and_wrong_endpoint_pin_records_fail_closed() {
        let dir = tempdir().unwrap();
        let store = MacServerIdentityStore::at_path(dir.path());
        let protocol = ProtocolId::rdp();
        let endpoint = Endpoint::new("fixture.invalid", 3389).unwrap();
        let other = Endpoint::new("other.invalid", 3389).unwrap();
        store.store_pin(&protocol, &endpoint, [1; 32]).unwrap();
        std::fs::copy(
            store.record_path(&protocol, &endpoint),
            store.record_path(&protocol, &other),
        )
        .unwrap();
        assert_eq!(
            store.load_pin(&protocol, &other),
            Err(PlatformError::StorageFailed)
        );
        std::fs::write(store.record_path(&protocol, &endpoint), b"broken").unwrap();
        assert_eq!(
            store.store_pin(&protocol, &endpoint, [2; 32]),
            Err(PlatformError::StorageFailed)
        );
    }
}
