use egui::{
    Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Response, Sense, Ui, Vec2,
    WidgetInfo, WidgetType,
};
use frd_ui_model::{
    CapabilityGlyphState, ConnectionGlyph, SessionChromeAction, SessionChromeModel,
};

const SLOT_SIZE: f32 = 44.0;
const SLOT_SPACING: f32 = 4.0;
const SLOT_COUNT: usize = 4;

pub const MATERIAL_SYMBOLS_FONT_FAMILY: &str = "frd-material-symbols-rounded";
const MATERIAL_SYMBOLS_FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/ui-icons/material-symbols-rounded-24-400.ttf"
));

pub fn install_session_chrome_font(definitions: &mut FontDefinitions) {
    definitions.font_data.insert(
        MATERIAL_SYMBOLS_FONT_FAMILY.to_owned(),
        FontData::from_static(MATERIAL_SYMBOLS_FONT_BYTES).into(),
    );
    definitions.families.insert(
        FontFamily::Name(MATERIAL_SYMBOLS_FONT_FAMILY.into()),
        vec![MATERIAL_SYMBOLS_FONT_FAMILY.to_owned()],
    );
}

fn material_symbol_font_id() -> FontId {
    FontId::new(24.0, FontFamily::Name(MATERIAL_SYMBOLS_FONT_FAMILY.into()))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SessionChromeMetrics {
    pub slot_size: f32,
    pub spacing: f32,
    pub total_width: f32,
    pub height: f32,
}

pub const fn session_chrome_metrics() -> SessionChromeMetrics {
    SessionChromeMetrics {
        slot_size: SLOT_SIZE,
        spacing: SLOT_SPACING,
        total_width: SLOT_SIZE * SLOT_COUNT as f32 + SLOT_SPACING * (SLOT_COUNT as f32 - 1.0),
        height: SLOT_SIZE,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GlyphSemantic {
    symbol_name: &'static str,
    codepoint: char,
    accessible_name: &'static str,
    tooltip: &'static str,
    available: bool,
}

fn connection_glyph(state: ConnectionGlyph) -> GlyphSemantic {
    match state {
        ConnectionGlyph::Connecting => GlyphSemantic {
            symbol_name: "progress_activity",
            codepoint: '\u{e9d0}',
            accessible_name: "正在连接",
            tooltip: "正在连接",
            available: true,
        },
        ConnectionGlyph::WaitingForFrame => GlyphSemantic {
            symbol_name: "hourglass_top",
            codepoint: '\u{ea5b}',
            accessible_name: "等待首个完整画面",
            tooltip: "等待首个完整画面",
            available: true,
        },
        ConnectionGlyph::Connected => GlyphSemantic {
            symbol_name: "check_circle",
            codepoint: '\u{f0be}',
            accessible_name: "已连接",
            tooltip: "已连接",
            available: true,
        },
        ConnectionGlyph::Disconnecting => GlyphSemantic {
            symbol_name: "pending",
            codepoint: '\u{ef64}',
            accessible_name: "正在断开连接",
            tooltip: "正在断开连接",
            available: true,
        },
        ConnectionGlyph::Failed => GlyphSemantic {
            symbol_name: "error",
            codepoint: '\u{f8b6}',
            accessible_name: "连接失败",
            tooltip: "连接失败",
            available: true,
        },
    }
}

fn audio_glyph(state: CapabilityGlyphState) -> GlyphSemantic {
    match state {
        CapabilityGlyphState::Available => GlyphSemantic {
            symbol_name: "volume_up",
            codepoint: '\u{e050}',
            accessible_name: "远程音频可用",
            tooltip: "远程音频可用",
            available: true,
        },
        CapabilityGlyphState::Unavailable => GlyphSemantic {
            symbol_name: "volume_off",
            codepoint: '\u{e04f}',
            accessible_name: "远程音频不可用",
            tooltip: "远程音频不可用",
            available: false,
        },
    }
}

fn clipboard_glyph(state: CapabilityGlyphState) -> GlyphSemantic {
    match state {
        CapabilityGlyphState::Available => GlyphSemantic {
            symbol_name: "content_paste",
            codepoint: '\u{e14f}',
            accessible_name: "剪贴板可用",
            tooltip: "剪贴板可用",
            available: true,
        },
        CapabilityGlyphState::Unavailable => GlyphSemantic {
            symbol_name: "content_paste_off",
            codepoint: '\u{e4f8}',
            accessible_name: "剪贴板不可用",
            tooltip: "剪贴板不可用",
            available: false,
        },
    }
}

fn action_glyph(action: Option<SessionChromeAction>) -> GlyphSemantic {
    match action {
        Some(SessionChromeAction::Cancel) => GlyphSemantic {
            symbol_name: "close",
            codepoint: '\u{e5cd}',
            accessible_name: "取消连接",
            tooltip: "取消连接",
            available: true,
        },
        Some(SessionChromeAction::Disconnect) => GlyphSemantic {
            symbol_name: "link_off",
            codepoint: '\u{e16f}',
            accessible_name: "断开连接",
            tooltip: "断开连接",
            available: true,
        },
        None => GlyphSemantic {
            symbol_name: "more_horiz",
            codepoint: '\u{e5d3}',
            accessible_name: "会话正在清理",
            tooltip: "会话正在清理",
            available: false,
        },
    }
}

pub fn show_session_chrome(ui: &mut Ui, model: &SessionChromeModel) -> Option<SessionChromeAction> {
    show_session_chrome_with_focus(ui, model, false).action
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionChromeRenderResult {
    pub action: Option<SessionChromeAction>,
    pub connection_id: egui::Id,
}

pub fn show_session_chrome_with_focus(
    ui: &mut Ui,
    model: &SessionChromeModel,
    focus_first: bool,
) -> SessionChromeRenderResult {
    let mut selected = None;
    let mut connection_id = None;
    let prior_spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing.x = SLOT_SPACING;
    ui.horizontal(|ui| {
        let mut connection = connection_glyph(model.connection);
        let diagnostic_tooltip;
        if let Some(diagnostics) = model.diagnostics.as_deref() {
            diagnostic_tooltip = format!("{}\n诊断：{diagnostics}", connection.tooltip);
            connection.tooltip = "";
        } else {
            diagnostic_tooltip = connection.tooltip.to_owned();
        }
        let connection_accessible = accessible_label(connection, model.diagnostics.as_deref());
        let connection_response = show_glyph(
            ui,
            connection,
            Some(&diagnostic_tooltip),
            Some(&connection_accessible),
            false,
        );
        if focus_first {
            connection_response.request_focus();
        }
        connection_id = Some(connection_response.id);
        show_glyph(ui, audio_glyph(model.audio), None, None, false);
        show_glyph(ui, clipboard_glyph(model.clipboard), None, None, false);

        let action = action_glyph(model.action);
        if show_glyph(ui, action, None, None, action.available).clicked() {
            selected = model.action;
        }
    });
    ui.spacing_mut().item_spacing = prior_spacing;
    SessionChromeRenderResult {
        action: selected,
        connection_id: connection_id.expect("session chrome always renders its connection glyph"),
    }
}

fn accessible_label(semantic: GlyphSemantic, diagnostics: Option<&str>) -> String {
    diagnostics.map_or_else(
        || semantic.accessible_name.to_owned(),
        |diagnostics| format!("{}；诊断：{diagnostics}", semantic.accessible_name),
    )
}

fn show_glyph(
    ui: &mut Ui,
    semantic: GlyphSemantic,
    tooltip_override: Option<&str>,
    accessible_override: Option<&str>,
    actionable: bool,
) -> Response {
    let sense = if actionable {
        Sense::click()
    } else {
        Sense::focusable_noninteractive()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SLOT_SIZE), sense);
    response.widget_info(|| {
        WidgetInfo::labeled(
            if actionable {
                WidgetType::Button
            } else {
                WidgetType::Label
            },
            semantic.available,
            accessible_override.unwrap_or(semantic.accessible_name),
        )
    });

    let fill = match glyph_fill_state(
        response.hovered(),
        response.has_focus(),
        actionable && response.is_pointer_button_down_on(),
    ) {
        GlyphFillState::None => Color32::TRANSPARENT,
        GlyphFillState::Hover => ui.visuals().widgets.hovered.bg_fill,
        GlyphFillState::Pressed => ui.visuals().widgets.active.bg_fill,
    };
    ui.painter().rect_filled(rect.shrink(2.0), 5.0, fill);
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            5.0,
            ui.visuals().selection.stroke,
            egui::StrokeKind::Inside,
        );
    }
    let color = if semantic.available {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        semantic.codepoint,
        material_symbol_font_id(),
        color,
    );
    response.on_hover_text(
        tooltip_override
            .filter(|text| !text.is_empty())
            .unwrap_or(semantic.tooltip),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GlyphFillState {
    None,
    Hover,
    Pressed,
}

fn glyph_fill_state(hovered: bool, focused: bool, pressed: bool) -> GlyphFillState {
    if pressed {
        GlyphFillState::Pressed
    } else if hovered || focused {
        GlyphFillState::Hover
    } else {
        GlyphFillState::None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui::{FontDefinitions, FontFamily};
    use frd_ui_model::{
        CapabilityGlyphState, ConnectionGlyph, SessionChromeAction, SessionChromeModel,
    };

    use super::{
        accessible_label, action_glyph, audio_glyph, clipboard_glyph, connection_glyph,
        glyph_fill_state, install_session_chrome_font, material_symbol_font_id,
        session_chrome_metrics, GlyphFillState, MATERIAL_SYMBOLS_FONT_FAMILY,
    };

    #[test]
    fn programmatic_local_chrome_entry_focuses_the_first_accessible_glyph() {
        use std::cell::Cell;

        let context = egui::Context::default();
        context.enable_accesskit();
        let mut fonts = FontDefinitions::default();
        install_session_chrome_font(&mut fonts);
        context.set_fonts(fonts);
        let connection_id = Cell::new(None);
        let mut output = context.run_ui(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let result = super::show_session_chrome_with_focus(
                    ui,
                    &SessionChromeModel {
                        connection: ConnectionGlyph::Connected,
                        diagnostics: None,
                        audio: CapabilityGlyphState::Unavailable,
                        clipboard: CapabilityGlyphState::Unavailable,
                        action: Some(SessionChromeAction::Disconnect),
                    },
                    true,
                );
                connection_id.set(Some(result.connection_id));
            });
        });

        assert_eq!(
            context.memory(|memory| memory.focused()),
            connection_id.get()
        );
        assert_eq!(
            connection_glyph(ConnectionGlyph::Connected).accessible_name,
            "已连接"
        );
        let connection_node_id = connection_id
            .get()
            .expect("connection glyph id was captured")
            .accesskit_id();
        let connection_node = output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("accessibility tree is enabled")
            .nodes
            .iter()
            .find_map(|(id, node)| (*id == connection_node_id).then_some(node))
            .expect("focused connection glyph remains in the accessibility tree");
        output.textures_delta.clear();
        assert_eq!(
            connection_node.label().or_else(|| connection_node.value()),
            Some("已连接")
        );
    }

    #[test]
    fn material_symbols_font_is_registered_as_an_isolated_named_family() {
        let mut definitions = FontDefinitions::default();

        install_session_chrome_font(&mut definitions);

        assert!(definitions
            .font_data
            .contains_key(MATERIAL_SYMBOLS_FONT_FAMILY));
        assert_eq!(
            definitions
                .families
                .get(&FontFamily::Name(Arc::from(MATERIAL_SYMBOLS_FONT_FAMILY)))
                .cloned(),
            Some(vec![MATERIAL_SYMBOLS_FONT_FAMILY.to_owned()])
        );
        assert!(!definitions
            .families
            .get(&FontFamily::Proportional)
            .is_some_and(|fonts| fonts
                .iter()
                .any(|font| font == MATERIAL_SYMBOLS_FONT_FAMILY)));
    }

    #[test]
    fn material_symbols_render_at_the_official_24dp_optical_size() {
        let font_id = material_symbol_font_id();

        assert_eq!(font_id.size, 24.0);
        assert_eq!(
            font_id.family,
            FontFamily::Name(Arc::from(MATERIAL_SYMBOLS_FONT_FAMILY))
        );
    }

    #[test]
    fn every_connection_state_has_a_distinct_shape_and_accessible_label() {
        let states = [
            ConnectionGlyph::Connecting,
            ConnectionGlyph::WaitingForFrame,
            ConnectionGlyph::Connected,
            ConnectionGlyph::Disconnecting,
            ConnectionGlyph::Failed,
        ];
        let semantics = states.map(connection_glyph);
        for left in 0..semantics.len() {
            for right in left + 1..semantics.len() {
                assert_ne!(semantics[left].symbol_name, semantics[right].symbol_name);
                assert_ne!(
                    semantics[left].accessible_name,
                    semantics[right].accessible_name
                );
            }
        }
        assert_eq!(
            connection_glyph(ConnectionGlyph::Connected).accessible_name,
            "已连接"
        );
    }

    #[test]
    fn capability_shapes_do_not_rely_on_color_only() {
        assert_ne!(
            audio_glyph(CapabilityGlyphState::Available).symbol_name,
            audio_glyph(CapabilityGlyphState::Unavailable).symbol_name
        );
        assert_ne!(
            clipboard_glyph(CapabilityGlyphState::Available).symbol_name,
            clipboard_glyph(CapabilityGlyphState::Unavailable).symbol_name
        );
        assert!(!audio_glyph(CapabilityGlyphState::Unavailable).available);
        assert!(!clipboard_glyph(CapabilityGlyphState::Unavailable).available);
    }

    #[test]
    fn semantic_states_use_official_material_symbol_names_and_codepoints() {
        let cases = [
            (
                connection_glyph(ConnectionGlyph::Connecting).symbol_name,
                connection_glyph(ConnectionGlyph::Connecting).codepoint,
                "progress_activity",
                '\u{e9d0}',
            ),
            (
                connection_glyph(ConnectionGlyph::WaitingForFrame).symbol_name,
                connection_glyph(ConnectionGlyph::WaitingForFrame).codepoint,
                "hourglass_top",
                '\u{ea5b}',
            ),
            (
                connection_glyph(ConnectionGlyph::Connected).symbol_name,
                connection_glyph(ConnectionGlyph::Connected).codepoint,
                "check_circle",
                '\u{f0be}',
            ),
            (
                connection_glyph(ConnectionGlyph::Disconnecting).symbol_name,
                connection_glyph(ConnectionGlyph::Disconnecting).codepoint,
                "pending",
                '\u{ef64}',
            ),
            (
                connection_glyph(ConnectionGlyph::Failed).symbol_name,
                connection_glyph(ConnectionGlyph::Failed).codepoint,
                "error",
                '\u{f8b6}',
            ),
            (
                audio_glyph(CapabilityGlyphState::Available).symbol_name,
                audio_glyph(CapabilityGlyphState::Available).codepoint,
                "volume_up",
                '\u{e050}',
            ),
            (
                audio_glyph(CapabilityGlyphState::Unavailable).symbol_name,
                audio_glyph(CapabilityGlyphState::Unavailable).codepoint,
                "volume_off",
                '\u{e04f}',
            ),
            (
                clipboard_glyph(CapabilityGlyphState::Available).symbol_name,
                clipboard_glyph(CapabilityGlyphState::Available).codepoint,
                "content_paste",
                '\u{e14f}',
            ),
            (
                clipboard_glyph(CapabilityGlyphState::Unavailable).symbol_name,
                clipboard_glyph(CapabilityGlyphState::Unavailable).codepoint,
                "content_paste_off",
                '\u{e4f8}',
            ),
            (
                action_glyph(Some(SessionChromeAction::Cancel)).symbol_name,
                action_glyph(Some(SessionChromeAction::Cancel)).codepoint,
                "close",
                '\u{e5cd}',
            ),
            (
                action_glyph(Some(SessionChromeAction::Disconnect)).symbol_name,
                action_glyph(Some(SessionChromeAction::Disconnect)).codepoint,
                "link_off",
                '\u{e16f}',
            ),
        ];

        for (actual_name, actual_codepoint, expected_name, expected_codepoint) in cases {
            assert_eq!(actual_name, expected_name);
            assert_eq!(actual_codepoint, expected_codepoint);
        }
    }

    #[test]
    fn missing_action_keeps_the_fourth_slot_but_cannot_dispatch() {
        let semantic = action_glyph(None);
        assert!(!semantic.available);
        assert_eq!(semantic.accessible_name, "会话正在清理");
        assert_eq!(session_chrome_metrics().slot_size, 44.0);

        let connected = SessionChromeModel {
            connection: ConnectionGlyph::Connected,
            diagnostics: None,
            audio: CapabilityGlyphState::Available,
            clipboard: CapabilityGlyphState::Available,
            action: Some(SessionChromeAction::Disconnect),
        };
        let waiting = SessionChromeModel {
            connection: ConnectionGlyph::WaitingForFrame,
            diagnostics: None,
            audio: CapabilityGlyphState::Unavailable,
            clipboard: CapabilityGlyphState::Unavailable,
            action: Some(SessionChromeAction::Cancel),
        };
        assert_eq!(session_chrome_metrics(), session_chrome_metrics());
        assert_ne!(connected.action, waiting.action);
    }

    #[test]
    fn actionable_slot_uses_a_distinct_pressed_visual_state() {
        assert_eq!(glyph_fill_state(false, false, false), GlyphFillState::None);
        assert_eq!(glyph_fill_state(true, false, false), GlyphFillState::Hover);
        assert_eq!(glyph_fill_state(false, true, false), GlyphFillState::Hover);
        assert_eq!(glyph_fill_state(true, true, true), GlyphFillState::Pressed);
        assert_ne!(
            glyph_fill_state(true, false, false),
            glyph_fill_state(true, false, true)
        );
    }

    #[test]
    fn connection_diagnostics_are_exposed_in_the_accessible_label() {
        let semantic = connection_glyph(ConnectionGlyph::Failed);

        assert_eq!(
            accessible_label(semantic, Some("apple_hpss_session_failed")),
            "连接失败；诊断：apple_hpss_session_failed"
        );
        assert_eq!(accessible_label(semantic, None), "连接失败");
    }
}
