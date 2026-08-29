use egui::Ui;
use frd_app::{AppIntent, AppPage};
use frd_protocol_api::{
    ServerIdentityChallenge, ServerIdentityDecision, ServerIdentityValidationFailure,
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
        AppPage::Connecting { .. }
        | AppPage::AwaitingFirstFrame { .. }
        | AppPage::Disconnecting { .. }
        | AppPage::RemoteSession { .. } => None,
        AppPage::Failed { code, .. } => {
            ui.heading("连接失败");
            ui.label(format!("错误代码：{code}"));
            ui.button("返回连接页")
                .clicked()
                .then_some(AppIntent::ReturnToConnection)
        }
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
    use frd_protocol_api::{evaluate_server_identity, ServerIdentityValidationFailure};

    use super::validation_failure_labels;

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
