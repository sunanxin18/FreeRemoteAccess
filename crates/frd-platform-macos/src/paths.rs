use frd_platform_api::PlatformError;
use std::path::{Path, PathBuf};

pub(crate) fn application_support() -> Result<PathBuf, PlatformError> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or(PlatformError::Unavailable)?;
    Ok(PathBuf::from(home).join("Library/Application Support/FreeRemoteDesk"))
}

pub(crate) fn create_private_directory(path: &Path) -> Result<(), PlatformError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .map_err(|_| PlatformError::StorageFailed)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path).map_err(|_| PlatformError::StorageFailed)?;
    Ok(())
}
