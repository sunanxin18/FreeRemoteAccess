#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn show_startup_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let title: Vec<u16> = "FreeRemoteAccess 启动失败\0".encode_utf16().collect();
    let text: Vec<u16> = format!("{message}\0").encode_utf16().collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn show_startup_error(message: &str) {
    eprintln!("错误: {message}");
}

fn main() {
    if let Err(error) = freeremotedesk::ui::run_desktop() {
        show_startup_error(&error.to_string());
        std::process::exit(1);
    }
}
