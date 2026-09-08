use frd_core::{Endpoint, ProtocolId};
use frd_platform_api::{PlatformError, ServerIdentityStore};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
const RECORD_MAGIC: &[u8; 8] = b"FRDPIN01";
pub struct LinuxServerIdentityStore {
    root: PathBuf,
}
impl LinuxServerIdentityStore {
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
        for field in [protocol.as_str(), endpoint.host()] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field.as_bytes());
        }
        hash.update(endpoint.port().to_le_bytes());
        let name: String = hash.finalize().iter().map(|v| format!("{v:02x}")).collect();
        self.root.join(format!("{name}.pin"))
    }
    fn lock(&self) -> Result<super::single_instance::LinuxSingleInstanceGuard, PlatformError> {
        super::single_instance::LinuxSingleInstanceGuard::lock_at(
            &self.root.join("pins.lock"),
            false,
        )
        .map_err(|_| PlatformError::StorageFailed)
    }
    fn read_pin(
        &self,
        protocol: &ProtocolId,
        endpoint: &Endpoint,
    ) -> Result<Option<[u8; 32]>, PlatformError> {
        super::paths::read(&self.record_path(protocol, endpoint))?
            .map(|record| decode_record(&record, protocol, endpoint))
            .transpose()
    }
}
impl ServerIdentityStore for LinuxServerIdentityStore {
    fn load_pin(
        &self,
        protocol: &ProtocolId,
        endpoint: &Endpoint,
    ) -> Result<Option<[u8; 32]>, PlatformError> {
        let _lock = self.lock()?;
        self.read_pin(protocol, endpoint)
    }
    fn store_pin(
        &self,
        protocol: &ProtocolId,
        endpoint: &Endpoint,
        pin: [u8; 32],
    ) -> Result<(), PlatformError> {
        let _lock = self.lock()?;
        if let Some(existing) = self.read_pin(protocol, endpoint)? {
            return if existing == pin {
                Ok(())
            } else {
                Err(PlatformError::ServerIdentityPinMismatch)
            };
        }
        super::paths::write_atomic(
            &self.record_path(protocol, endpoint),
            &encode_record(protocol, endpoint, pin)?,
        )
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn concurrent_tofu_publishes_one_pin_and_never_overwrites() {
        let temp = crate::private_tempdir();
        let store = LinuxServerIdentityStore::at_path(temp.path().canonicalize().unwrap());
        let endpoint = Endpoint::new("fixture.invalid", 3389).unwrap();
        let protocol = ProtocolId::rdp();
        let results = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..8u8)
                .map(|value| {
                    let (store, endpoint, protocol) = (&store, &endpoint, &protocol);
                    scope.spawn(move || (value, store.store_pin(protocol, endpoint, [value; 32])))
                })
                .collect();
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .collect::<Vec<_>>()
        });
        let winners: Vec<_> = results.iter().filter(|(_, r)| r.is_ok()).collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(
            store.load_pin(&protocol, &endpoint).unwrap(),
            Some([winners[0].0; 32])
        );
        for (_, result) in results.iter().filter(|(_, r)| r.is_err()) {
            assert_eq!(*result, Err(PlatformError::ServerIdentityPinMismatch));
        }
    }
    #[test]
    fn corrupt_and_wrong_endpoint_records_fail_closed() {
        let temp = crate::private_tempdir();
        let store = LinuxServerIdentityStore::at_path(temp.path().canonicalize().unwrap());
        let endpoint = Endpoint::new("fixture.invalid", 3389).unwrap();
        let other = Endpoint::new("other.invalid", 3389).unwrap();
        let protocol = ProtocolId::rdp();
        store.store_pin(&protocol, &endpoint, [1; 32]).unwrap();
        super::super::paths::write_atomic(
            &store.record_path(&protocol, &other),
            &encode_record(&protocol, &endpoint, [1; 32]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            store.load_pin(&protocol, &other),
            Err(PlatformError::StorageFailed)
        );
        super::super::paths::write_atomic(&store.record_path(&protocol, &endpoint), b"broken")
            .unwrap();
        assert_eq!(
            store.store_pin(&protocol, &endpoint, [2; 32]),
            Err(PlatformError::StorageFailed)
        );
    }
}
