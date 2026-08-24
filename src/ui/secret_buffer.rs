use std::fmt;
use std::ops::Deref;

use zeroize::{Zeroize, Zeroizing};

#[derive(Default)]
pub struct SecretBuffer {
    value: Zeroizing<String>,
}

impl SecretBuffer {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: Zeroizing::new(value.into()),
        }
    }

    pub fn push_str(&mut self, value: &str) {
        self.value.push_str(value);
    }

    pub fn clear(&mut self) {
        self.value.zeroize();
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn expose(&self) -> &str {
        self.value.as_str()
    }

    #[cfg(feature = "gui")]
    pub(crate) fn edit_string(&mut self) -> &mut String {
        &mut self.value
    }
}

impl Deref for SecretBuffer {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.expose()
    }
}

impl fmt::Debug for SecretBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBuffer([REDACTED])")
    }
}
