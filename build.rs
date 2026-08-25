#[cfg(target_os = "windows")]
use std::fs;
#[cfg(target_os = "windows")]
use std::path::Path;

#[cfg(target_os = "windows")]
fn write_icon(path: &Path) -> std::io::Result<()> {
    const WIDTH: usize = 32;
    const HEIGHT: usize = 32;
    let pixel_bytes = WIDTH * HEIGHT * 4;
    let mask_bytes = WIDTH * HEIGHT / 8;
    let image_bytes = 40 + pixel_bytes + mask_bytes;
    let mut icon = Vec::with_capacity(22 + image_bytes);
    icon.extend_from_slice(&0u16.to_le_bytes());
    icon.extend_from_slice(&1u16.to_le_bytes());
    icon.extend_from_slice(&1u16.to_le_bytes());
    icon.extend_from_slice(&[WIDTH as u8, HEIGHT as u8, 0, 0]);
    icon.extend_from_slice(&1u16.to_le_bytes());
    icon.extend_from_slice(&32u16.to_le_bytes());
    icon.extend_from_slice(&(image_bytes as u32).to_le_bytes());
    icon.extend_from_slice(&22u32.to_le_bytes());
    icon.extend_from_slice(&40u32.to_le_bytes());
    icon.extend_from_slice(&(WIDTH as i32).to_le_bytes());
    icon.extend_from_slice(&((HEIGHT * 2) as i32).to_le_bytes());
    icon.extend_from_slice(&1u16.to_le_bytes());
    icon.extend_from_slice(&32u16.to_le_bytes());
    icon.extend_from_slice(&0u32.to_le_bytes());
    icon.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    icon.extend_from_slice(&[0; 16]);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let white = (7..=24).contains(&x) && ((8..=12).contains(&y) || (19..=23).contains(&y));
            icon.extend_from_slice(if white {
                &[0xff, 0xff, 0xff, 0xff]
            } else {
                &[0xd7, 0x68, 0x16, 0xff]
            });
        }
    }
    icon.resize(22 + image_bytes, 0);
    fs::write(path, icon)
}

#[cfg(target_os = "windows")]
fn main() {
    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR is required");
    let icon_path = Path::new(&out_dir).join("freeremoteaccess.ico");
    write_icon(&icon_path).expect("write Windows icon");
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is required");
    let manifest = include_str!("packaging/windows/windows_app.manifest");
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon_path.to_str().expect("icon path must be Unicode"))
        .set_manifest(manifest)
        .set("ProductName", "FreeRemoteAccess")
        .set(
            "FileDescription",
            "FreeRemoteAccess native remote-login client",
        )
        .set("ProductVersion", &version)
        .set("FileVersion", &version)
        .set("LegalCopyright", "FreeRemoteAccess contributors");
    resource.compile().expect("compile Windows resources");
}

#[cfg(not(target_os = "windows"))]
fn main() {}
