use std::path::PathBuf;

use egui::{FontData, FontDefinitions, FontFamily};

pub(crate) const NOTO_SANS_SC_FAMILY: &str = "frd-noto-sans-sc";
const CURRENT_UI_LOCALE: &str = "zh-Hans";
const NOTO_SANS_SC_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/noto-sans-sc/NotoSansSC-VariableFont_wght.ttf"
));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UiLanguage {
    SimplifiedChinese,
    TraditionalChinese,
    Japanese,
    Korean,
    Other,
}

impl UiLanguage {
    pub(crate) fn from_bcp47(locale: &str) -> Self {
        let normalized = locale.trim().replace('_', "-").to_ascii_lowercase();
        if normalized == "zh-hant"
            || normalized.starts_with("zh-hant-")
            || normalized.starts_with("zh-tw")
            || normalized.starts_with("zh-hk")
            || normalized.starts_with("zh-mo")
        {
            Self::TraditionalChinese
        } else if normalized == "zh"
            || normalized == "zh-hans"
            || normalized.starts_with("zh-hans-")
            || normalized.starts_with("zh-cn")
            || normalized.starts_with("zh-sg")
        {
            Self::SimplifiedChinese
        } else if normalized == "ja" || normalized.starts_with("ja-") {
            Self::Japanese
        } else if normalized == "ko" || normalized.starts_with("ko-") {
            Self::Korean
        } else {
            Self::Other
        }
    }
}

struct LoadedFont {
    name: &'static str,
    bytes: Vec<u8>,
    face_index: u32,
}

impl LoadedFont {
    #[cfg(test)]
    fn new(name: &'static str, bytes: Vec<u8>) -> Self {
        Self {
            name,
            bytes,
            face_index: 0,
        }
    }
}

struct FontFacePath {
    path: PathBuf,
    face_index: u32,
}

struct FontCandidateGroup {
    name: &'static str,
    alternatives: Vec<FontFacePath>,
}

pub(crate) fn system_font_definitions() -> FontDefinitions {
    let language = UiLanguage::from_bcp47(CURRENT_UI_LOCALE);
    let platform_fonts = load_platform_fonts(language);
    let mut definitions = definitions_with_font_stack(platform_fonts, NOTO_SANS_SC_BYTES.to_vec());
    frd_ui_egui::install_login_icons_font(&mut definitions);
    definitions
}

fn definitions_with_font_stack(
    platform_fonts: Vec<LoadedFont>,
    noto_bytes: Vec<u8>,
) -> FontDefinitions {
    let mut definitions = FontDefinitions::default();
    let platform_names = platform_fonts
        .iter()
        .map(|font| font.name.to_owned())
        .collect::<Vec<_>>();
    for font in platform_fonts {
        let mut data = FontData::from_owned(font.bytes);
        data.index = font.face_index;
        definitions
            .font_data
            .insert(font.name.to_owned(), data.into());
    }
    let mut noto = FontData::from_owned(noto_bytes);
    noto.tweak.coords.push(b"wght", 400.0);
    definitions
        .font_data
        .insert(NOTO_SANS_SC_FAMILY.to_owned(), noto.into());

    let proportional = definitions
        .families
        .entry(FontFamily::Proportional)
        .or_default();
    if platform_names.is_empty() {
        proportional.insert(0, NOTO_SANS_SC_FAMILY.to_owned());
    } else {
        proportional.splice(0..0, platform_names);
        proportional.push(NOTO_SANS_SC_FAMILY.to_owned());
    }
    definitions
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push(NOTO_SANS_SC_FAMILY.to_owned());
    definitions
}

fn load_platform_fonts(language: UiLanguage) -> Vec<LoadedFont> {
    platform_font_candidate_groups(language)
        .into_iter()
        .filter_map(|group| {
            group.alternatives.into_iter().find_map(|alternative| {
                std::fs::read(&alternative.path)
                    .ok()
                    .filter(|bytes| !bytes.is_empty())
                    .map(|bytes| LoadedFont {
                        name: group.name,
                        bytes,
                        face_index: alternative.face_index,
                    })
            })
        })
        .collect()
}

fn face(path: impl Into<PathBuf>) -> FontFacePath {
    FontFacePath {
        path: path.into(),
        face_index: 0,
    }
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
fn indexed_face(path: impl Into<PathBuf>, face_index: u32) -> FontFacePath {
    FontFacePath {
        path: path.into(),
        face_index,
    }
}

fn group(name: &'static str, alternatives: Vec<FontFacePath>) -> FontCandidateGroup {
    FontCandidateGroup { name, alternatives }
}

fn platform_font_candidate_groups(language: UiLanguage) -> Vec<FontCandidateGroup> {
    #[cfg(target_os = "windows")]
    {
        let fonts = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        let mut candidates = vec![group(
            "frd-platform-ui",
            vec![face(fonts.join("segoeui.ttf"))],
        )];
        let cjk = match language {
            UiLanguage::SimplifiedChinese => Some(group(
                "frd-platform-cjk-sc",
                vec![
                    face(fonts.join("msyh.ttc")),
                    face(fonts.join("msyhbd.ttc")),
                    face(fonts.join("simhei.ttf")),
                ],
            )),
            UiLanguage::TraditionalChinese => Some(group(
                "frd-platform-cjk-tc",
                vec![face(fonts.join("msjh.ttc"))],
            )),
            UiLanguage::Japanese => Some(group(
                "frd-platform-cjk-jp",
                vec![
                    face(fonts.join("YuGothM.ttc")),
                    face(fonts.join("YuGothR.ttc")),
                ],
            )),
            UiLanguage::Korean => Some(group(
                "frd-platform-cjk-kr",
                vec![face(fonts.join("malgun.ttf"))],
            )),
            UiLanguage::Other => None,
        };
        candidates.extend(cjk);
        return candidates;
    }

    #[cfg(target_os = "macos")]
    {
        let mut candidates = vec![group(
            "frd-platform-ui",
            vec![
                face("/System/Library/Fonts/SFNS.ttf"),
                face("/System/Library/Fonts/SFNSDisplay.ttf"),
            ],
        )];
        let cjk = match language {
            UiLanguage::SimplifiedChinese | UiLanguage::TraditionalChinese => Some(group(
                "frd-platform-cjk-zh",
                vec![face("/System/Library/Fonts/PingFang.ttc")],
            )),
            UiLanguage::Japanese => Some(group(
                "frd-platform-cjk-jp",
                vec![face("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc")],
            )),
            UiLanguage::Korean => Some(group(
                "frd-platform-cjk-kr",
                vec![face("/System/Library/Fonts/AppleSDGothicNeo.ttc")],
            )),
            UiLanguage::Other => None,
        };
        candidates.extend(cjk);
        return candidates;
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        let mut candidates = vec![group(
            "frd-platform-ui",
            vec![
                face("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf"),
                face("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
                face("/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf"),
            ],
        )];
        let cjk_index = match language {
            UiLanguage::Japanese => Some(("frd-platform-cjk-jp", 0)),
            UiLanguage::Korean => Some(("frd-platform-cjk-kr", 1)),
            UiLanguage::SimplifiedChinese => Some(("frd-platform-cjk-sc", 2)),
            UiLanguage::TraditionalChinese => Some(("frd-platform-cjk-tc", 3)),
            UiLanguage::Other => None,
        };
        if let Some((name, index)) = cjk_index {
            candidates.push(group(
                name,
                vec![
                    indexed_face(
                        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                        index,
                    ),
                    indexed_face("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", index),
                ],
            ));
        }
        return candidates;
    }

    #[cfg(target_os = "android")]
    {
        let mut candidates = vec![group(
            "frd-platform-ui",
            vec![face("/system/fonts/Roboto-Regular.ttf")],
        )];
        let cjk = match language {
            UiLanguage::SimplifiedChinese => "NotoSansCJKsc-Regular.otf",
            UiLanguage::TraditionalChinese => "NotoSansCJKtc-Regular.otf",
            UiLanguage::Japanese => "NotoSansCJKjp-Regular.otf",
            UiLanguage::Korean => "NotoSansCJKkr-Regular.otf",
            UiLanguage::Other => return candidates,
        };
        candidates.push(group(
            "frd-platform-cjk",
            vec![face(PathBuf::from("/system/fonts").join(cjk))],
        ));
        return candidates;
    }

    #[cfg(target_env = "ohos")]
    {
        let _ = language;
        return vec![group(
            "frd-platform-ui",
            vec![face("/system/fonts/HarmonyOS_Sans.ttf")],
        )];
    }

    #[allow(unreachable_code)]
    Vec::new()
}

#[cfg(test)]
mod tests {
    use egui::{FontDefinitions, FontFamily};
    use frd_ui_egui::MATERIAL_SYMBOLS_FONT_FAMILY;

    use super::{
        definitions_with_font_stack, system_font_definitions, LoadedFont, UiLanguage,
        NOTO_SANS_SC_FAMILY,
    };

    #[test]
    fn platform_ui_and_cjk_fonts_precede_defaults_while_noto_is_last_fallback() {
        let default_primary = FontDefinitions::default()
            .families
            .get(&FontFamily::Proportional)
            .unwrap()[0]
            .clone();
        let definitions = definitions_with_font_stack(
            vec![
                LoadedFont::new("platform-ui", vec![1]),
                LoadedFont::new("platform-cjk", vec![2]),
            ],
            vec![3],
        );
        let proportional = definitions.families.get(&FontFamily::Proportional).unwrap();

        assert_eq!(proportional[0], "platform-ui");
        assert_eq!(proportional[1], "platform-cjk");
        assert!(
            proportional
                .iter()
                .position(|name| name == &default_primary)
                .unwrap()
                > 1
        );
        assert_eq!(proportional.last().unwrap(), NOTO_SANS_SC_FAMILY);
        assert_eq!(
            definitions
                .families
                .get(&FontFamily::Monospace)
                .and_then(|fonts| fonts.last())
                .map(String::as_str),
            Some(NOTO_SANS_SC_FAMILY)
        );
    }

    #[test]
    fn noto_becomes_primary_when_no_platform_font_is_available() {
        let definitions = definitions_with_font_stack(Vec::new(), vec![3]);

        assert_eq!(
            definitions
                .families
                .get(&FontFamily::Proportional)
                .and_then(|fonts| fonts.first())
                .map(String::as_str),
            Some(NOTO_SANS_SC_FAMILY)
        );
        let noto = definitions.font_data.get(NOTO_SANS_SC_FAMILY).unwrap();
        assert_eq!(noto.tweak.coords.as_ref()[0].1, 400.0);
    }

    #[test]
    fn bcp47_locale_selects_the_matching_cjk_glyph_convention() {
        assert_eq!(
            UiLanguage::from_bcp47("zh-CN"),
            UiLanguage::SimplifiedChinese
        );
        assert_eq!(
            UiLanguage::from_bcp47("zh-Hant"),
            UiLanguage::TraditionalChinese
        );
        assert_eq!(UiLanguage::from_bcp47("ja-JP"), UiLanguage::Japanese);
        assert_eq!(UiLanguage::from_bcp47("ko-KR"), UiLanguage::Korean);
        assert_eq!(UiLanguage::from_bcp47("en-US"), UiLanguage::Other);
    }

    #[test]
    fn system_definitions_include_the_material_symbols_named_family() {
        let definitions = system_font_definitions();

        assert!(definitions
            .font_data
            .contains_key(MATERIAL_SYMBOLS_FONT_FAMILY));
        assert_eq!(
            definitions
                .families
                .get(&FontFamily::Name(MATERIAL_SYMBOLS_FONT_FAMILY.into()))
                .cloned(),
            Some(vec![MATERIAL_SYMBOLS_FONT_FAMILY.to_owned()])
        );
    }
}
