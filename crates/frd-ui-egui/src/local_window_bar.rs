use egui::{
    Align2, Color32, FontFamily, FontId, Id, LayerId, Order, Sense, WidgetInfo, WidgetType,
};
use frd_ui_model::IslandAction;

use crate::MATERIAL_SYMBOLS_FONT_FAMILY;

const CLOSE_CODEPOINT: char = '\u{e5cd}';
const CLOSE_LABEL: &str = "关闭窗口";

#[derive(Clone, Debug, PartialEq)]
pub struct LocalWindowBarResult {
    pub hit_rects: Vec<(egui::Rect, IslandAction)>,
    pub action: Option<IslandAction>,
}

pub fn show_local_window_bar(
    context: &egui::Context,
    bar_rect: egui::Rect,
    close_rect: Option<egui::Rect>,
) -> LocalWindowBarResult {
    let style = context.style_of(context.theme());
    let visuals = &style.visuals;
    let painter = context.layer_painter(LayerId::new(
        Order::Foreground,
        Id::new("freeremotedesk-local-window-bar"),
    ));
    painter.rect_filled(bar_rect, 0.0, visuals.panel_fill);
    painter.text(
        bar_rect.center(),
        Align2::CENTER_CENTER,
        "FreeRemoteDesk",
        FontId::proportional(13.0),
        visuals.weak_text_color(),
    );

    let mut result = LocalWindowBarResult {
        hit_rects: Vec::with_capacity(usize::from(close_rect.is_some())),
        action: None,
    };
    let Some(close_rect) = close_rect else {
        return result;
    };

    egui::Area::new(Id::new("freeremotedesk-local-window-bar"))
        .order(Order::Foreground)
        .fixed_pos(close_rect.min)
        .show(context, |ui| {
            ui.set_min_size(close_rect.size());
            ui.set_max_size(close_rect.size());
            let (rect, response) = ui.allocate_exact_size(close_rect.size(), Sense::click());
            response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, CLOSE_LABEL));

            let fill = if response.is_pointer_button_down_on() {
                ui.visuals().widgets.active.bg_fill
            } else if response.hovered() || response.has_focus() {
                ui.visuals().widgets.hovered.bg_fill
            } else {
                Color32::TRANSPARENT
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
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                CLOSE_CODEPOINT,
                FontId::new(24.0, FontFamily::Name(MATERIAL_SYMBOLS_FONT_FAMILY.into())),
                ui.visuals().text_color(),
            );

            let open = response.has_focus() || egui::Tooltip::should_show_tooltip(&response, true);
            let mut tooltip = egui::Tooltip::for_widget(&response);
            tooltip.popup = tooltip.popup.open(open);
            tooltip.show(|ui| {
                ui.label(CLOSE_LABEL);
            });

            result.hit_rects.push((rect, IslandAction::CloseWindow));
            if response.clicked() {
                result.action = Some(IslandAction::CloseWindow);
            }
        });
    result
}

#[cfg(test)]
mod tests {
    use egui::{pos2, vec2, Context, FontDefinitions, Rect};
    use frd_ui_model::IslandAction;

    #[test]
    fn local_window_bar_reports_the_exact_close_hit_rectangle() {
        let context = Context::default();
        let mut fonts = FontDefinitions::default();
        crate::install_control_island_font(&mut fonts);
        context.set_fonts(fonts);
        let bar_rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(460.0, 44.0));
        let close_rect = Rect::from_min_size(pos2(416.0, 0.0), vec2(44.0, 44.0));
        let mut result = None;

        let mut output = context.run_ui(Default::default(), |context| {
            result = Some(super::show_local_window_bar(
                context,
                bar_rect,
                Some(close_rect),
            ));
        });
        output.textures_delta.clear();
        let result = result.expect("renderer returns local chrome geometry");

        assert_eq!(
            result.hit_rects,
            vec![(close_rect, IslandAction::CloseWindow)]
        );
        assert_eq!(result.action, None);
    }

    #[test]
    fn local_window_bar_paints_the_close_glyph_after_its_covering_panel() {
        let context = Context::default();
        let mut fonts = FontDefinitions::default();
        crate::install_control_island_font(&mut fonts);
        context.set_fonts(fonts);
        let bar_rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(460.0, 44.0));
        let close_rect = Rect::from_min_size(pos2(416.0, 0.0), vec2(44.0, 44.0));

        // The first frame establishes the Area's persisted size before inspecting paint order.
        let mut first_output = context.run_ui(Default::default(), |context| {
            super::show_local_window_bar(context, bar_rect, Some(close_rect));
        });
        first_output.textures_delta.clear();
        let mut output = context.run_ui(Default::default(), |context| {
            super::show_local_window_bar(context, bar_rect, Some(close_rect));
        });
        output.textures_delta.clear();

        let panel_index = output
            .shapes
            .iter()
            .position(|clipped_shape| {
                matches!(
                    &clipped_shape.shape,
                    egui::Shape::Rect(rect_shape)
                        if rect_shape.rect == bar_rect
                            && rect_shape.fill != egui::Color32::TRANSPARENT
                )
            })
            .expect("the local window bar must paint its opaque panel");
        let close_glyph_index = output
            .shapes
            .iter()
            .position(|clipped_shape| {
                matches!(
                    &clipped_shape.shape,
                    egui::Shape::Text(text_shape)
                        if text_shape.galley.text() == super::CLOSE_CODEPOINT.to_string()
                )
            })
            .expect("the local window bar must paint the Material close glyph");

        assert!(
            panel_index < close_glyph_index,
            "the close glyph must be painted after the panel that covers its rectangle; panel={panel_index}, close={close_glyph_index}"
        );
    }
}
