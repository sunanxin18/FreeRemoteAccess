#[cfg(any(test, feature = "secret-wipe-test-hook"))]
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

    pub fn from_text(text: String) -> Self {
        Self(text.into_bytes())
    }

    pub fn expose_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    pub fn replace_text_char_range(
        &mut self,
        char_range: std::ops::Range<usize>,
        replacement: &str,
    ) -> bool {
        let Some(text) = self.expose_text() else {
            return false;
        };
        let Some(byte_range) = char_range_to_byte_range(text, char_range) else {
            return false;
        };

        let mut replaced = Vec::with_capacity(self.0.len() - byte_range.len() + replacement.len());
        replaced.extend_from_slice(&self.0[..byte_range.start]);
        replaced.extend_from_slice(replacement.as_bytes());
        replaced.extend_from_slice(&self.0[byte_range.end..]);
        self.clear();
        self.0 = replaced;
        true
    }

    pub fn insert_text_at_char(&mut self, char_index: usize, text: &str) -> bool {
        self.replace_text_char_range(char_index..char_index, text)
    }

    pub fn delete_text_char_range(&mut self, char_range: std::ops::Range<usize>) -> bool {
        self.replace_text_char_range(char_range, "")
    }

    fn clear(&mut self) {
        clear_bytes(&mut self.0);
    }
}

fn char_range_to_byte_range(
    text: &str,
    char_range: std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    if char_range.start > char_range.end {
        return None;
    }

    let mut boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(text.len());
    let start = *boundaries.get(char_range.start)?;
    let end = *boundaries.get(char_range.end)?;
    Some(start..end)
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

    #[cfg(any(test, feature = "secret-wipe-test-hook"))]
    observe_wipe(bytes);
}

#[cfg(any(test, feature = "secret-wipe-test-hook"))]
std::thread_local! {
    static WIPE_OBSERVATION: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

#[cfg(any(test, feature = "secret-wipe-test-hook"))]
fn observe_wipe(bytes: &[u8]) {
    WIPE_OBSERVATION.with(|observation| *observation.borrow_mut() = Some(bytes.to_vec()));
}

#[cfg(any(test, feature = "secret-wipe-test-hook"))]
fn reset_wipe_observation() {
    WIPE_OBSERVATION.with(|observation| *observation.borrow_mut() = None);
}

#[cfg(any(test, feature = "secret-wipe-test-hook"))]
fn take_wipe_observation() -> Option<Vec<u8>> {
    WIPE_OBSERVATION.with(|observation| observation.borrow_mut().take())
}

#[cfg(feature = "secret-wipe-test-hook")]
#[doc(hidden)]
pub mod wipe_test_observer {
    pub fn reset_wipe_observation() {
        super::reset_wipe_observation();
    }

    pub fn take_wipe_observation() -> Option<Vec<u8>> {
        super::take_wipe_observation()
    }
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

    #[test]
    fn secret_text_edit_replaces_a_character_without_leaving_the_secret_owner() {
        let mut buffer = SecretBuffer::from_text("ab".to_owned());

        assert!(buffer.replace_text_char_range(1..2, "密"));

        assert_eq!(buffer.expose_text(), Some("a密"));
    }
}
