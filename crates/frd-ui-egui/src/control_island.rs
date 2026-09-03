use egui::{
    Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Id, LayerId, Order, Response,
    Sense, Ui, Vec2, WidgetInfo, WidgetType,
};
use frd_ui_model::{
    CapabilityGlyphState, ConnectionGlyph, IslandAction, IslandWindowCapabilities,
    SessionChromeModel, SessionTiming, SessionTimingSource,
};

const SLOT_SIZE: f32 = 44.0;
const SLOT_SPACING: f32 = 4.0;
const SLOT_COUNT: usize = 4;
const FRAME_RESPONSE_WIDTH: f32 = 64.0;
const FRAME_RESPONSE_TOOLTIP: &str =
    "画面响应时间（从画面更新请求成功发送到完整更新处理完成，不含本地呈现）";
const MEDIA_INGRESS_TO_PRESENT_TOOLTIP: &str =
    "视频接收至呈现（从首个已认证视频包到本地画面呈现，不含服务端采集编码及此前网络时间）";

pub const MATERIAL_SYMBOLS_FONT_FAMILY: &str = "frd-material-symbols-rounded";
const MATERIAL_SYMBOLS_FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/ui-icons/material-symbols-rounded-24-400.ttf"
));

pub fn install_control_island_font(definitions: &mut FontDefinitions) {
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
    pub frame_response_width: f32,
    pub spacing: f32,
    pub total_width: f32,
    pub height: f32,
}

pub const fn session_chrome_metrics() -> SessionChromeMetrics {
    SessionChromeMetrics {
        slot_size: SLOT_SIZE,
        frame_response_width: FRAME_RESPONSE_WIDTH,
        spacing: SLOT_SPACING,
        total_width: SLOT_SIZE * SLOT_COUNT as f32
            + FRAME_RESPONSE_WIDTH
            + SLOT_SPACING * SLOT_COUNT as f32,
        height: SLOT_SIZE,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FrameResponseSemantic {
    text: String,
    accessible_name: String,
    tooltip: &'static str,
}

fn presentation_timing_semantic(timing: Option<SessionTiming>) -> FrameResponseSemantic {
    let text = match timing.map(|timing| timing.milliseconds) {
        None => "-- ms".to_owned(),
        Some(ms) if ms < 1_000 => format!("{ms} ms"),
        Some(ms) if ms < 100_000 => format!("{}.{:01} s", ms / 1_000, ms % 1_000 / 100),
        Some(ms) if ms < 1_000_000 => format!("{} s", ms / 1_000),
        Some(_) => "999+ s".to_owned(),
    };
    let tooltip = match timing.map(|timing| timing.source) {
        Some(SessionTimingSource::MediaIngressToPresent) => MEDIA_INGRESS_TO_PRESENT_TOOLTIP,
        Some(SessionTimingSource::FramebufferResponse) | None => FRAME_RESPONSE_TOOLTIP,
    };
    FrameResponseSemantic {
        accessible_name: format!("{tooltip}；当前值：{text}"),
        text,
        tooltip,
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

fn drag_glyph() -> GlyphSemantic {
    GlyphSemantic {
        symbol_name: "drag_indicator",
        codepoint: '\u{e945}',
        accessible_name: "移动控制岛",
        tooltip: "拖动以移动控制岛",
        available: true,
    }
}

fn action_glyph(action: Option<IslandAction>) -> GlyphSemantic {
    match action {
        Some(IslandAction::CancelConnect) => GlyphSemantic {
            symbol_name: "close",
            codepoint: '\u{e5cd}',
            accessible_name: "取消连接",
            tooltip: "取消连接",
            available: true,
        },
        Some(IslandAction::Disconnect) => GlyphSemantic {
            symbol_name: "link_off",
            codepoint: '\u{e16f}',
            accessible_name: "断开连接",
            tooltip: "断开连接",
            available: true,
        },
        Some(_) => GlyphSemantic {
            symbol_name: "more_horiz",
            codepoint: '\u{e5d3}',
            accessible_name: "操作不可用",
            tooltip: "操作不可用",
            available: false,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IslandActionSemantic {
    pub action: IslandAction,
    pub symbol_name: &'static str,
    pub codepoint: char,
    pub accessible_name: &'static str,
    pub tooltip: &'static str,
    pub target_size: f32,
}

impl IslandActionSemantic {
    fn glyph(self) -> GlyphSemantic {
        GlyphSemantic {
            symbol_name: self.symbol_name,
            codepoint: self.codepoint,
            accessible_name: self.accessible_name,
            tooltip: self.tooltip,
            available: true,
        }
    }
}

pub fn window_action_semantics(
    capabilities: IslandWindowCapabilities,
    maximized: bool,
) -> Vec<IslandActionSemantic> {
    let mut semantics = Vec::with_capacity(3);
    if capabilities.minimize {
        semantics.push(IslandActionSemantic {
            action: IslandAction::MinimizeWindow,
            symbol_name: "remove",
            codepoint: '\u{e15b}',
            accessible_name: "最小化窗口",
            tooltip: "最小化窗口",
            target_size: SLOT_SIZE,
        });
    }
    if capabilities.maximize {
        semantics.push(IslandActionSemantic {
            action: IslandAction::ToggleMaximizeWindow,
            symbol_name: if maximized {
                "fullscreen_exit"
            } else {
                "fullscreen"
            },
            codepoint: if maximized { '\u{e5d1}' } else { '\u{e5d0}' },
            accessible_name: if maximized {
                "还原窗口"
            } else {
                "最大化窗口"
            },
            tooltip: if maximized {
                "还原窗口"
            } else {
                "最大化窗口"
            },
            target_size: SLOT_SIZE,
        });
    }
    if capabilities.close {
        semantics.push(IslandActionSemantic {
            action: IslandAction::CloseWindow,
            symbol_name: "close",
            codepoint: '\u{e5cd}',
            accessible_name: "关闭窗口",
            tooltip: "关闭窗口",
            target_size: SLOT_SIZE,
        });
    }
    semantics
}

pub fn control_island_metrics(
    window_capabilities: IslandWindowCapabilities,
) -> SessionChromeMetrics {
    let session = session_chrome_metrics();
    let additional_slots = 1 + window_action_semantics(window_capabilities, false).len();
    SessionChromeMetrics {
        total_width: session.total_width + (SLOT_SIZE + SLOT_SPACING) * additional_slots as f32,
        ..session
    }
}

pub struct ControlIslandRenderInput<'a> {
    pub model: &'a SessionChromeModel,
    pub window_capabilities: IslandWindowCapabilities,
    pub visible: bool,
    pub maximized: bool,
    pub island_rect: egui::Rect,
    pub reveal_line_rect: egui::Rect,
    pub focus_first: bool,
    pub opaque_material: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlIslandRenderResult {
    pub action: Option<IslandAction>,
    pub hovered_union: bool,
    pub focused_union: bool,
    pub pressed: bool,
    pub reposition_delta: egui::Vec2,
    pub window_move_requested: bool,
    pub hit_rects: Vec<(egui::Rect, IslandAction)>,
    pub reveal_line_alpha: f32,
}

impl Default for ControlIslandRenderResult {
    fn default() -> Self {
        Self {
            action: None,
            hovered_union: false,
            focused_union: false,
            pressed: false,
            reposition_delta: egui::Vec2::ZERO,
            window_move_requested: false,
            hit_rects: Vec::new(),
            reveal_line_alpha: 0.0,
        }
    }
}

pub fn show_control_island(
    ctx: &egui::Context,
    input: ControlIslandRenderInput<'_>,
) -> ControlIslandRenderResult {
    if !input.visible {
        let alpha = 0.5;
        ctx.layer_painter(LayerId::new(
            Order::Foreground,
            Id::new("freeremotedesk-control-island-reveal-line"),
        ))
        .rect_filled(
            input.reveal_line_rect,
            input.reveal_line_rect.height() / 2.0,
            Color32::from_rgba_unmultiplied(43, 181, 99, (alpha * 255.0) as u8),
        );
        return ControlIslandRenderResult {
            reveal_line_alpha: alpha,
            ..ControlIslandRenderResult::default()
        };
    }

    let mut result = ControlIslandRenderResult::default();
    let response = egui::Area::new(Id::new("freeremotedesk-control-island"))
        .order(Order::Foreground)
        .fixed_pos(input.island_rect.min)
        .show(ctx, |ui| {
            ui.set_min_size(input.island_rect.size());
            ui.set_max_size(input.island_rect.size());
            let frame_fill = if input.opaque_material {
                ui.visuals().panel_fill
            } else if ui.visuals().dark_mode {
                Color32::from_rgba_unmultiplied(28, 28, 30, 58)
            } else {
                Color32::from_rgba_unmultiplied(245, 245, 247, 58)
            };
            let border = if ui.visuals().dark_mode {
                Color32::from_rgba_unmultiplied(255, 255, 255, 72)
            } else {
                Color32::from_rgba_unmultiplied(0, 0, 0, 64)
            };
            egui::Frame::new()
                .fill(frame_fill)
                .stroke(egui::Stroke::new(1.0, border))
                .corner_radius(16)
                .inner_margin(egui::Margin::symmetric(4, 4))
                .show(ui, |ui| render_visible_island(ui, &input, &mut result));
        })
        .response;
    result.hovered_union |= response.hovered();
    result.pressed |= response.is_pointer_button_down_on();
    result
}

fn render_visible_island(
    ui: &mut Ui,
    input: &ControlIslandRenderInput<'_>,
    result: &mut ControlIslandRenderResult,
) {
    let full_width = control_island_metrics(input.window_capabilities).total_width;
    let collapse_capabilities = input.island_rect.width() < full_width;
    let prior_spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing.x = SLOT_SPACING;
    ui.horizontal(|ui| {
        let drag = show_glyph_with_sense(ui, drag_glyph(), None, None, Sense::drag());
        observe_response(result, &drag);
        result.reposition_delta = drag.drag_delta();

        let mut connection = connection_glyph(input.model.connection);
        let diagnostic_tooltip;
        if let Some(diagnostics) = input.model.diagnostics.as_deref() {
            diagnostic_tooltip = format!("{}\n诊断：{diagnostics}", connection.tooltip);
            connection.tooltip = "";
        } else {
            diagnostic_tooltip = connection.tooltip.to_owned();
        }
        let connection_accessible =
            accessible_label(connection, input.model.diagnostics.as_deref());
        let connection_response = show_glyph(
            ui,
            connection,
            Some(&diagnostic_tooltip),
            Some(&connection_accessible),
            false,
        );
        if input.focus_first {
            connection_response.request_focus();
        }
        observe_response(result, &connection_response);

        let timing = show_presentation_timing(ui, input.model.presentation_timing);
        observe_response(result, &timing);
        if collapse_capabilities {
            let capabilities = GlyphSemantic {
                symbol_name: "more_horiz",
                codepoint: '\u{e5d3}',
                accessible_name: "远程音频与剪贴板状态",
                tooltip: "远程音频与剪贴板状态",
                available: false,
            };
            let tooltip = format!(
                "{}；{}",
                audio_glyph(input.model.audio).tooltip,
                clipboard_glyph(input.model.clipboard).tooltip
            );
            let collapsed = show_glyph(ui, capabilities, Some(&tooltip), Some(&tooltip), false);
            observe_response(result, &collapsed);
        } else {
            let audio = show_glyph(ui, audio_glyph(input.model.audio), None, None, false);
            observe_response(result, &audio);
            let clipboard = show_glyph(
                ui,
                clipboard_glyph(input.model.clipboard),
                None,
                None,
                false,
            );
            observe_response(result, &clipboard);
        }

        let action = action_glyph(input.model.action);
        let action_response = show_glyph(ui, action, None, None, action.available);
        observe_response(result, &action_response);
        if action.available {
            if let Some(action) = input.model.action {
                result.hit_rects.push((action_response.rect, action));
                if action_response.clicked() {
                    result.action = Some(action);
                }
            }
        }

        for semantic in window_action_semantics(input.window_capabilities, input.maximized) {
            let response = show_glyph(ui, semantic.glyph(), None, None, true);
            observe_response(result, &response);
            result.hit_rects.push((response.rect, semantic.action));
            if response.clicked() {
                result.action = Some(semantic.action);
            }
        }
    });
    ui.spacing_mut().item_spacing = prior_spacing;
}

fn observe_response(result: &mut ControlIslandRenderResult, response: &Response) {
    result.hovered_union |= response.hovered();
    result.focused_union |= response.has_focus();
    result.pressed |= response.is_pointer_button_down_on();
}

fn show_presentation_timing(ui: &mut Ui, timing: Option<SessionTiming>) -> Response {
    let semantic = presentation_timing_semantic(timing);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(FRAME_RESPONSE_WIDTH, SLOT_SIZE),
        Sense::focusable_noninteractive(),
    );
    response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Label, true, semantic.accessible_name.clone())
    });
    let fill = match glyph_fill_state(response.hovered(), response.has_focus(), false) {
        GlyphFillState::None => contrast_plate_fill(ui),
        GlyphFillState::Hover => ui.visuals().widgets.hovered.bg_fill,
        GlyphFillState::Pressed => unreachable!("frame response timing is not actionable"),
    };
    ui.painter().rect_filled(rect.shrink(2.0), 5.0, fill);
    ui.painter().rect_stroke(
        rect.shrink(2.0),
        5.0,
        if response.has_focus() {
            ui.visuals().selection.stroke
        } else {
            ui.visuals().widgets.inactive.bg_stroke
        },
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        semantic.text,
        FontId::proportional(13.0),
        ui.visuals().text_color(),
    );
    response.on_hover_text(semantic.tooltip)
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
    show_glyph_with_sense(ui, semantic, tooltip_override, accessible_override, sense)
}

fn show_glyph_with_sense(
    ui: &mut Ui,
    semantic: GlyphSemantic,
    tooltip_override: Option<&str>,
    accessible_override: Option<&str>,
    sense: Sense,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SLOT_SIZE), sense);
    let actionable = sense.interactive();
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
        GlyphFillState::None => contrast_plate_fill(ui),
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

fn contrast_plate_fill(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgba_unmultiplied(24, 24, 27, 220)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 230)
    }
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
        CapabilityGlyphState, ConnectionGlyph, IslandAction, SessionChromeModel, SessionTiming,
        SessionTimingSource,
    };

    use super::{
        accessible_label, action_glyph, audio_glyph, clipboard_glyph, connection_glyph, drag_glyph,
        glyph_fill_state, install_control_island_font, material_symbol_font_id,
        presentation_timing_semantic, session_chrome_metrics, GlyphFillState,
        MATERIAL_SYMBOLS_FONT_FAMILY,
    };

    #[test]
    fn frame_response_box_formats_text_accessibility_tooltip_and_metrics() {
        let unknown = presentation_timing_semantic(None);
        assert_eq!(unknown.text, "-- ms");
        assert_eq!(
            unknown.accessible_name,
            "画面响应时间（从画面更新请求成功发送到完整更新处理完成，不含本地呈现）；当前值：-- ms"
        );
        assert_eq!(
            unknown.tooltip,
            "画面响应时间（从画面更新请求成功发送到完整更新处理完成，不含本地呈现）"
        );

        assert_eq!(
            presentation_timing_semantic(Some(SessionTiming {
                source: SessionTimingSource::FramebufferResponse,
                milliseconds: 37,
            }))
            .text,
            "37 ms"
        );
        for (milliseconds, expected) in [
            (0, "0 ms"),
            (999, "999 ms"),
            (1_000, "1.0 s"),
            (12_399, "12.3 s"),
            (99_999, "99.9 s"),
            (100_000, "100 s"),
            (999_999, "999 s"),
            (1_000_000, "999+ s"),
            (u32::MAX, "999+ s"),
        ] {
            assert_eq!(
                presentation_timing_semantic(Some(SessionTiming {
                    source: SessionTimingSource::FramebufferResponse,
                    milliseconds,
                }))
                .text,
                expected
            );
        }
        let media = presentation_timing_semantic(Some(SessionTiming {
            source: SessionTimingSource::MediaIngressToPresent,
            milliseconds: 24,
        }));
        assert_eq!(media.text, "24 ms");
        assert_eq!(
            media.tooltip,
            "视频接收至呈现（从首个已认证视频包到本地画面呈现，不含服务端采集编码及此前网络时间）"
        );

        let metrics = session_chrome_metrics();
        assert_eq!(metrics.frame_response_width, 64.0);
        assert_eq!(metrics.height, 44.0);
        assert_eq!(metrics.total_width, 256.0);
    }

    #[test]
    fn programmatic_local_chrome_entry_focuses_the_first_accessible_glyph() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut fonts = FontDefinitions::default();
        install_control_island_font(&mut fonts);
        context.set_fonts(fonts);
        let model = SessionChromeModel {
            connection: ConnectionGlyph::Connected,
            diagnostics: None,
            presentation_timing: None,
            audio: CapabilityGlyphState::Unavailable,
            clipboard: CapabilityGlyphState::Unavailable,
            action: Some(IslandAction::Disconnect),
        };
        let mut output = context.run_ui(Default::default(), |context| {
            super::show_control_island(
                context,
                super::ControlIslandRenderInput {
                    model: &model,
                    window_capabilities: frd_ui_model::IslandWindowCapabilities::NONE,
                    visible: true,
                    maximized: false,
                    island_rect: egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(304.0, 52.0),
                    ),
                    reveal_line_rect: egui::Rect::NOTHING,
                    focus_first: true,
                    opaque_material: false,
                },
            );
        });

        let connection_id = context
            .memory(|memory| memory.focused())
            .expect("connection glyph receives programmatic focus");
        assert_eq!(
            connection_glyph(ConnectionGlyph::Connected).accessible_name,
            "已连接"
        );
        let connection_node_id = connection_id.accesskit_id();
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

        install_control_island_font(&mut definitions);

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
                drag_glyph().symbol_name,
                drag_glyph().codepoint,
                "drag_indicator",
                '\u{e945}',
            ),
            (
                action_glyph(Some(IslandAction::CancelConnect)).symbol_name,
                action_glyph(Some(IslandAction::CancelConnect)).codepoint,
                "close",
                '\u{e5cd}',
            ),
            (
                action_glyph(Some(IslandAction::Disconnect)).symbol_name,
                action_glyph(Some(IslandAction::Disconnect)).codepoint,
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
            presentation_timing: None,
            audio: CapabilityGlyphState::Available,
            clipboard: CapabilityGlyphState::Available,
            action: Some(IslandAction::Disconnect),
        };
        let waiting = SessionChromeModel {
            connection: ConnectionGlyph::WaitingForFrame,
            diagnostics: None,
            presentation_timing: None,
            audio: CapabilityGlyphState::Unavailable,
            clipboard: CapabilityGlyphState::Unavailable,
            action: Some(IslandAction::CancelConnect),
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
