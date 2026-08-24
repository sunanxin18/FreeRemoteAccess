use std::path::PathBuf;
use std::sync::Arc;

pub fn install_cjk_fallback(context: &egui::Context) -> bool {
    let Some(bytes) = font_candidates()
        .into_iter()
        .find_map(|path| std::fs::read(path).ok())
    else {
        return false;
    };
    let mut definitions = egui::FontDefinitions::default();
    let name = "FreeRemoteAccess CJK".to_owned();
    definitions
        .font_data
        .insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        definitions
            .families
            .entry(family)
            .or_default()
            .push(name.clone());
    }
    context.set_fonts(definitions);
    true
}

fn font_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let windows = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        return ["msyh.ttc", "msyhl.ttc", "simhei.ttf"]
            .into_iter()
            .map(|name| windows.join("Fonts").join(name))
            .collect();
    }
    #[cfg(target_os = "macos")]
    {
        return [
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    }
    #[cfg(target_os = "linux")]
    {
        return [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    }
    #[allow(unreachable_code)]
    Vec::new()
}
