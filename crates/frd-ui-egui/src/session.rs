use egui::Ui;
use frd_app::{AppIntent, AppPage};
use frd_protocol_api::{
    ConnectionStage, ServerIdentityChallenge, ServerIdentityDecision,
    ServerIdentityValidationFailure,
};

pub fn show_session_page(
    ui: &mut Ui,
    page: &AppPage,
    challenge: Option<&ServerIdentityChallenge>,
) -> Option<AppIntent> {
    if let Some(challenge) = challenge {
        return show_identity_challenge(ui, challenge);
    }

    match page {
        AppPage::ConnectionForm(_) => None,
        AppPage::Connecting {
            stage, diagnostics, ..
        }
        | AppPage::AwaitingFirstFrame {
            stage, diagnostics, ..
        } => show_pending_connection(ui, stage, diagnostics.as_deref()),
        AppPage::Disconnecting { .. } | AppPage::RemoteSession { .. } => None,
        AppPage::Failed { code, .. } => {
            ui.heading("连接失败");
            ui.label(format!("错误代码：{code}"));
            ui.button("返回连接页")
                .clicked()
                .then_some(AppIntent::ReturnToConnection)
        }
    }
}

fn show_pending_connection(
    ui: &mut Ui,
    stage: &ConnectionStage,
    diagnostics: Option<&str>,
) -> Option<AppIntent> {
    ui.vertical_centered(|ui| {
        ui.heading("正在连接");
        ui.label(connection_stage_label(stage));
        if let Some(diagnostics) = diagnostics {
            ui.colored_label(ui.visuals().warn_fg_color, diagnostics);
        }
        ui.add_space(12.0);
        ui.button("取消连接")
            .clicked()
            .then_some(AppIntent::CancelConnect)
    })
    .inner
}

fn connection_stage_label(stage: &ConnectionStage) -> &'static str {
    match stage {
        ConnectionStage::Connecting => "正在建立安全连接…",
        ConnectionStage::TransportReady => "安全连接已建立，正在等待远程画面…",
        ConnectionStage::AwaitingIdentityDecision => "正在等待服务器身份确认…",
        ConnectionStage::Disconnecting => "正在结束连接…",
    }
}

fn show_identity_challenge(ui: &mut Ui, challenge: &ServerIdentityChallenge) -> Option<AppIntent> {
    ui.heading("确认服务器身份");
    ui.label(format!("端点：{}", challenge.endpoint));
    ui.label(format!("主题：{}", challenge.subject));
    ui.label(format!("签发者：{}", challenge.issuer));
    if let Some(failure) = challenge.validation.failure() {
        let [code, reason] = validation_failure_labels(failure);
        ui.colored_label(ui.visuals().warn_fg_color, code);
        ui.colored_label(ui.visuals().warn_fg_color, reason);
    }
    ui.label(format!(
        "SHA-256：{}",
        challenge
            .sha256_fingerprint
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    ));
    if challenge.validation.is_pin_mismatch() {
        ui.colored_label(ui.visuals().error_fg_color, "已保存的服务器指纹不匹配");
        return ui
            .button("拒绝")
            .clicked()
            .then(|| identity_intent(challenge, ServerIdentityDecision::Reject));
    }

    let mut intent = None;
    ui.horizontal(|ui| {
        if ui.button("仅本次信任").clicked() {
            intent = Some(identity_intent(
                challenge,
                ServerIdentityDecision::TrustOnce,
            ));
        }
        if ui.button("信任并记住").clicked() {
            intent = Some(identity_intent(
                challenge,
                ServerIdentityDecision::TrustAndRemember,
            ));
        }
        if ui.button("拒绝").clicked() {
            intent = Some(identity_intent(challenge, ServerIdentityDecision::Reject));
        }
    });
    intent
}

fn validation_failure_labels(failure: &ServerIdentityValidationFailure) -> [String; 2] {
    [
        format!("验证代码：{}", failure.code()),
        format!("验证原因：{}", failure.reason()),
    ]
}

fn identity_intent(
    challenge: &ServerIdentityChallenge,
    decision: ServerIdentityDecision,
) -> AppIntent {
    AppIntent::ResolveServerIdentity {
        session_id: challenge.session_id,
        challenge_id: challenge.challenge_id,
        decision,
    }
}

#[cfg(test)]
mod tests {
    use egui::{Context, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
    use frd_app::{AppIntent, AppPage};
    use frd_protocol_api::{
        evaluate_server_identity, ConnectionStage, ServerIdentityValidationFailure,
    };
    use frd_ui_model::ConnectionDraft;

    use super::{show_session_page, validation_failure_labels};

    #[test]
    fn pending_session_page_cancel_button_emits_cancel_connect() {
        let context = Context::default();
        context.enable_accesskit();
        let page = AppPage::Connecting {
            draft: ConnectionDraft::default(),
            stage: ConnectionStage::Connecting,
            diagnostics: None,
        };
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(520.0, 556.0));
        let mut initial_intent = None;
        let mut initial = context.run_ui(
            RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| initial_intent = show_session_page(ui, &page, None),
        );
        let cancel_bounds = initial
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("accessibility tree is enabled")
            .nodes
            .iter()
            .find_map(|(_, node)| {
                (node.label().or_else(|| node.value()) == Some("取消连接"))
                    .then(|| node.bounds())
                    .flatten()
            });
        initial.textures_delta.clear();
        let cancel_bounds = cancel_bounds.expect("pending page exposes the cancel button");
        assert!(initial_intent.is_none());
        let pointer = egui::pos2(
            ((cancel_bounds.x0 + cancel_bounds.x1) / 2.0) as f32,
            ((cancel_bounds.y0 + cancel_bounds.y1) / 2.0) as f32,
        );

        let mut pressed = context.run_ui(
            RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    Event::PointerMoved(pointer),
                    Event::PointerButton {
                        pos: pointer,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ui| {
                let _ = show_session_page(ui, &page, None);
            },
        );
        pressed.textures_delta.clear();

        let mut released_intent = None;
        let mut released = context.run_ui(
            RawInput {
                screen_rect: Some(screen_rect),
                events: vec![Event::PointerButton {
                    pos: pointer,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| released_intent = show_session_page(ui, &page, None),
        );
        released.textures_delta.clear();

        assert!(matches!(released_intent, Some(AppIntent::CancelConnect)));
    }

    #[test]
    fn identity_page_labels_include_sanitized_validation_code_and_reason() {
        let failure =
            ServerIdentityValidationFailure::new("certificate.expired", "服务器证书已过期")
                .expect("sanitized failure");

        assert_eq!(
            validation_failure_labels(&failure),
            [
                "验证代码：certificate.expired".to_owned(),
                "验证原因：服务器证书已过期".to_owned(),
            ]
        );
    }

    #[test]
    fn unknown_identity_page_always_has_validation_labels() {
        let validation = evaluate_server_identity(None, [0x22; 32]);
        let failure = validation
            .failure()
            .expect("unknown validation has a required reason");

        let [code, reason] = validation_failure_labels(failure);
        assert!(code.starts_with("验证代码：identity."));
        assert!(reason.starts_with("验证原因："));
        assert!(reason.len() > "验证原因：".len());
    }
}
