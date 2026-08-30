mod connection;
mod login_icons;
mod session;
mod session_chrome;

pub use connection::{show_connection_form, show_connection_form_with_state, show_page};
pub use login_icons::{
    install_login_icons_font, LoginIcon, LoginIconSemantic, LOGIN_ICON_BUTTON_SIZE,
    LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY,
};
pub use session::show_session_page;
pub use session_chrome::{
    install_session_chrome_font, session_chrome_metrics, show_session_chrome,
    show_session_chrome_with_focus, SessionChromeMetrics, SessionChromeRenderResult,
    MATERIAL_SYMBOLS_FONT_FAMILY,
};

#[cfg(test)]
#[test]
fn password_enter_submits_once() {
    assert_eq!(
        connection::connect_trigger(connection::ConnectTriggerInput {
            button_clicked: false,
            password_has_focus: true,
            enter_pressed: true,
            enter_is_repeat: false,
            ime_composing: false,
            connection_busy: false,
        }),
        connection::ConnectTrigger::Submit,
    );
}

#[cfg(test)]
#[test]
fn password_enter_ignores_ime_composition() {
    assert_eq!(
        connection::connect_trigger(connection::ConnectTriggerInput {
            button_clicked: false,
            password_has_focus: true,
            enter_pressed: true,
            enter_is_repeat: false,
            ime_composing: true,
            connection_busy: false,
        }),
        connection::ConnectTrigger::None,
    );
}

#[cfg(test)]
#[test]
fn password_enter_ignores_auto_repeat() {
    assert_eq!(
        connection::connect_trigger(connection::ConnectTriggerInput {
            button_clicked: false,
            password_has_focus: true,
            enter_pressed: true,
            enter_is_repeat: true,
            ime_composing: false,
            connection_busy: false,
        }),
        connection::ConnectTrigger::None,
    );
}

#[cfg(test)]
#[test]
fn connect_trigger_ignores_actions_while_busy() {
    assert_eq!(
        connection::connect_trigger(connection::ConnectTriggerInput {
            button_clicked: true,
            password_has_focus: true,
            enter_pressed: true,
            enter_is_repeat: false,
            ime_composing: false,
            connection_busy: true,
        }),
        connection::ConnectTrigger::None,
    );
}

#[cfg(test)]
#[test]
fn login_icons_use_official_names_codepoints_and_chinese_help() {
    use login_icons::LoginIcon;

    let expected = [
        (LoginIcon::DesktopWindows, "desktop_windows", '\u{e30c}'),
        (LoginIcon::Dns, "dns", '\u{e875}'),
        (LoginIcon::Person, "person", '\u{f0d3}'),
        (LoginIcon::Lock, "lock", '\u{e899}'),
        (LoginIcon::Visibility, "visibility", '\u{e8f4}'),
        (LoginIcon::VisibilityOff, "visibility_off", '\u{e8f5}'),
        (LoginIcon::ExpandMore, "expand_more", '\u{e5cf}'),
        (LoginIcon::Login, "login", '\u{ea77}'),
        (LoginIcon::ShieldLock, "shield_lock", '\u{f686}'),
        (LoginIcon::Delete, "delete", '\u{e92e}'),
        (LoginIcon::CheckCircle, "check_circle", '\u{f0be}'),
    ];

    for (icon, symbol_name, codepoint) in expected {
        let semantic = icon.semantic();
        assert_eq!(semantic.symbol_name, symbol_name);
        assert_eq!(semantic.codepoint, codepoint);
        assert!(!semantic.tooltip.is_empty());
        assert!(!semantic.accessible_name.is_empty());
        assert!(!semantic.tooltip.is_ascii());
        assert!(!semantic.accessible_name.is_ascii());
    }
}

#[cfg(test)]
#[test]
fn login_icon_button_meets_desktop_accessibility_target() {
    assert_eq!(login_icons::LOGIN_ICON_BUTTON_SIZE, 44.0);
}

#[cfg(test)]
#[test]
fn login_icons_font_is_registered_as_an_isolated_named_family() {
    use std::sync::Arc;

    use egui::{FontDefinitions, FontFamily};

    let mut definitions = FontDefinitions::default();
    login_icons::install_login_icons_font(&mut definitions);

    assert!(definitions
        .font_data
        .contains_key(login_icons::LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY));
    assert_eq!(
        definitions
            .families
            .get(&FontFamily::Name(Arc::from(
                login_icons::LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY
            )))
            .cloned(),
        Some(vec![
            login_icons::LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY.to_owned()
        ])
    );
    assert!(!definitions
        .families
        .get(&FontFamily::Proportional)
        .is_some_and(|fonts| fonts
            .iter()
            .any(|font| font == login_icons::LOGIN_MATERIAL_SYMBOLS_FONT_FAMILY)));
}

#[cfg(test)]
#[test]
fn desktop_login_card_uses_approved_width_pairs_and_spacing() {
    let metrics = connection::login_card_metrics(1100.0);

    assert_eq!(metrics.card_width, 460.0);
    assert!(metrics.use_paired_rows);
    assert_eq!(metrics.corner_radius, 16.0);
    assert_eq!(metrics.inner_margin, 24.0);
    assert_eq!(metrics.primary_button_height, 48.0);
}

#[cfg(test)]
#[test]
fn narrow_login_card_stacks_fields_without_horizontal_clipping() {
    let metrics = connection::login_card_metrics(480.0);

    assert_eq!(metrics.card_width, 448.0);
    assert!(!metrics.use_paired_rows);
    assert!(metrics.card_width <= 460.0);
}

#[cfg(test)]
#[test]
fn explicit_protocol_disables_incompatible_target_in_multi_protocol_catalog() {
    use frd_core::{ProtocolId, TargetSystem};
    use frd_ui_model::ProtocolChoice;

    let catalog =
        frd_protocol_api::ProtocolCatalog::new([ProtocolId::apple_hpss_mvs(), ProtocolId::rdp()]);

    assert!(connection::target_choice_is_supported(
        &catalog,
        TargetSystem::Windows,
        &ProtocolChoice::Automatic,
    ));
    assert!(!connection::target_choice_is_supported(
        &catalog,
        TargetSystem::Windows,
        &ProtocolChoice::Explicit(ProtocolId::apple_hpss_mvs()),
    ));
    assert!(connection::target_choice_is_supported(
        &catalog,
        TargetSystem::MacOs,
        &ProtocolChoice::Explicit(ProtocolId::apple_hpss_mvs()),
    ));
}
