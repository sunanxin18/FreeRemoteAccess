use std::ops::Range;

use egui::epaint::text::CharIndex;
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Button, ComboBox, CornerRadius, DragValue, Event, FontFamily, FontId, Frame, ImeEvent,
    Key, Layout, Margin, Response, RichText, ScrollArea, TextBuffer, TextEdit, Ui, Vec2,
    WidgetInfo, WidgetType,
};
use frd_app::{AppIntent, AppPage};
use frd_core::{SecretBuffer, TargetSystem};
use frd_protocol_api::{ProtocolCatalog, ProtocolSelection};
use frd_ui_model::{ConnectionForm, ProtocolChoice};

use crate::login_icons::{icon_button, show_icon, LoginIcon, LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY};

const COMPACT_LOGIN_PAIR_MIN_WIDTH: f32 = 400.0;
const OUTLINED_FIELD_HORIZONTAL_INSET: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CompactIdentityFieldLayout {
    pub outer_width: f32,
    pub outer_height: f32,
    pub content_width: f32,
    pub text_width: f32,
    pub trailing_width: f32,
    pub trailing_height: f32,
}

pub(crate) fn compact_identity_field_layout(
    outer_width: f32,
    leading_slot_width: f32,
    trailing_slot_width: f32,
    item_spacing: f32,
) -> CompactIdentityFieldLayout {
    let content_width = (outer_width - OUTLINED_FIELD_HORIZONTAL_INSET * 2.0).max(0.0);
    let trailing_spacing = if trailing_slot_width > 0.0 {
        item_spacing
    } else {
        0.0
    };
    let text_width = (content_width
        - leading_slot_width
        - item_spacing
        - trailing_slot_width
        - trailing_spacing)
        .max(0.0);

    CompactIdentityFieldLayout {
        outer_width,
        outer_height: compact_login_metrics().field_height,
        content_width,
        text_width,
        trailing_width: trailing_slot_width,
        trailing_height: trailing_slot_width,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompactLoginMetrics {
    pub initial_window_width: f64,
    pub initial_window_height: f64,
    pub minimum_window_width: f64,
    pub minimum_window_height: f64,
    pub content_max_width: f32,
    pub content_inset: f32,
    pub field_height: f32,
    pub username_width: f32,
    pub password_width: f32,
    pub password_trailing_target: f32,
    pub window_bar_height: f32,
}

pub const fn compact_login_metrics() -> CompactLoginMetrics {
    CompactLoginMetrics {
        initial_window_width: 520.0,
        initial_window_height: 600.0,
        minimum_window_width: 480.0,
        minimum_window_height: 520.0,
        content_max_width: 460.0,
        content_inset: 24.0,
        field_height: 52.0,
        username_width: 412.0,
        password_width: 412.0,
        password_trailing_target: 44.0,
        window_bar_height: 44.0,
    }
}

pub const fn floating_label_is_raised(focused: bool, has_text: bool, invalid: bool) -> bool {
    focused || has_text || invalid
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectTriggerInput {
    pub button_clicked: bool,
    pub password_has_focus: bool,
    pub enter_pressed: bool,
    pub enter_is_repeat: bool,
    pub ime_composing: bool,
    pub connection_busy: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectTrigger {
    None,
    Submit,
}

pub const fn connect_trigger(input: ConnectTriggerInput) -> ConnectTrigger {
    if input.connection_busy {
        return ConnectTrigger::None;
    }
    if input.button_clicked
        || (input.password_has_focus
            && input.enter_pressed
            && !input.enter_is_repeat
            && !input.ime_composing)
    {
        ConnectTrigger::Submit
    } else {
        ConnectTrigger::None
    }
}

pub fn show_connection_form(
    ui: &mut Ui,
    form: &mut ConnectionForm,
    catalog: &ProtocolCatalog,
) -> Option<AppIntent> {
    show_connection_form_with_state(ui, form, catalog, false)
}

pub fn show_connection_form_with_state(
    ui: &mut Ui,
    form: &mut ConnectionForm,
    catalog: &ProtocolCatalog,
    connection_busy: bool,
) -> Option<AppIntent> {
    let available_width = ui.available_width();
    let metrics = compact_login_metrics();
    let column_width =
        (available_width - metrics.content_inset * 2.0).clamp(0.0, metrics.content_max_width);
    let content_width = (column_width - metrics.content_inset * 2.0).max(0.0);
    let use_paired_rows = content_width >= COMPACT_LOGIN_PAIR_MIN_WIDTH;
    let original_identity = form.draft.clone();
    let mut intent = None;
    let mut submit_input = None;

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.set_width(column_width);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(metrics.content_inset);
                    ui.vertical(|ui| {
                        ui.set_width(content_width);
                        show_card_header(ui);
                        ui.add_space(12.0);

                        intent = show_recent_profiles(ui, form);
                        show_error(ui, form.errors().profile.as_deref());
                        ui.add_space(12.0);

                        if use_paired_rows {
                            ui.columns(2, |columns| {
                                show_target_selector(&mut columns[0], form, catalog);
                                show_protocol_selector(&mut columns[1], form, catalog);
                            });
                        } else {
                            show_target_selector(ui, form, catalog);
                            ui.add_space(8.0);
                            show_protocol_selector(ui, form, catalog);
                        }
                        ui.add_space(12.0);

                        if use_paired_rows {
                            ui.columns(2, |columns| {
                                show_address_field(&mut columns[0], form);
                                show_port_field(&mut columns[1], form);
                            });
                        } else {
                            show_address_field(ui, form);
                            ui.add_space(8.0);
                            show_port_field(ui, form);
                        }
                        ui.add_space(12.0);

                        show_username_field(ui, form, content_width);
                        form.invalidate_loaded_secret_after_identity_edit(&original_identity);
                        ui.add_space(12.0);
                        let password_input = show_password_field(ui, form, content_width);
                        ui.add_space(12.0);

                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut form.remember_on_this_device,
                                "在此设备上保存登录信息",
                            );
                            show_icon(ui, LoginIcon::ShieldLock, 20.0);
                            ui.label(
                                RichText::new("系统安全凭据库保护密码")
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            );
                        });
                        ui.add_space(16.0);

                        let unsupported = selected_path_is_unsupported(form, catalog);
                        if unsupported {
                            ui.colored_label(ui.visuals().warn_fg_color, "所选目标或协议即将支持");
                            ui.add_space(6.0);
                        }
                        let button_enabled = !connection_busy && !unsupported;
                        let button = ui.add_enabled(
                            button_enabled,
                            Button::new(connect_button_text(ui))
                                .min_size(Vec2::new(content_width, 48.0)),
                        );
                        button.widget_info(|| {
                            WidgetInfo::labeled(WidgetType::Button, button_enabled, "连接")
                        });
                        submit_input = Some(ConnectTriggerInput {
                            button_clicked: button.clicked(),
                            password_has_focus: password_input.password_has_focus,
                            enter_pressed: password_input.enter_pressed,
                            enter_is_repeat: password_input.enter_is_repeat,
                            ime_composing: password_input.ime_composing,
                            connection_busy,
                        });
                    });
                });
            });
            ui.add_space(12.0);
        });

    if intent.is_none()
        && submit_input.is_some_and(|input| connect_trigger(input) == ConnectTrigger::Submit)
    {
        intent = form.take_submission(catalog).map(AppIntent::Connect);
    }
    intent
}

pub fn show_page(ui: &mut Ui, page: &mut AppPage, catalog: &ProtocolCatalog) -> Option<AppIntent> {
    match page {
        AppPage::ConnectionForm(form) => show_connection_form(ui, form, catalog),
        _ => None,
    }
}

fn show_card_header(ui: &mut Ui) {
    ui.vertical_centered(|ui| {
        show_icon(ui, LoginIcon::DesktopWindows, 36.0);
        ui.label(RichText::new("FreeRemoteDesk").heading().strong());
        ui.label(RichText::new("安全连接到远程设备").color(ui.visuals().weak_text_color()));
    });
}

fn show_recent_profiles(ui: &mut Ui, form: &ConnectionForm) -> Option<AppIntent> {
    field_label(ui, "最近连接");
    let original = form.selected_profile.clone();
    let default_profile = (original.is_none()
        && form.draft.target_system.is_none()
        && form.draft.address.is_empty()
        && form.draft.port.is_none()
        && matches!(form.draft.protocol, ProtocolChoice::Automatic)
        && form.draft.username.is_empty()
        && form.password_is_empty())
    .then(|| form.profiles.first().map(|profile| profile.key.clone()))
    .flatten();
    let mut selected = original.clone();
    let selected_text = selected
        .as_ref()
        .and_then(|key| form.profiles.iter().find(|profile| &profile.key == key))
        .map(|profile| {
            format!(
                "{} · {}@{}:{}",
                target_label(Some(profile.target_system)),
                profile.key.username(),
                profile.key.address(),
                profile.key.port()
            )
        })
        .unwrap_or_else(|| {
            if form.profiles.is_empty() {
                "暂无最近连接".to_owned()
            } else {
                "选择保存的连接".to_owned()
            }
        });
    let enabled = !form.profiles.is_empty();
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            ComboBox::from_id_salt("recent-connection-profile")
                .width(ui.available_width())
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for profile in &form.profiles {
                        let label = format!(
                            "{} · {}@{}:{}",
                            target_label(Some(profile.target_system)),
                            profile.key.username(),
                            profile.key.address(),
                            profile.key.port()
                        );
                        ui.selectable_value(&mut selected, Some(profile.key.clone()), label);
                    }
                })
                .response
        })
        .inner;
    response.widget_info(|| WidgetInfo::labeled(WidgetType::ComboBox, enabled, "最近连接"));

    if selected != original {
        selected.map(AppIntent::SelectSavedProfile)
    } else {
        default_profile.map(AppIntent::SelectSavedProfile)
    }
}

fn show_target_selector(ui: &mut Ui, form: &mut ConnectionForm, catalog: &ProtocolCatalog) {
    field_label(ui, "目标系统");
    let protocol_choice = form.draft.protocol.clone();
    let selected = form
        .draft
        .target_system
        .map(|target| target_option_label(target, &protocol_choice, catalog))
        .unwrap_or_else(|| "请选择".to_owned());
    let response = ComboBox::from_id_salt("connection-target")
        .width(ui.available_width())
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for target in [
                TargetSystem::MacOs,
                TargetSystem::Windows,
                TargetSystem::Linux,
                TargetSystem::Custom,
            ] {
                let supported = target_choice_is_supported(catalog, target, &protocol_choice);
                ui.add_enabled_ui(supported, |ui| {
                    ui.selectable_value(
                        &mut form.draft.target_system,
                        Some(target),
                        target_option_label(target, &protocol_choice, catalog),
                    );
                });
            }
        })
        .response;
    response.widget_info(|| WidgetInfo::labeled(WidgetType::ComboBox, true, "目标系统"));
    show_error(ui, form.errors().target_system.as_deref());
}

fn show_protocol_selector(ui: &mut Ui, form: &mut ConnectionForm, catalog: &ProtocolCatalog) {
    field_label(ui, "连接协议");
    let selected = protocol_option_label(&form.draft.protocol, form.draft.target_system, catalog);
    let response = ComboBox::from_id_salt("connection-protocol")
        .width(ui.available_width())
        .selected_text(selected)
        .show_ui(ui, |ui| {
            let automatic_supported = form
                .draft
                .target_system
                .is_none_or(|target| catalog.select(target, ProtocolSelection::Automatic).is_ok());
            ui.add_enabled_ui(automatic_supported, |ui| {
                ui.selectable_value(
                    &mut form.draft.protocol,
                    ProtocolChoice::Automatic,
                    if automatic_supported {
                        "自动选择"
                    } else {
                        "自动选择（即将支持）"
                    },
                );
            });
            for descriptor in catalog.descriptors() {
                let supported = form.draft.target_system.is_none_or(|target| {
                    catalog
                        .select(target, ProtocolSelection::Explicit(descriptor.id.clone()))
                        .is_ok()
                });
                ui.add_enabled_ui(supported, |ui| {
                    let label = if supported {
                        descriptor.display_name.clone()
                    } else {
                        format!("{}（即将支持）", descriptor.display_name)
                    };
                    ui.selectable_value(
                        &mut form.draft.protocol,
                        ProtocolChoice::Explicit(descriptor.id.clone()),
                        label,
                    );
                });
            }
        })
        .response;
    response.widget_info(|| WidgetInfo::labeled(WidgetType::ComboBox, true, "连接协议"));
    show_error(ui, form.errors().protocol.as_deref());
}

fn show_address_field(ui: &mut Ui, form: &mut ConnectionForm) {
    field_label(ui, "地址");
    ui.horizontal(|ui| {
        show_icon(ui, LoginIcon::Dns, 24.0);
        let response = ui.add_sized(
            [ui.available_width(), 36.0],
            TextEdit::singleline(&mut form.draft.address).hint_text("主机名或 IP 地址"),
        );
        response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "地址"));
    });
    show_error(ui, form.errors().address.as_deref());
}

fn show_port_field(ui: &mut Ui, form: &mut ConnectionForm) {
    field_label(ui, "端口");
    let mut port = form.draft.port.unwrap_or(0);
    let response = ui.add_sized(
        [ui.available_width(), 36.0],
        DragValue::new(&mut port).range(0..=u16::MAX).speed(1),
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::DragValue, true, "端口"));
    form.draft.port = (port != 0).then_some(port);
    show_error(ui, form.errors().port.as_deref());
}

fn outlined_field_frame(
    ui: &mut Ui,
    width: f32,
    label: &'static str,
    focused: bool,
    has_text: bool,
    error: Option<&str>,
    leading_slot_width: f32,
    trailing_slot_width: f32,
    add_contents: impl FnOnce(&mut Ui, CompactIdentityFieldLayout) -> Response,
) -> Response {
    let metrics = compact_login_metrics();
    let layout = compact_identity_field_layout(
        width,
        leading_slot_width,
        trailing_slot_width,
        ui.spacing().item_spacing.x,
    );
    let raised = floating_label_is_raised(focused, has_text, error.is_some());
    let field_fill = ui.visuals().extreme_bg_color;
    let (stroke, label_color) = if error.is_some() {
        (ui.visuals().error_fg_color, ui.visuals().error_fg_color)
    } else if focused {
        (
            ui.visuals().widgets.active.bg_stroke.color,
            ui.visuals().widgets.active.fg_stroke.color,
        )
    } else {
        (
            ui.visuals().widgets.inactive.bg_stroke.color,
            ui.visuals().weak_text_color(),
        )
    };
    let stroke = egui::Stroke::new(if focused || error.is_some() { 2.0 } else { 1.0 }, stroke);

    ui.allocate_ui_with_layout(
        Vec2::new(width, metrics.field_height),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(Vec2::new(width, metrics.field_height));
            let frame = Frame::new()
                .fill(field_fill)
                .stroke(stroke)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(OUTLINED_FIELD_HORIZONTAL_INSET as i8, 0));
            let field = frame.show(ui, |ui| {
                ui.set_min_width(layout.content_width);
                ui.set_min_height((metrics.field_height - 2.0).max(0.0));
                add_contents(ui, layout)
            });

            let label_font = FontId::proportional(if raised { 12.0 } else { 14.0 });
            let label_galley =
                ui.painter()
                    .layout_no_wrap(label.to_owned(), label_font, label_color);
            let label_x = field.response.rect.left()
                + OUTLINED_FIELD_HORIZONTAL_INSET
                + if raised {
                    0.0
                } else {
                    leading_slot_width + ui.spacing().item_spacing.x
                };
            let label_y = if raised {
                field.response.rect.top()
            } else {
                field.response.rect.center().y
            };
            let label_position = egui::pos2(label_x, label_y - label_galley.size().y / 2.0);
            if raised {
                let label_mask = egui::Rect::from_center_size(
                    egui::pos2(label_position.x + label_galley.size().x / 2.0, label_y),
                    label_galley.size() + Vec2::new(8.0, 2.0),
                );
                ui.painter()
                    .rect_filled(label_mask, CornerRadius::ZERO, field_fill);
            }
            ui.painter()
                .galley(label_position, label_galley, label_color);
            field.inner
        },
    )
    .inner
}

fn show_username_field(ui: &mut Ui, form: &mut ConnectionForm, width: f32) {
    let username_id = ui.make_persistent_id("connection-username");
    let focused = ui.memory(|memory| memory.has_focus(username_id));
    let error = form.errors().username.clone();
    let response = outlined_field_frame(
        ui,
        width,
        "用户名",
        focused,
        !form.draft.username.is_empty(),
        error.as_deref(),
        24.0,
        0.0,
        |ui, layout| {
            ui.horizontal(|ui| {
                show_icon(ui, LoginIcon::Person, 24.0);
                ui.add_sized(
                    [layout.text_width, 36.0],
                    TextEdit::singleline(&mut form.draft.username)
                        .id(username_id)
                        .frame(Frame::NONE),
                )
            })
            .inner
        },
    );
    install_identity_field_accessibility(
        ui,
        &response,
        "用户名",
        "输入远程设备账户用户名",
        error.as_deref(),
    );
    show_error(ui, error.as_deref());
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PasswordInput {
    password_has_focus: bool,
    enter_pressed: bool,
    enter_is_repeat: bool,
    ime_composing: bool,
}

fn show_password_field(ui: &mut Ui, form: &mut ConnectionForm, width: f32) -> PasswordInput {
    let password_id = ui.make_persistent_id("connection-password");
    let had_focus = ui.memory(|memory| memory.has_focus(password_id));
    let visible = form.password_visible;
    let has_text = !form.password_mut().is_empty();
    let error = form.errors().password.clone();
    let mut visibility_clicked = false;
    let response = outlined_field_frame(
        ui,
        width,
        "密码",
        had_focus,
        has_text,
        error.as_deref(),
        24.0,
        compact_login_metrics().password_trailing_target,
        |ui, layout| {
            ui.horizontal(|ui| {
                show_icon(ui, LoginIcon::Lock, 24.0);
                let response = {
                    let mut password = SecretTextBuffer(form.password_mut());
                    ui.add_sized(
                        [layout.text_width, 36.0],
                        TextEdit::singleline(&mut password)
                            .id(password_id)
                            .password(!visible)
                            .frame(Frame::NONE),
                    )
                };
                install_identity_field_accessibility(
                    ui,
                    &response,
                    "密码",
                    "输入远程设备密码；内容默认隐藏",
                    error.as_deref(),
                );
                let visibility_icon = if visible {
                    LoginIcon::VisibilityOff
                } else {
                    LoginIcon::Visibility
                };
                ui.allocate_ui_with_layout(
                    Vec2::new(layout.trailing_width, layout.trailing_height),
                    Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| {
                        let visibility = icon_button(ui, visibility_icon, visible);
                        visibility_clicked = visibility.clicked();
                    },
                );
                response
            })
            .inner
        },
    );
    if visibility_clicked {
        form.password_visible = !visible;
        response.request_focus();
    }
    show_error(ui, error.as_deref());

    password_input(ui, &response, had_focus || response.has_focus())
}

fn password_input(ui: &Ui, response: &Response, password_has_focus: bool) -> PasswordInput {
    let composition_id = response.id.with("ime-composing");
    let mut composition_active = ui
        .ctx()
        .data_mut(|data| data.get_persisted::<bool>(composition_id).unwrap_or(false));
    let mut composition_seen = false;
    let mut enter_pressed = false;
    let mut enter_is_repeat = false;

    if password_has_focus {
        ui.input(|input| {
            for event in &input.events {
                match event {
                    Event::Key {
                        key: Key::Enter,
                        pressed: true,
                        repeat,
                        ..
                    } => {
                        if *repeat {
                            enter_is_repeat = true;
                        } else {
                            enter_pressed = true;
                        }
                    }
                    Event::Ime(ImeEvent::Preedit { text, .. }) => {
                        composition_active = !text.is_empty();
                        composition_seen |= !text.is_empty();
                    }
                    Event::Ime(ImeEvent::Commit(_)) => {
                        composition_seen |= composition_active;
                        composition_active = false;
                    }
                    _ => {}
                }
            }
        });
    } else {
        composition_active = false;
    }
    ui.ctx()
        .data_mut(|data| data.insert_persisted(composition_id, composition_active));

    PasswordInput {
        password_has_focus,
        enter_pressed,
        enter_is_repeat,
        ime_composing: composition_active || composition_seen,
    }
}

fn field_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).strong());
}

fn install_identity_field_accessibility(
    ui: &Ui,
    response: &Response,
    name: &'static str,
    usage_description: &'static str,
    error: Option<&str>,
) {
    response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, name));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_label(name);
        if let Some(error) = error {
            node.set_invalid(egui::accesskit::Invalid::True);
            node.set_description(connection_error_message(error));
        } else {
            node.clear_invalid();
            node.set_description(usage_description);
        }
    });
}

pub(crate) fn target_choice_is_supported(
    catalog: &ProtocolCatalog,
    target: TargetSystem,
    protocol: &ProtocolChoice,
) -> bool {
    match protocol {
        ProtocolChoice::Automatic => catalog.descriptors().iter().any(|descriptor| {
            catalog
                .select(target, ProtocolSelection::Explicit(descriptor.id.clone()))
                .is_ok()
        }),
        ProtocolChoice::Explicit(protocol) => catalog
            .select(target, ProtocolSelection::Explicit(protocol.clone()))
            .is_ok(),
    }
}

fn target_option_label(
    target: TargetSystem,
    protocol: &ProtocolChoice,
    catalog: &ProtocolCatalog,
) -> String {
    let label = target_label(Some(target));
    if target_choice_is_supported(catalog, target, protocol) {
        label.to_owned()
    } else {
        format!("{label}（即将支持）")
    }
}

pub(crate) fn protocol_option_label(
    choice: &ProtocolChoice,
    target: Option<TargetSystem>,
    catalog: &ProtocolCatalog,
) -> String {
    let (label, selection) = match choice {
        ProtocolChoice::Automatic => ("自动选择".to_owned(), ProtocolSelection::Automatic),
        ProtocolChoice::Explicit(protocol) => (
            catalog.descriptor(protocol).map_or_else(
                || protocol.as_str().to_owned(),
                |item| item.display_name.clone(),
            ),
            ProtocolSelection::Explicit(protocol.clone()),
        ),
    };
    if target.is_none_or(|target| catalog.select(target, selection).is_ok()) {
        label
    } else {
        format!("{label}（即将支持）")
    }
}

fn selected_path_is_unsupported(form: &ConnectionForm, catalog: &ProtocolCatalog) -> bool {
    form.draft.target_system.is_some_and(|target| {
        let selection = match &form.draft.protocol {
            ProtocolChoice::Automatic => ProtocolSelection::Automatic,
            ProtocolChoice::Explicit(protocol) => ProtocolSelection::Explicit(protocol.clone()),
        };
        catalog.select(target, selection).is_err()
    })
}

fn connect_button_text(ui: &Ui) -> LayoutJob {
    let mut job = LayoutJob::default();
    let color = ui.visuals().strong_text_color();
    job.append(
        &LoginIcon::Login.semantic().codepoint.to_string(),
        0.0,
        TextFormat {
            font_id: FontId::new(
                24.0,
                FontFamily::Name(LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY.into()),
            ),
            color,
            ..Default::default()
        },
    );
    job.append(
        "  连接",
        0.0,
        TextFormat {
            font_id: FontId::new(16.0, FontFamily::Proportional),
            color,
            ..Default::default()
        },
    );
    job
}

fn target_label(target: Option<TargetSystem>) -> &'static str {
    match target {
        Some(TargetSystem::MacOs) => "Mac OS",
        Some(TargetSystem::Windows) => "Windows",
        Some(TargetSystem::Linux) => "Linux",
        Some(TargetSystem::Custom) => "自定义",
        None => "请选择",
    }
}

fn show_error(ui: &mut Ui, error: Option<&str>) {
    if let Some(error) = error {
        ui.colored_label(ui.visuals().error_fg_color, connection_error_message(error));
    }
}

fn connection_error_message(error: &str) -> &'static str {
    match error {
        "target_system_required" => "请选择目标系统",
        "address_required" => "请输入地址",
        "port_required" => "请输入有效端口",
        "username_required" => "请输入用户名",
        "password_required" => "请输入密码",
        "credential_provider_failed" => "无法从凭据提供器读取该字段",
        "profile_storage_unavailable" => "系统凭据库当前不可用",
        "profile_storage_failed" => "无法读取最近连接，请稍后重试",
        "invalid_profile" => "保存的连接信息无效",
        "saved_profile_not_found" => "保存的连接已不存在",
        "credential_storage_failed" => "无法安全保存密码，请重试或取消保存",
        "saved_credential_unavailable" => "无法读取保存的密码，请重新输入",
        "unsupported_target_protocol" => "目标系统与协议不匹配",
        "unregistered_protocol" => "所选协议未在本产品中注册",
        _ => "输入无效",
    }
}

struct SecretTextBuffer<'a>(&'a mut SecretBuffer);

impl TextBuffer for SecretTextBuffer<'_> {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.0.expose_text().unwrap_or("")
    }

    fn insert_text(&mut self, text: &str, char_index: CharIndex) -> usize {
        if self.0.insert_text_at_char(char_index.0, text) {
            text.chars().count()
        } else {
            0
        }
    }

    fn delete_char_range(&mut self, char_range: Range<CharIndex>) {
        self.0
            .delete_text_char_range(char_range.start.0..char_range.end.0);
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<SecretTextBuffer<'static>>()
    }
}

#[cfg(test)]
mod accessibility_tests {
    use egui::{Context, RawInput, Rect, Vec2};
    use frd_ui_model::{ConnectionDraft, ConnectionForm};

    use super::{show_password_field, show_username_field};

    #[derive(Debug, Eq, PartialEq)]
    struct FieldAccessibility {
        name: String,
        description: Option<String>,
        invalid: Option<egui::accesskit::Invalid>,
    }

    fn accessibility_context() -> Context {
        let context = Context::default();
        context.enable_accesskit();
        let mut fonts = egui::FontDefinitions::default();
        crate::login_icons::install_login_icons_font(&mut fonts);
        context.set_fonts(fonts);
        context
    }

    fn render_identity_fields(
        context: &Context,
        form: &mut ConnectionForm,
    ) -> Vec<FieldAccessibility> {
        let mut output = context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(520.0, 240.0),
                )),
                ..Default::default()
            },
            |ui| {
                show_username_field(ui, form, 412.0);
                ui.add_space(12.0);
                let _ = show_password_field(ui, form, 412.0);
            },
        );
        let fields = output
            .platform_output
            .accesskit_update
            .as_ref()
            .map(|update| {
                update
                    .nodes
                    .iter()
                    .filter_map(|(_, node)| {
                        let name = node.label().or_else(|| node.value())?;
                        matches!(name, "用户名" | "密码").then(|| FieldAccessibility {
                            name: name.to_owned(),
                            description: node.description().map(str::to_owned),
                            invalid: node.invalid(),
                        })
                    })
                    .collect::<Vec<_>>()
            });
        output.textures_delta.clear();
        let mut fields = fields.expect("accessibility tree is enabled");
        fields.sort_by(|left, right| left.name.cmp(&right.name));
        fields
    }

    #[test]
    fn identity_text_edits_expose_error_descriptions_and_invalid_state() {
        let context = accessibility_context();
        let mut form = ConnectionForm::new(ConnectionDraft::default());
        form.set_username_error("username_required");
        form.set_password_error("password_required");

        let fields = render_identity_fields(&context, &mut form);

        assert_eq!(
            fields,
            vec![
                FieldAccessibility {
                    name: "密码".to_owned(),
                    description: Some("请输入密码".to_owned()),
                    invalid: Some(egui::accesskit::Invalid::True),
                },
                FieldAccessibility {
                    name: "用户名".to_owned(),
                    description: Some("请输入用户名".to_owned()),
                    invalid: Some(egui::accesskit::Invalid::True),
                },
            ]
        );
    }

    #[test]
    fn identity_text_edits_clear_invalid_and_keep_persistent_usage_descriptions() {
        let context = accessibility_context();
        let mut invalid = ConnectionForm::new(ConnectionDraft::default());
        invalid.set_username_error("username_required");
        invalid.set_password_error("password_required");
        let _ = render_identity_fields(&context, &mut invalid);

        let mut clean = ConnectionForm::new(ConnectionDraft::default());
        let fields = render_identity_fields(&context, &mut clean);

        assert_eq!(
            fields,
            vec![
                FieldAccessibility {
                    name: "密码".to_owned(),
                    description: Some("输入远程设备密码；内容默认隐藏".to_owned()),
                    invalid: None,
                },
                FieldAccessibility {
                    name: "用户名".to_owned(),
                    description: Some("输入远程设备账户用户名".to_owned()),
                    invalid: None,
                },
            ]
        );
    }
}
