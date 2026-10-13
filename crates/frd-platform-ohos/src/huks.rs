use crate::secure_credentials::CredentialBackend;
use frd_core::SecretBuffer;
use frd_platform_api::PlatformError;
use std::path::PathBuf;
const MAGIC: &[u8; 8] = b"FRDHUK01";
const MAX: usize = 65_536;
pub(crate) struct HuksBackend {
    root: PathBuf,
}
impl HuksBackend {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }
}
#[cfg(target_env = "ohos")]
extern "C" {
    fn frd_huks_random(nonce: *mut u8) -> i32;
    fn frd_huks_crypt(
        decrypt: u32,
        nonce: *const u8,
        aad: *const u8,
        aad_len: u32,
        input: *const u8,
        input_len: u32,
        output: *mut u8,
        output_len: *mut u32,
    ) -> i32;
}
fn aad(account: &str) -> Vec<u8> {
    [MAGIC.as_slice(), account.as_bytes()].concat()
}
fn crypt(decrypt: bool, nonce: &[u8], aad: &[u8], input: &[u8]) -> Result<Vec<u8>, PlatformError> {
    #[cfg(target_env = "ohos")]
    {
        let mut out = vec![0u8; input.len() + 16];
        let mut len = out.len() as u32;
        let code = unsafe {
            frd_huks_crypt(
                u32::from(decrypt),
                nonce.as_ptr(),
                aad.as_ptr(),
                aad.len() as u32,
                input.as_ptr(),
                input.len() as u32,
                out.as_mut_ptr(),
                &mut len,
            )
        };
        let expected = if decrypt {
            input.len().checked_sub(16)
        } else {
            input.len().checked_add(16)
        };
        if code != 0 || expected != Some(len as usize) || len as usize > out.len() {
            for byte in &mut out {
                unsafe { std::ptr::write_volatile(byte, 0) }
            }
            return Err(PlatformError::CredentialProviderFailed);
        }
        out.truncate(len as usize);
        Ok(out)
    }
    #[cfg(not(target_env = "ohos"))]
    {
        let _ = (decrypt, nonce, aad, input);
        Err(PlatformError::Unavailable)
    }
}
impl CredentialBackend for HuksBackend {
    fn read(&self, account: &str) -> Result<Option<SecretBuffer>, PlatformError> {
        let Some(record) = crate::paths::read(&self.root.join(format!("{account}.huks")))? else {
            return Ok(None);
        };
        if record.len() < 8 + 12 + 16 || record.len() > 8 + 12 + MAX + 16 || &record[..8] != MAGIC {
            return Err(PlatformError::StorageFailed);
        }
        let secret = SecretBuffer::new(crypt(true, &record[8..20], &aad(account), &record[20..])?);
        if secret.expose_text().is_none() {
            return Err(PlatformError::CredentialProviderFailed);
        }
        Ok(Some(secret))
    }
    fn write(&self, account: &str, value: &SecretBuffer) -> Result<(), PlatformError> {
        let text = value
            .expose_text()
            .ok_or(PlatformError::CredentialProviderFailed)?;
        if text.len() > MAX {
            return Err(PlatformError::CredentialTooLarge);
        }
        #[cfg(target_env = "ohos")]
        {
            let mut nonce = [0u8; 12];
            if unsafe { frd_huks_random(nonce.as_mut_ptr()) } != 0 {
                return Err(PlatformError::Unavailable);
            }
            let encrypted = crypt(false, &nonce, &aad(account), text.as_bytes())?;
            let record = [MAGIC.as_slice(), &nonce, encrypted.as_slice()].concat();
            crate::paths::write_atomic(&self.root.join(format!("{account}.huks")), &record)
        }
        #[cfg(not(target_env = "ohos"))]
        {
            let _ = account;
            Err(PlatformError::Unavailable)
        }
    }
    fn delete(&self, account: &str) -> Result<(), PlatformError> {
        crate::paths::remove(&self.root.join(format!("{account}.huks")))
    }
}
