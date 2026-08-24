use freeremotedesk::app::connection::{ProtocolKind, ServiceKind};
use freeremotedesk::ui::{
    ConnectionFormState, FreeRemoteApplication, SubmissionOutcome, UiAction, UiPage,
};

#[test]
fn terminal_submission_clears_password_without_clearing_endpoint() {
    let mut form = ConnectionFormState::fixture();
    form.password_mut().push_str("secret-value");

    form.finish_submission(SubmissionOutcome::Failed);

    assert!(form.password().is_empty());
    assert_eq!(form.host(), "mac.local");
}

#[test]
fn debug_output_never_contains_password() {
    let form = ConnectionFormState::with_password("secret-value");

    assert!(!format!("{form:?}").contains("secret-value"));
}

#[test]
fn domain_is_visible_only_for_windows() {
    let mut form = ConnectionFormState::fixture();

    for service in [
        ServiceKind::Auto,
        ServiceKind::MacOsArd,
        ServiceKind::LinuxVnc,
    ] {
        form.set_service(service);
        assert!(!form.domain_visible());
    }
    form.set_service(ServiceKind::WindowsRdp);
    assert!(form.domain_visible());
}

#[test]
fn submitting_moves_a_redacted_connection_into_a_ui_action() {
    let mut app = FreeRemoteApplication::fixture();
    app.connection_form_mut()
        .password_mut()
        .push_str("secret-value");

    let action = app.submit_connection().unwrap();

    let UiAction::Connect(connection) = action else {
        panic!("expected connect action");
    };
    assert_eq!(connection.protocol, ProtocolKind::AppleRfb);
    assert!(!format!("{connection:?}").contains("secret-value"));
    assert!(app.connection_form().password().is_empty());
    assert_eq!(app.page(), UiPage::Connecting);
}
