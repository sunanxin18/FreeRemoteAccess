use std::ops::Range;

use egui::epaint::text::CharIndex;
use egui::{ComboBox, DragValue, TextBuffer, TextEdit, Ui};
use frd_app::{AppIntent, AppPage};
use frd_core::{SecretBuffer, TargetSystem};
use frd_protocol_api::ProtocolCatalog;
use frd_ui_model::{ConnectionForm, ProtocolChoice};

pub fn show_connection_form(
    ui: &mut Ui,
    form: &mut ConnectionForm,
    catalog: &ProtocolCatalog,
) -> Option<AppIntent> {
    ui.heading("连接远程桌面");

    ComboBox::from_label("目标系统")
        .selected_text(target_label(form.draft.target_system))
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut form.draft.target_system,
                Some(TargetSystem::MacOs),
                "Mac OS",
            );
            ui.selectable_value(
                &mut form.draft.target_system,
                Some(TargetSystem::Windows),
                "Windows",
            );
            ui.selectable_value(
                &mut form.draft.target_system,
                Some(TargetSystem::Linux),
                "Linux",
            );
            ui.selectable_value(
                &mut form.draft.target_system,
                Some(TargetSystem::Custom),
                "自定义",
            );
        });
    show_error(ui, form.errors().target_system.as_deref());

    ui.horizontal(|ui| {
        ui.label("地址");
        ui.text_edit_singleline(&mut form.draft.address);
    });
    show_error(ui, form.errors().address.as_deref());

    let mut port = form.draft.port.unwrap_or(0);
    ui.horizontal(|ui| {
        ui.label("端口");
        ui.add(DragValue::new(&mut port).range(0..=u16::MAX));
    });
    form.draft.port = (port != 0).then_some(port);
    show_error(ui, form.errors().port.as_deref());

    ComboBox::from_label("协议")
        .selected_text(protocol_label(&form.draft.protocol))
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut form.draft.protocol,
                ProtocolChoice::Automatic,
                "自动选择",
            );
            for descriptor in catalog.descriptors() {
                ui.selectable_value(
                    &mut form.draft.protocol,
                    ProtocolChoice::Explicit(descriptor.id.clone()),
                    &descriptor.display_name,
                );
            }
        });
    show_error(ui, form.errors().protocol.as_deref());

    ui.horizontal(|ui| {
        ui.label("用户名");
        ui.text_edit_singleline(&mut form.draft.username);
    });
    show_error(ui, form.errors().username.as_deref());

    ui.horizontal(|ui| {
        ui.label("密码");
        let mut password = SecretTextBuffer(form.password_mut());
        ui.add(TextEdit::singleline(&mut password).password(true));
    });
    show_error(ui, form.errors().password.as_deref());

    ui.button("连接")
        .clicked()
        .then(|| form.take_submission(catalog))
        .flatten()
        .map(AppIntent::Connect)
}

pub fn show_page(ui: &mut Ui, page: &mut AppPage, catalog: &ProtocolCatalog) -> Option<AppIntent> {
    match page {
        AppPage::ConnectionForm(form) => show_connection_form(ui, form, catalog),
        _ => None,
    }
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

fn protocol_label(protocol: &ProtocolChoice) -> &str {
    match protocol {
        ProtocolChoice::Automatic => "自动选择",
        ProtocolChoice::Explicit(protocol) => protocol.as_str(),
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
