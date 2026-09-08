use frd_core::{InputEvent, KeyState, PhysicalKeyCode, SessionId};
use frd_shell_desktop::{InputGate, InputRouter, KeyboardDomain, KeyboardPreDispatch};

#[test]
fn native_shell_can_match_keyboard_dispatch_and_release_on_focus_loss() {
    let mut input = InputRouter::default();
    input.focus_gained();
    input.set_gate(InputGate::Interactive {
        session_id: SessionId::allocate(),
        generation: 1,
    });
    assert_eq!(input.keyboard_domain(), KeyboardDomain::RemoteSurface);
    let code = PhysicalKeyCode::from_usb_hid_usage(0x04);
    assert!(matches!(
        input.dispatch_key_event(Some(code), KeyState::Pressed, false, true, false),
        KeyboardPreDispatch::Remote(Some(InputEvent::PhysicalKey {
            state: KeyState::Pressed,
            ..
        }))
    ));
    assert_eq!(input.focus_lost(), Some(InputEvent::ReleaseAll));
    assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
    assert!(matches!(
        input.dispatch_key_event(Some(code), KeyState::Released, false, true, false),
        KeyboardPreDispatch::LocalChrome
    ));
    assert!(!input.has_remote_held_input());
}
