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
    for byte in bytes {
        // SAFETY: `byte` is a valid, uniquely borrowed byte in the owned allocation.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::SecretBuffer;

    #[test]
    fn transferred_secret_owner_clears_its_allocation() {
        let mut buffer = SecretBuffer::new(vec![0x11, 0x22]);
        let mut bytes = buffer.take();

        bytes.clear();

        assert_eq!(bytes.expose(), &[0, 0]);
    }
}
