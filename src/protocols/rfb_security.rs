#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityListFraming {
    /// RFB 3.3：服务端直接选择一个 U32 安全类型。
    ServerSelectedU32,
    /// RFB 3.7/3.8：一个 U8 数量，后接相同数量的 U8 安全类型。
    CountedU8,
}

pub const NONE: u8 = 1;
pub const VNC_AUTH: u8 = 2;
pub const APPLE_ARD: u8 = 30;
pub const APPLE_RSA_SRP: u8 = 33;
pub const APPLE_ARD_39: u8 = 35;
pub const APPLE_SRP: u8 = 36;

pub const fn security_list_framing(negotiated_minor: u16) -> SecurityListFraming {
    if negotiated_minor == 3 {
        SecurityListFraming::ServerSelectedU32
    } else {
        SecurityListFraming::CountedU8
    }
}

pub const fn is_supported_standard(value: u8) -> bool {
    matches!(value, NONE | VNC_AUTH)
}

pub const fn is_supported_apple_native(value: u8) -> bool {
    // 已知线上存在类型 35，但当前认证器没有实现它；探测器和适配器
    // 对仅提供类型 35 的服务端必须一致地关闭失败。
    matches!(value, APPLE_ARD | APPLE_RSA_SRP | APPLE_SRP)
}
