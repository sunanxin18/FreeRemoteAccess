//! Windows RDP protocol adapter boundary.

mod config;
mod factory;
mod upstream;

pub use config::{ParsedUsername, RdpConnectionConfig};
pub use factory::{RdpProtocolFactory, RdpProtocolSession};
