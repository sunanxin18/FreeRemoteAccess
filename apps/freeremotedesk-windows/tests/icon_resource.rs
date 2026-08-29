#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::UI::Shell::ExtractIconExW;
use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;

const RUNTIME_ICON_RGBA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/app-icon/windows/window-icon-64.rgba"
));
const EXECUTABLE_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/app-icon/windows/freeremotedesk.ico"
));

#[test]
fn runtime_and_executable_icons_share_the_same_64_pixel_artwork() {
    let icon = ico::IconDir::read(std::io::Cursor::new(EXECUTABLE_ICON)).unwrap();
    let image = icon
        .entries()
        .iter()
        .find(|entry| entry.width() == 64 && entry.height() == 64)
        .expect("ICO 必须包含 64x64 图层")
        .decode()
        .unwrap();

    assert_eq!(image.rgba_data(), RUNTIME_ICON_RGBA);
}

#[test]
fn window_icon_resource_contains_large_and_small_icons() {
    let executable = std::ffi::OsStr::new(env!("CARGO_BIN_EXE_freeremotedesk-windows"))
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let count = unsafe {
        ExtractIconExW(
            executable.as_ptr(),
            -1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert!(count >= 1, "可执行文件必须至少包含一个图标组");

    let mut large = std::ptr::null_mut();
    let mut small = std::ptr::null_mut();
    let extracted = unsafe { ExtractIconExW(executable.as_ptr(), 0, &mut large, &mut small, 1) };
    assert!(extracted >= 1);
    assert!(!large.is_null());
    assert!(!small.is_null());
    unsafe {
        DestroyIcon(large);
        DestroyIcon(small);
    }
}
