use std::fmt;

pub const MAX_LOCAL_USERNAME_BYTES: usize = 255;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLocalUsername;

impl fmt::Display for InvalidLocalUsername {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("本地用户名无效")
    }
}

impl std::error::Error for InvalidLocalUsername {}

/// 验证提供给系统原生登录服务的本地账户名。
///
/// 保留合法 Unicode，不做大小写或正规化改写；线上的字节预算与 FRDSTD01 一致。
pub fn validate_local_username(value: &str) -> Result<(), InvalidLocalUsername> {
    if !(1..=MAX_LOCAL_USERNAME_BYTES).contains(&value.len())
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(InvalidLocalUsername);
    }
    Ok(())
}
