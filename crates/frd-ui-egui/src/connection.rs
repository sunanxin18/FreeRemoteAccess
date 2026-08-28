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

const LOGIN_CARD_MAX_WIDTH: f32 = 460.0;
const LOGIN_CARD_OUTER_MARGIN: f32 = 16.0;
const LOGIN_CARD_PAIR_BREAKPOINT: f32 = 520.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoginCardMetrics {
    pub card_width: f32,
    pub use_paired_rows: bool,
    pub corner_radius: f32,
    pub inner_margin: f32,
    pub primary_button_height: f32,
}

pub fn login_card_metrics(available_width: f32) -> LoginCardMetrics {
    LoginCardMetrics {
        card_width: (available_width - LOGIN_CARD_OUTER_MARGIN * 2.0)
            .clamp(0.0, LOGIN_CARD_MAX_WIDTH),
        use_paired_rows: available_width >= LOGIN_CARD_PAIR_BREAKPOINT,
        corner_radius: 16.0,
        inner_margin: 24.0,
        primary_button_height: 48.0,
    }
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
    let available_height = ui.available_height();
    let metrics = login_card_metrics(available_width);
    let estimated_height = if metrics.use_paired_rows {
        560.0
    } else {
        680.0
    };
    let top_space = ((available_height - estimated_height) / 2.0).max(16.0);
    let mut intent = None;
    let mut submit_input = None;

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(top_space);
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.set_width(metrics.card_width);
                Frame::new()
                    .fill(ui.visuals().window_fill())
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                    .shadow(ui.visuals().window_shadow)
                    .corner_radius(CornerRadius::same(metrics.corner_radius as u8))
                    .inner_margin(Margin::same(metrics.inner_margin as i8))
                    .show(ui, |ui| {
                        ui.set_width((metrics.card_width - metrics.inner_margin * 2.0).max(0.0));
                        show_card_header(ui);
                        ui.add_space(20.0);

                        intent = show_recent_profiles(ui, form);
                        show_error(ui, form.errors().profile.as_deref());
                        ui.add_space(12.0);

                        if metrics.use_paired_rows {
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

                        if metrics.use_paired_rows {
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

                        show_username_field(ui, form);
                        ui.add_space(12.0);
                        let password_input = show_password_field(ui, form);
                        ui.add_space(12.0);

                        ui.checkbox(&mut form.remember_on_this_device, "在此设备上保存登录信息");
                        ui.horizontal(|ui| {
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
                            Button::new(connect_button_text(ui)).min_size(Vec2::new(
                                ui.available_width(),
                                metrics.primary_button_height,
                            )),
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
            ui.add_space(16.0);
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
    let selected = form
        .draft
        .target_system
        .map(|target| target_option_label(target, catalog))
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
                let supported = target_is_supported(catalog, target);
                ui.add_enabled_ui(supported, |ui| {
                    ui.selectable_value(
                        &mut form.draft.target_system,
                        Some(target),
                        target_option_label(target, catalog),
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

fn show_username_field(ui: &mut Ui, form: &mut ConnectionForm) {
    field_label(ui, "用户名");
    ui.horizontal(|ui| {
        show_icon(ui, LoginIcon::Person, 24.0);
        let response = ui.add_sized(
            [ui.available_width(), 36.0],
            TextEdit::singleline(&mut form.draft.username).hint_text("远程设备账户"),
        );
        response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "用户名"));
    });
    show_error(ui, form.errors().username.as_deref());
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PasswordInput {
    password_has_focus: bool,
    enter_pressed: bool,
    enter_is_repeat: bool,
    ime_composing: bool,
}

fn show_password_field(ui: &mut Ui, form: &mut ConnectionForm) -> PasswordInput {
    field_label(ui, "密码");
    let password_id = ui.make_persistent_id("connection-password");
    let had_focus = ui.memory(|memory| memory.has_focus(password_id));
    let visible = form.password_visible;
    let (response, visibility_clicked) = ui
        .horizontal(|ui| {
            show_icon(ui, LoginIcon::Lock, 24.0);
            let text_width = (ui.available_width() - 48.0).max(40.0);
            let response = {
                let mut password = SecretTextBuffer(form.password_mut());
                ui.add_sized(
                    [text_width, 36.0],
                    TextEdit::singleline(&mut password)
                        .id(password_id)
                        .password(!visible)
                        .hint_text("远程设备密码"),
                )
            };
            response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, "密码"));
            let visibility_icon = if visible {
                LoginIcon::VisibilityOff
            } else {
                LoginIcon::Visibility
            };
            let visibility = icon_button(ui, visibility_icon, visible);
            (response, visibility.clicked())
        })
        .inner;
    if visibility_clicked {
        form.password_visible = !visible;
    }
    show_error(ui, form.errors().password.as_deref());

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

fn target_is_supported(catalog: &ProtocolCatalog, target: TargetSystem) -> bool {
    catalog.descriptors().iter().any(|descriptor| {
        catalog
            .select(target, ProtocolSelection::Explicit(descriptor.id.clone()))
            .is_ok()
    })
}

fn target_option_label(target: TargetSystem, catalog: &ProtocolCatalog) -> String {
    let label = target_label(Some(target));
    if target_is_supported(catalog, target) {
        label.to_owned()
    } else {
        format!("{label}（即将支持）")
    }
}

fn protocol_option_label(
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
        let message = match error {
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
        };
        ui.colored_label(ui.visuals().error_fg_color, message);
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
