use crate::session::{SessionPhase, SessionSnapshot};

pub fn show_toolbar(ui: &mut egui::Ui, snapshot: SessionSnapshot) -> bool {
    let status = match snapshot.phase() {
        SessionPhase::Idle => "未连接",
        SessionPhase::Connecting => "正在连接",
        SessionPhase::SurfaceReady => "正在准备画面",
        SessionPhase::Connected => "已连接",
        SessionPhase::Disconnecting => "正在断开",
        SessionPhase::Failed => "连接失败",
    };
    ui.horizontal(|ui| {
        ui.label(status);
        ui.button("断开连接").clicked()
    })
    .inner
}
