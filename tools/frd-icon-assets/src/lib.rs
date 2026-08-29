use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use ico::{IconDir, IconDirEntry, IconImage, ResourceType};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat, Rgba, RgbaImage};

const NAVY: Rgba<u8> = Rgba([6, 27, 69, 255]);
const MASTER_SIZE: u32 = 1024;
const ANDROID_SIZE: u32 = 432;
const MASTER_FOREGROUND_EXTENT: f32 = 0.82;
const ANDROID_FOREGROUND_EXTENT: f32 = 0.56;
const WINDOWS_ICON_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
const LINUX_ICON_SIZES: [u32; 7] = [16, 32, 48, 64, 128, 256, 512];

pub fn extract_black_matte(source: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = image::open(source)?.into_rgba8();
    let foreground = RgbaImage::from_fn(source.width(), source.height(), |x, y| {
        let pixel = source.get_pixel(x, y);
        let matte_value = pixel[0].max(pixel[1]).max(pixel[2]);
        if matte_value <= 8 {
            return Rgba([0, 0, 0, 0]);
        }
        let alpha = ((u16::from(matte_value - 8) * 255) / 247) as u8;
        let unpremultiply =
            |channel: u8| ((u16::from(channel) * 255) / u16::from(matte_value)).min(255) as u8;
        Rgba([
            unpremultiply(pixel[0]),
            unpremultiply(pixel[1]),
            unpremultiply(pixel[2]),
            alpha,
        ])
    });
    save_png(&foreground, output)
}

pub fn export_assets(source: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = image::open(source)?.into_rgba8();
    let apple_foreground = normalized_layer(&source, MASTER_SIZE, MASTER_FOREGROUND_EXTENT)?;
    let apple_background = solid_background(MASTER_SIZE);
    let master = composite(&apple_background, &apple_foreground);
    let apple_monochrome = monochrome_layer(&apple_foreground);

    save_png(&apple_background, &output.join("apple/background.png"))?;
    save_png(&apple_foreground, &output.join("apple/foreground.png"))?;
    save_png(&apple_monochrome, &output.join("apple/monochrome.png"))?;
    save_png(&master, &output.join("source/app-icon-master.png"))?;

    let android_foreground = normalized_layer(&source, ANDROID_SIZE, ANDROID_FOREGROUND_EXTENT)?;
    let android_background = solid_background(ANDROID_SIZE);
    save_png(&android_background, &output.join("android/background.png"))?;
    save_png(&android_foreground, &output.join("android/foreground.png"))?;
    save_png(
        &monochrome_layer(&android_foreground),
        &output.join("android/monochrome.png"),
    )?;

    let play = image::imageops::resize(&master, 512, 512, FilterType::Lanczos3);
    save_png(&play, &output.join("google-play/icon-512.png"))?;

    export_windows(&master, output)?;
    export_linux(&master, output)?;
    Ok(())
}

fn normalized_layer(
    source: &RgbaImage,
    canvas: u32,
    extent: f32,
) -> Result<RgbaImage, Box<dyn std::error::Error>> {
    let (left, top, right, bottom) = alpha_bounds(source).ok_or("透明前景没有可见像素")?;
    let crop = source
        .view(left, top, right - left + 1, bottom - top + 1)
        .to_image();
    let maximum = (canvas as f32 * extent).floor().max(1.0) as u32;
    let scale = (maximum as f64 / crop.width() as f64).min(maximum as f64 / crop.height() as f64);
    let width = (crop.width() as f64 * scale).round().max(1.0) as u32;
    let height = (crop.height() as f64 * scale).round().max(1.0) as u32;
    let resized = image::imageops::resize(&crop, width, height, FilterType::Lanczos3);
    let mut layer = RgbaImage::new(canvas, canvas);
    image::imageops::overlay(
        &mut layer,
        &resized,
        i64::from((canvas - width) / 2),
        i64::from((canvas - height) / 2),
    );
    Ok(layer)
}

fn alpha_bounds(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let mut left = image.width();
    let mut top = image.height();
    let mut right = 0;
    let mut bottom = 0;
    let mut found = false;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] > 4 {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
            found = true;
        }
    }
    found.then_some((left, top, right, bottom))
}

fn solid_background(size: u32) -> RgbaImage {
    RgbaImage::from_pixel(size, size, NAVY)
}

fn composite(background: &RgbaImage, foreground: &RgbaImage) -> RgbaImage {
    let mut result = background.clone();
    image::imageops::overlay(&mut result, foreground, 0, 0);
    result
}

fn monochrome_layer(source: &RgbaImage) -> RgbaImage {
    RgbaImage::from_fn(source.width(), source.height(), |x, y| {
        Rgba([255, 255, 255, source.get_pixel(x, y)[3]])
    })
}

fn export_windows(master: &RgbaImage, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let directory = output.join("windows");
    fs::create_dir_all(&directory)?;
    let mut icon = IconDir::new(ResourceType::Icon);
    for size in WINDOWS_ICON_SIZES {
        let resized = image::imageops::resize(master, size, size, FilterType::Lanczos3);
        let image = IconImage::from_rgba_data(size, size, resized.into_raw());
        icon.add_entry(IconDirEntry::encode(&image)?);
    }
    icon.write(BufWriter::new(File::create(
        directory.join("freeremotedesk.ico"),
    )?))?;

    let runtime = image::imageops::resize(master, 64, 64, FilterType::Lanczos3);
    fs::write(directory.join("window-icon-64.rgba"), runtime.into_raw())?;
    Ok(())
}

fn export_linux(master: &RgbaImage, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    for size in LINUX_ICON_SIZES {
        let resized = image::imageops::resize(master, size, size, FilterType::Lanczos3);
        save_png(
            &resized,
            &output.join(format!(
                "linux/hicolor/{size}x{size}/apps/freeremotedesk.png"
            )),
        )?;
    }
    Ok(())
}

fn save_png(image: &RgbaImage, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    DynamicImage::ImageRgba8(image.clone()).save_with_format(path, ImageFormat::Png)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use ico::IconDir;
    use image::{GenericImageView, Rgba, RgbaImage};

    use super::{
        alpha_bounds, export_assets, extract_black_matte, ANDROID_SIZE, WINDOWS_ICON_SIZES,
    };

    fn fixture(path: &std::path::Path) {
        let mut image = RgbaImage::new(256, 256);
        for y in 70..186 {
            for x in 24..232 {
                image.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        image.save(path).unwrap();
    }

    #[test]
    fn black_matte_extraction_produces_real_alpha_and_unpremultiplied_color() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("matte.png");
        let output = temporary.path().join("transparent.png");
        let image = RgbaImage::from_raw(
            3,
            1,
            vec![0, 0, 0, 255, 128, 128, 128, 255, 0, 64, 128, 255],
        )
        .unwrap();
        image.save(&source).unwrap();

        extract_black_matte(&source, &output).unwrap();

        let transparent = image::open(output).unwrap().into_rgba8();
        assert_eq!(transparent.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(&transparent.get_pixel(1, 0).0[..3], &[255, 255, 255]);
        assert!(transparent.get_pixel(1, 0)[3] > 100);
        assert_eq!(&transparent.get_pixel(2, 0).0[..3], &[0, 127, 255]);
    }

    #[test]
    fn exports_have_literal_dimensions_alpha_and_ico_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("foreground.png");
        let output = temporary.path().join("out");
        fixture(&source);

        export_assets(&source, &output).unwrap();

        for relative in [
            "apple/background.png",
            "apple/foreground.png",
            "apple/monochrome.png",
            "source/app-icon-master.png",
        ] {
            assert_eq!(
                image::open(output.join(relative)).unwrap().dimensions(),
                (1024, 1024)
            );
        }
        assert_eq!(
            image::open(output.join("google-play/icon-512.png"))
                .unwrap()
                .dimensions(),
            (512, 512)
        );
        let foreground = image::open(output.join("apple/foreground.png"))
            .unwrap()
            .into_rgba8();
        assert_eq!(foreground.get_pixel(0, 0)[3], 0);
        assert!(foreground.pixels().any(|pixel| pixel[3] != 0));

        let icon =
            IconDir::read(File::open(output.join("windows/freeremotedesk.ico")).unwrap()).unwrap();
        let sizes = icon
            .entries()
            .iter()
            .map(|entry| entry.width())
            .collect::<Vec<_>>();
        assert_eq!(sizes, WINDOWS_ICON_SIZES);
        assert_eq!(
            std::fs::metadata(output.join("windows/window-icon-64.rgba"))
                .unwrap()
                .len(),
            64 * 64 * 4
        );
    }

    #[test]
    fn android_essential_alpha_stays_inside_the_centered_safe_zone() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("foreground.png");
        let output = temporary.path().join("out");
        fixture(&source);
        export_assets(&source, &output).unwrap();

        let foreground = image::open(output.join("android/foreground.png"))
            .unwrap()
            .into_rgba8();
        assert_eq!(foreground.dimensions(), (ANDROID_SIZE, ANDROID_SIZE));
        let (left, top, right, bottom) = alpha_bounds(&foreground).unwrap();
        let safe_extent = ANDROID_SIZE * 66 / 108;
        let safe_start = (ANDROID_SIZE - safe_extent) / 2;
        let safe_end = safe_start + safe_extent - 1;
        assert!(left >= safe_start && top >= safe_start);
        assert!(right <= safe_end && bottom <= safe_end);
    }

    #[test]
    fn sixteen_pixel_linux_export_retains_foreground_contrast() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("foreground.png");
        let output = temporary.path().join("out");
        fixture(&source);
        export_assets(&source, &output).unwrap();

        let icon = image::open(output.join("linux/hicolor/16x16/apps/freeremotedesk.png"))
            .unwrap()
            .into_rgba8();
        assert!(icon.pixels().any(|pixel| pixel.0 != [6, 27, 69, 255]));
    }
}
