//! Apple Remote Desktop 的认证与加密会话适配器。

pub mod ard;
mod auth;
mod connection;
mod factory;
pub mod protocol;
pub mod rsa_srp;
pub mod session;
pub mod srp;

pub use auth::{
    select_apple_security_type, APPLE_CREDENTIALS_REQUIRED, APPLE_SECURITY_TYPE_UNAVAILABLE,
};
pub use connection::{is_cold_deadline_error, is_peer_closed, AppleConnection, AppleWriterHandle};
pub use factory::{AppleProtocolFactory, AppleProtocolSession};
pub use session::{SessionCrypto, SessionEncodingProfile};
