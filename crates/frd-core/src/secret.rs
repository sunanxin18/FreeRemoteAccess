#[cfg(test)]
use std::cell::RefCell;

pub struct SecretBuffer(Vec<u8>);

pub struct SecretBytes(Vec<u8>);

impl SecretBuffer {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn take(&mut self) -> SecretBytes {
        SecretBytes(std::mem::take(&mut self.0))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn clear(&mut self) {
        clear_bytes(&mut self.0);
    }
}

impl Drop for SecretBuffer {
    fn drop(&mut self) {
        self.clear();
    }
}

impl SecretBytes {
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    fn clear(&mut self) {
        clear_bytes(&mut self.0);
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.clear();
    }
}

fn clear_bytes(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a valid, uniquely borrowed byte in the owned allocation.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }

    #[cfg(test)]
    observe_wipe(bytes);
}

#[cfg(test)]
std::thread_local! {
    static WIPE_OBSERVATION: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn observe_wipe(bytes: &[u8]) {
    WIPE_OBSERVATION.with(|observation| *observation.borrow_mut() = Some(bytes.to_vec()));
}

#[cfg(test)]
fn reset_wipe_observation() {
    WIPE_OBSERVATION.with(|observation| *observation.borrow_mut() = None);
}

#[cfg(test)]
fn take_wipe_observation() -> Option<Vec<u8>> {
    WIPE_OBSERVATION.with(|observation| observation.borrow_mut().take())
}

#[cfg(test)]
mod tests {
    use super::{reset_wipe_observation, take_wipe_observation, SecretBuffer};

    #[test]
    fn dropping_transferred_secret_bytes_observes_wiped_owner_memory() {
        let bytes = {
            let mut buffer = SecretBuffer::new(vec![0x11, 0x22]);
            buffer.take()
        };

        reset_wipe_observation();
        drop(bytes);

        assert_eq!(take_wipe_observation(), Some(vec![0, 0]));
    }

    #[test]
    fn dropping_secret_buffer_observes_wiped_owner_memory() {
        let buffer = SecretBuffer::new(vec![0x11, 0x22]);

        reset_wipe_observation();
        drop(buffer);

        assert_eq!(take_wipe_observation(), Some(vec![0, 0]));
    }
}
