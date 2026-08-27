use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionId(NonZeroU64);

impl SessionId {
    pub fn allocate() -> Self {
        let value = NEXT_SESSION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .expect("会话 ID 已耗尽");

        Self(NonZeroU64::new(value).expect("会话 ID 不能为零"))
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}
