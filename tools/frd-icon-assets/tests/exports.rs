use std::fs::File;
use std::path::Path;

use ico::IconDir;
use image::{GenericImageView, Rgba, RgbaImage};

const WINDOWS_ICON_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
const GENERATED_ASSETS: [&str; 17] = [
    "android/background.png",
    "android/foreground.png",
    "android/monochrome.png",
    "apple/background.png",
    "apple/foreground.png",
    "apple/monochrome.png",
    "google-play/icon-512.png",
    "linux/hicolor/16x16/apps/freeremotedesk.png",
    "linux/hicolor/32x32/apps/freeremotedesk.png",
    "linux/hicolor/48x48/apps/freeremotedesk.png",
    "linux/hicolor/64x64/apps/freeremotedesk.png",
    "linux/hicolor/128x128/apps/freeremotedesk.png",
    "linux/hicolor/256x256/apps/freeremotedesk.png",
    "linux/hicolor/512x512/apps/freeremotedesk.png",
    "source/app-icon-master.png",
    "windows/freeremotedesk.ico",
    "windows/window-icon-64.rgba",
];

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
fn exports_platform_dimensions_alpha_safe_zone_and_ico_entries() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("foreground.png");
    let output = temporary.path().join("out");
    fixture(&source);

    frd_icon_assets::export_assets(&source, &output).unwrap();

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

    let android = image::open(output.join("android/foreground.png"))
        .unwrap()
        .into_rgba8();
    assert_eq!(android.dimensions(), (432, 432));
    let visible = android
        .enumerate_pixels()
        .filter(|(_, _, pixel)| pixel[3] > 4)
        .map(|(x, y, _)| (x, y))
        .collect::<Vec<_>>();
    let safe_start = (432 - (432 * 66 / 108)) / 2;
    let safe_end = safe_start + (432 * 66 / 108) - 1;
    assert!(visible
        .iter()
        .all(|(x, y)| *x >= safe_start && *x <= safe_end && *y >= safe_start && *y <= safe_end));

    let icon =
        IconDir::read(File::open(output.join("windows/freeremotedesk.ico")).unwrap()).unwrap();
    assert_eq!(
        icon.entries()
            .iter()
            .map(|entry| entry.width())
            .collect::<Vec<_>>(),
        WINDOWS_ICON_SIZES
    );
    assert_eq!(
        std::fs::metadata(output.join("windows/window-icon-64.rgba"))
            .unwrap()
            .len(),
        64 * 64 * 4
    );

    let tiny = image::open(output.join("linux/hicolor/16x16/apps/freeremotedesk.png"))
        .unwrap()
        .into_rgba8();
    assert!(tiny.pixels().any(|pixel| pixel.0 != [6, 27, 69, 255]));
}

#[test]
fn committed_derivatives_match_the_deterministic_export() {
    let repository_assets = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/app-icon")
        .canonicalize()
        .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let generated = temporary.path().join("generated");

    frd_icon_assets::export_assets(
        &repository_assets.join("source/portal-foreground.png"),
        &generated,
    )
    .unwrap();

    for relative in GENERATED_ASSETS {
        assert_eq!(
            std::fs::read(generated.join(relative)).unwrap(),
            std::fs::read(repository_assets.join(relative)).unwrap(),
            "已提交资产必须由 frd-icon-assets 确定性生成：{relative}"
        );
    }
}
