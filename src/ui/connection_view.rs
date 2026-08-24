use crate::app::connection::ServiceKind;

use super::ConnectionFormState;

pub fn show(ui: &mut egui::Ui, form: &mut ConnectionFormState) -> bool {
    ui.heading("FreeRemoteAccess");
    ui.label("连接原生远程桌面服务");

    egui::ComboBox::from_label("服务端")
        .selected_text(service_label(form.service()))
        .show_ui(ui, |ui| {
            for service in [
                ServiceKind::Auto,
                ServiceKind::WindowsRdp,
                ServiceKind::MacOsArd,
                ServiceKind::LinuxVnc,
            ] {
                let selected = form.service() == service;
                if ui
                    .selectable_label(selected, service_label(service))
                    .clicked()
                {
                    form.set_service(service);
                }
            }
        });
    ui.horizontal(|ui| {
        ui.label("地址");
        ui.text_edit_singleline(form.host_mut());
    });
    ui.horizontal(|ui| {
        ui.label("端口（留空自动）");
        ui.text_edit_singleline(form.port_mut());
    });
    ui.horizontal(|ui| {
        ui.label("用户名");
        ui.text_edit_singleline(form.username_mut());
    });
    if form.domain_visible() {
        ui.horizontal(|ui| {
            ui.label("域");
            ui.text_edit_singleline(form.domain_mut());
        });
    }
    ui.horizontal(|ui| {
        ui.label("密码");
        ui.add(egui::TextEdit::singleline(form.password_mut().edit_string()).password(true));
    });
    ui.button("连接").clicked()
}

fn service_label(service: ServiceKind) -> &'static str {
    match service {
        ServiceKind::Auto => "自动识别",
        ServiceKind::WindowsRdp => "Windows",
        ServiceKind::MacOsArd => "Mac OS",
        ServiceKind::LinuxVnc => "Linux / VNC",
    }
}
