use egui::{
    Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Response, Sense, Tooltip, Ui,
    Vec2, WidgetInfo, WidgetType,
};

pub const LOGIN_ICON_BUTTON_SIZE: f32 = 44.0;
pub const LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY: &str = "frd-material-symbols-rounded";
const LOGIN_ICON_SIZE: f32 = 24.0;
const LOGIN_MATERIAL_SYMBOLS_FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/ui-icons/material-symbols-rounded-24-400.ttf"
));

pub fn install_login_icons_font(definitions: &mut FontDefinitions) {
    definitions.font_data.insert(
        LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY.to_owned(),
        FontData::from_static(LOGIN_MATERIAL_SYMBOLS_FONT_BYTES).into(),
    );
    definitions.families.insert(
        FontFamily::Name(LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY.into()),
        vec![LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY.to_owned()],
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginIcon {
    DesktopWindows,
    Dns,
    Person,
    Lock,
    Visibility,
    VisibilityOff,
    ExpandMore,
    Login,
    ShieldLock,
    Delete,
    CheckCircle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoginIconSemantic {
    pub symbol_name: &'static str,
    pub codepoint: char,
    pub tooltip: &'static str,
    pub accessible_name: &'static str,
}

impl LoginIcon {
    pub const fn semantic(self) -> LoginIconSemantic {
        match self {
            Self::DesktopWindows => LoginIconSemantic {
                symbol_name: "desktop_windows",
                codepoint: '\u{e30c}',
                tooltip: "远程桌面",
                accessible_name: "远程桌面",
            },
            Self::Dns => LoginIconSemantic {
                symbol_name: "dns",
                codepoint: '\u{e875}',
                tooltip: "远程设备地址",
                accessible_name: "远程设备地址",
            },
            Self::Person => LoginIconSemantic {
                symbol_name: "person",
                codepoint: '\u{f0d3}',
                tooltip: "登录用户名",
                accessible_name: "登录用户名",
            },
            Self::Lock => LoginIconSemantic {
                symbol_name: "lock",
                codepoint: '\u{e899}',
                tooltip: "登录密码",
                accessible_name: "登录密码",
            },
            Self::Visibility => LoginIconSemantic {
                symbol_name: "visibility",
                codepoint: '\u{e8f4}',
                tooltip: "显示密码",
                accessible_name: "显示密码",
            },
            Self::VisibilityOff => LoginIconSemantic {
                symbol_name: "visibility_off",
                codepoint: '\u{e8f5}',
                tooltip: "隐藏密码",
                accessible_name: "隐藏密码",
            },
            Self::ExpandMore => LoginIconSemantic {
                symbol_name: "expand_more",
                codepoint: '\u{e5cf}',
                tooltip: "展开选项",
                accessible_name: "展开选项",
            },
            Self::Login => LoginIconSemantic {
                symbol_name: "login",
                codepoint: '\u{ea77}',
                tooltip: "连接远程设备",
                accessible_name: "连接远程设备",
            },
            Self::ShieldLock => LoginIconSemantic {
                symbol_name: "shield_lock",
                codepoint: '\u{f686}',
                tooltip: "系统安全凭据库保护密码",
                accessible_name: "系统安全凭据库保护密码",
            },
            Self::Delete => LoginIconSemantic {
                symbol_name: "delete",
                codepoint: '\u{e92e}',
                tooltip: "删除保存的连接",
                accessible_name: "删除保存的连接",
            },
            Self::CheckCircle => LoginIconSemantic {
                symbol_name: "check_circle",
                codepoint: '\u{f0be}',
                tooltip: "已选择保存的连接",
                accessible_name: "已选择保存的连接",
            },
        }
    }
}

pub fn icon_button(ui: &mut Ui, icon: LoginIcon, selected: bool) -> Response {
    let semantic = icon.semantic();
    let (rect, response) =
        ui.allocate_exact_size(Vec2::splat(LOGIN_ICON_BUTTON_SIZE), Sense::click());
    response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, semantic.accessible_name));

    let fill = if response.hovered() || response.has_focus() || selected {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect.shrink(2.0), 8.0, fill);
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            8.0,
            ui.visuals().selection.stroke,
            egui::StrokeKind::Inside,
        );
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        semantic.codepoint,
        material_symbol_font_id(),
        ui.visuals().text_color(),
    );
    let focused = response.has_focus();
    let response = response.on_hover_text(semantic.tooltip);
    if focused {
        Tooltip::for_widget(&response).show(|ui| {
            ui.label(semantic.tooltip);
        });
    }
    response
}

pub fn show_icon(ui: &mut Ui, icon: LoginIcon, size: f32) -> Response {
    let semantic = icon.semantic();
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, semantic.accessible_name));
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        semantic.codepoint,
        FontId::new(
            size.min(LOGIN_ICON_SIZE),
            FontFamily::Name(LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY.into()),
        ),
        ui.visuals().text_color(),
    );
    response
}

fn material_symbol_font_id() -> FontId {
    FontId::new(
        LOGIN_ICON_SIZE,
        FontFamily::Name(LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY.into()),
    )
}
