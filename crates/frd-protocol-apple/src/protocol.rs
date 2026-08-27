//! Apple 专属的 RFB 安全类型与会话常量。

pub const RFB_SECURITY_RESULT_OK: u32 = 0;
pub const RAW: i32 = 0;
pub const MEDIA_STREAM_CONTROL_ENCODING: i32 = 0x03f2;

pub mod apple_session {
    pub const SHARED_CLIENT_INIT: u8 = 0x01;
    pub const ENCRYPTED_SESSION_CLIENT_INIT: u8 = 0xc1;
    pub const SERVER_KEEPALIVE_MESSAGE_TYPE: u8 = 0x14;
    pub const SERVER_KEEPALIVE_MESSAGE_LEN: usize = 8;
    pub const SERVER_KEEPALIVE_TYPE_FIELD_LEN: usize = size_of::<u8>();
}

pub mod security {
    pub const APPLE_ARD: u8 = 30;
    pub const APPLE_RSA_SRP: u8 = 33;
    pub const APPLE_ARD_39: u8 = 35;
    pub const APPLE_SRP: u8 = 36;

    pub const fn requires_apple_account_credentials(value: u8) -> bool {
        matches!(value, APPLE_ARD | APPLE_RSA_SRP | APPLE_ARD_39 | APPLE_SRP)
    }
}
