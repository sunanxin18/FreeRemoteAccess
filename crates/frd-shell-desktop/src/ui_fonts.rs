use std::path::PathBuf;

use egui::{FontData, FontDefinitions, FontFamily};

const CJK_FALLBACK_NAME: &str = "frd-cjk-fallback";

pub(crate) fn system_font_definitions() -> FontDefinitions {
    system_cjk_font_paths()
        .into_iter()
        .find_map(|path| std::fs::read(path).ok().filter(|bytes| !bytes.is_empty()))
        .map(definitions_with_cjk_fallback)
        .unwrap_or_default()
}

fn definitions_with_cjk_fallback(bytes: Vec<u8>) -> FontDefinitions {
    let mut definitions = FontDefinitions::default();
    definitions.font_data.insert(
        CJK_FALLBACK_NAME.to_owned(),
        FontData::from_owned(bytes).into(),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        definitions
            .families
            .entry(family)
            .or_default()
            .push(CJK_FALLBACK_NAME.to_owned());
    }
    definitions
}

fn system_cjk_font_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let fonts = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        return ["msyh.ttc", "msyhbd.ttc", "simhei.ttf"]
            .into_iter()
            .map(|name| fonts.join(name))
            .collect();
    }

    #[cfg(target_os = "macos")]
    {
        return vec![PathBuf::from("/System/Library/Fonts/PingFang.ttc")];
    }

    #[cfg(target_os = "linux")]
    {
        return vec![
            PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
            PathBuf::from("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"),
        ];
    }

    #[allow(unreachable_code)]
    Vec::new()
}

#[cfg(test)]
mod tests {
    use egui::FontFamily;

    use super::{definitions_with_cjk_fallback, CJK_FALLBACK_NAME};

    #[test]
    fn cjk_font_is_registered_as_both_family_fallbacks() {
        let definitions = definitions_with_cjk_fallback(vec![1, 2, 3]);

        assert!(definitions.font_data.contains_key(CJK_FALLBACK_NAME));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            assert_eq!(
                definitions
                    .families
                    .get(&family)
                    .and_then(|fonts| fonts.last()),
                Some(&CJK_FALLBACK_NAME.to_owned())
            );
        }
    }
}
