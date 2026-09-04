use std::collections::BTreeSet;

use frd_core::{
    ButtonState, ContentViewport, InputEvent, KeyState, Modifiers, PhysicalKeyCode, PointerButton,
    PointerButtons, PointerSample, SessionId, WheelDelta,
};
use winit::keyboard::KeyCode;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InputGate {
    #[default]
    Blocked,
    Interactive {
        session_id: SessionId,
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputOwnership {
    Remote,
    Ui,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum KeyboardDomain {
    RemoteSurface,
    #[default]
    LocalChrome,
}

#[derive(Clone, Debug, PartialEq)]
pub enum KeyboardPreDispatch {
    Consume,
    Remote(Option<InputEvent>),
    EnterLocalChrome(Option<InputEvent>),
    EnterRemoteSurface,
    LocalChrome,
}

#[derive(Default)]
pub struct InputRouter {
    gate: InputGate,
    focused: bool,
    pointer_position: Option<(f32, f32)>,
    local_buttons: PointerButtons,
    remote_buttons: PointerButtons,
    local_back_pressed: bool,
    local_forward_pressed: bool,
    remote_back_pressed: bool,
    remote_forward_pressed: bool,
    local_keys: BTreeSet<PhysicalKeyCode>,
    remote_keys: BTreeSet<PhysicalKeyCode>,
    pointer_armed: bool,
    keyboard_armed: bool,
    modifiers: Modifiers,
    keyboard_domain: KeyboardDomain,
    restore_remote_keyboard_on_focus: bool,
}

impl InputRouter {
    pub fn set_gate(&mut self, gate: InputGate) -> Option<InputEvent> {
        if self.gate == gate {
            return None;
        }
        let release = self.release_remote_state();
        self.restore_remote_keyboard_on_focus = false;
        self.gate = gate;
        self.keyboard_domain = match gate {
            InputGate::Blocked => {
                self.local_keys.clear();
                KeyboardDomain::LocalChrome
            }
            InputGate::Interactive { .. } if self.focused => KeyboardDomain::RemoteSurface,
            InputGate::Interactive { .. } => KeyboardDomain::LocalChrome,
        };
        self.pointer_armed =
            self.is_interactive() && self.focused && !self.any_local_button_pressed();
        self.keyboard_armed = self.is_interactive() && self.focused && self.local_keys.is_empty();
        release
    }

    pub fn interactive_epoch(&self) -> Option<(SessionId, u64)> {
        match self.gate {
            InputGate::Blocked => None,
            InputGate::Interactive {
                session_id,
                generation,
            } => Some((session_id, generation)),
        }
    }

    pub fn focus_gained(&mut self) {
        self.focused = true;
        let restore_remote = std::mem::take(&mut self.restore_remote_keyboard_on_focus)
            && self.is_interactive()
            && self.local_keys.is_empty()
            && self.remote_keys.is_empty();
        if restore_remote {
            self.keyboard_domain = KeyboardDomain::RemoteSurface;
        }
        self.pointer_armed = self.is_interactive() && !self.any_local_button_pressed();
        self.keyboard_armed = self.is_interactive() && self.local_keys.is_empty();
    }

    pub fn focus_lost(&mut self) -> Option<InputEvent> {
        if self.focused {
            self.restore_remote_keyboard_on_focus = self.is_interactive()
                && self.keyboard_domain == KeyboardDomain::RemoteSurface
                && self.local_keys.is_empty()
                && self.remote_keys.is_empty();
        }
        self.focused = false;
        self.keyboard_domain = KeyboardDomain::LocalChrome;
        self.local_keys.clear();
        self.modifiers = Modifiers::default();
        self.disarm_remote_ownership()
    }

    pub fn cursor_left(&mut self) -> Option<InputEvent> {
        self.pointer_position = None;
        self.disarm_remote_ownership()
    }

    pub fn keyboard_capability_lost(&mut self) -> Option<InputEvent> {
        self.disarm_remote_ownership()
    }

    pub fn shutdown(&mut self) -> Option<InputEvent> {
        self.set_gate(InputGate::Blocked)
    }

    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }

    pub fn keyboard_domain(&self) -> KeyboardDomain {
        self.keyboard_domain
    }

    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    pub fn has_remote_held_input(&self) -> bool {
        self.remote_buttons.any_pressed()
            || self.remote_back_pressed
            || self.remote_forward_pressed
            || !self.remote_keys.is_empty()
    }

    pub fn enter_local_chrome(&mut self) -> Option<InputEvent> {
        self.restore_remote_keyboard_on_focus = false;
        self.keyboard_domain = KeyboardDomain::LocalChrome;
        self.disarm_remote_ownership()
    }

    pub fn enter_remote_surface(&mut self) -> bool {
        if !self.is_interactive() {
            return false;
        }
        self.restore_remote_keyboard_on_focus = false;
        self.keyboard_domain = KeyboardDomain::RemoteSurface;
        self.keyboard_armed = self.is_interactive_and_focused() && self.local_keys.is_empty();
        true
    }

    pub fn pointer_pressed_for_domain(&mut self, ownership: InputOwnership) -> Option<InputEvent> {
        match ownership {
            InputOwnership::Ui => self.enter_local_chrome(),
            InputOwnership::Remote => {
                let _ = self.enter_remote_surface();
                None
            }
        }
    }

    pub fn pre_dispatch_key(
        &mut self,
        code: PhysicalKeyCode,
        state: KeyState,
        enter_local_shortcut: bool,
    ) -> KeyboardPreDispatch {
        if self.keyboard_domain == KeyboardDomain::LocalChrome {
            if code.usb_hid_usage() == 0x29
                && state == KeyState::Pressed
                && self.enter_remote_surface()
            {
                return KeyboardPreDispatch::EnterRemoteSurface;
            }
            return KeyboardPreDispatch::LocalChrome;
        }

        if enter_local_shortcut {
            return KeyboardPreDispatch::EnterLocalChrome(self.enter_local_chrome());
        }

        KeyboardPreDispatch::Remote(self.key(code, state, InputOwnership::Remote))
    }

    pub fn dispatch_key_event(
        &mut self,
        code: Option<PhysicalKeyCode>,
        state: KeyState,
        is_synthetic: bool,
        remote_allowed: bool,
        enter_local_shortcut: bool,
    ) -> KeyboardPreDispatch {
        if is_synthetic {
            return KeyboardPreDispatch::Consume;
        }
        let Some(code) = code else {
            return if self.keyboard_domain == KeyboardDomain::RemoteSurface {
                KeyboardPreDispatch::Consume
            } else {
                KeyboardPreDispatch::LocalChrome
            };
        };
        if self.keyboard_domain == KeyboardDomain::RemoteSurface
            && !enter_local_shortcut
            && !remote_allowed
        {
            let _ = self.key(code, state, InputOwnership::Ui);
            return KeyboardPreDispatch::Consume;
        }
        self.pre_dispatch_key(code, state, enter_local_shortcut)
    }

    pub fn pointer_moved(
        &mut self,
        drawable_x: f32,
        drawable_y: f32,
        viewport: ContentViewport,
        ownership: InputOwnership,
    ) -> Option<InputEvent> {
        self.pointer_position = Some((drawable_x, drawable_y));
        if ownership == InputOwnership::Ui {
            return self.disarm_remote_ownership();
        }
        let Some(remote) = self.map_current_pointer(viewport) else {
            return self.disarm_remote_ownership();
        };
        if !self.pointer_armed {
            if self.any_local_button_pressed() || !self.is_interactive_and_focused() {
                return None;
            }
            self.pointer_armed = true;
            self.keyboard_armed = self.local_keys.is_empty();
        }
        if !self.is_interactive_and_focused() {
            return None;
        }
        self.remote_buttons = self.local_buttons;
        Some(InputEvent::PointerSample(PointerSample::new(
            remote,
            self.remote_buttons,
            WheelDelta::default(),
        )))
    }

    pub fn pointer_button(
        &mut self,
        button: PointerButton,
        state: ButtonState,
        viewport: ContentViewport,
        ownership: InputOwnership,
    ) -> Option<InputEvent> {
        set_pointer_button(&mut self.local_buttons, button, state);
        set_auxiliary_button(
            &mut self.local_back_pressed,
            &mut self.local_forward_pressed,
            button,
            state,
        );
        if ownership == InputOwnership::Ui {
            return self.disarm_remote_ownership();
        }
        let Some(remote) = self.map_current_pointer(viewport) else {
            return self.disarm_remote_ownership();
        };
        if !self.is_interactive_and_focused() {
            return self.disarm_remote_ownership();
        }
        if !self.pointer_armed {
            return None;
        }

        match button {
            PointerButton::Primary | PointerButton::Middle | PointerButton::Secondary => {
                self.remote_buttons = self.local_buttons;
                Some(InputEvent::PointerSample(PointerSample::new(
                    remote,
                    self.remote_buttons,
                    WheelDelta::default(),
                )))
            }
            PointerButton::Back | PointerButton::Forward => {
                set_auxiliary_button(
                    &mut self.remote_back_pressed,
                    &mut self.remote_forward_pressed,
                    button,
                    state,
                );
                Some(InputEvent::PointerButton { button, state })
            }
        }
    }

    pub fn wheel(
        &mut self,
        horizontal: i8,
        vertical: i8,
        viewport: ContentViewport,
        ownership: InputOwnership,
    ) -> Option<InputEvent> {
        if ownership == InputOwnership::Ui {
            return self.disarm_remote_ownership();
        }
        if !self.is_interactive_and_focused() || !self.pointer_armed {
            return None;
        }
        let remote = self.map_current_pointer(viewport)?;
        self.remote_buttons = self.local_buttons;
        Some(InputEvent::PointerSample(PointerSample::new(
            remote,
            self.remote_buttons,
            WheelDelta {
                horizontal,
                vertical,
            },
        )))
    }

    pub fn key(
        &mut self,
        code: PhysicalKeyCode,
        state: KeyState,
        ownership: InputOwnership,
    ) -> Option<InputEvent> {
        let local_was_empty = self.local_keys.is_empty();
        match state {
            KeyState::Pressed => {
                self.local_keys.insert(code);
            }
            KeyState::Released => {
                self.local_keys.remove(&code);
            }
        }
        if ownership == InputOwnership::Ui || !self.is_interactive_and_focused() {
            return self.disarm_remote_ownership();
        }
        if !self.keyboard_armed {
            if state == KeyState::Pressed && local_was_empty {
                self.keyboard_armed = true;
            } else {
                return None;
            }
        }
        match state {
            KeyState::Pressed => {
                self.remote_keys.insert(code);
            }
            KeyState::Released if !self.remote_keys.remove(&code) => return None,
            KeyState::Released => {}
        }
        Some(InputEvent::PhysicalKey {
            code,
            state,
            modifiers: self.modifiers,
        })
    }

    pub fn text(&mut self, utf8: String, ownership: InputOwnership) -> Option<InputEvent> {
        if ownership == InputOwnership::Ui {
            return self.disarm_remote_ownership();
        }
        (self.is_interactive_and_focused() && self.keyboard_armed && !utf8.is_empty())
            .then_some(InputEvent::Text { utf8 })
    }

    fn map_current_pointer(&self, viewport: ContentViewport) -> Option<frd_core::PixelPoint> {
        let (x, y) = self.pointer_position?;
        viewport.map_pointer(x, y)
    }

    fn is_interactive(&self) -> bool {
        matches!(self.gate, InputGate::Interactive { .. })
    }

    fn is_interactive_and_focused(&self) -> bool {
        self.is_interactive() && self.focused
    }

    fn release_remote_state(&mut self) -> Option<InputEvent> {
        let held = self.has_remote_held_input();
        self.remote_buttons = PointerButtons::default();
        self.remote_back_pressed = false;
        self.remote_forward_pressed = false;
        self.remote_keys.clear();
        held.then_some(InputEvent::ReleaseAll)
    }

    fn any_local_button_pressed(&self) -> bool {
        self.local_buttons.any_pressed() || self.local_back_pressed || self.local_forward_pressed
    }

    fn disarm_remote_ownership(&mut self) -> Option<InputEvent> {
        self.pointer_armed = false;
        self.keyboard_armed = false;
        self.release_remote_state()
    }
}

pub(crate) fn hid_usage_from_key_code(code: KeyCode) -> Option<u16> {
    use KeyCode::*;

    Some(match code {
        KeyA => 0x04,
        KeyB => 0x05,
        KeyC => 0x06,
        KeyD => 0x07,
        KeyE => 0x08,
        KeyF => 0x09,
        KeyG => 0x0a,
        KeyH => 0x0b,
        KeyI => 0x0c,
        KeyJ => 0x0d,
        KeyK => 0x0e,
        KeyL => 0x0f,
        KeyM => 0x10,
        KeyN => 0x11,
        KeyO => 0x12,
        KeyP => 0x13,
        KeyQ => 0x14,
        KeyR => 0x15,
        KeyS => 0x16,
        KeyT => 0x17,
        KeyU => 0x18,
        KeyV => 0x19,
        KeyW => 0x1a,
        KeyX => 0x1b,
        KeyY => 0x1c,
        KeyZ => 0x1d,
        Digit1 => 0x1e,
        Digit2 => 0x1f,
        Digit3 => 0x20,
        Digit4 => 0x21,
        Digit5 => 0x22,
        Digit6 => 0x23,
        Digit7 => 0x24,
        Digit8 => 0x25,
        Digit9 => 0x26,
        Digit0 => 0x27,
        Enter => 0x28,
        Escape => 0x29,
        Backspace => 0x2a,
        Tab => 0x2b,
        Space => 0x2c,
        Minus => 0x2d,
        Equal => 0x2e,
        BracketLeft => 0x2f,
        BracketRight => 0x30,
        Backslash => 0x31,
        Semicolon => 0x33,
        Quote => 0x34,
        Backquote => 0x35,
        Comma => 0x36,
        Period => 0x37,
        Slash => 0x38,
        CapsLock => 0x39,
        F1 => 0x3a,
        F2 => 0x3b,
        F3 => 0x3c,
        F4 => 0x3d,
        F5 => 0x3e,
        F6 => 0x3f,
        F7 => 0x40,
        F8 => 0x41,
        F9 => 0x42,
        F10 => 0x43,
        F11 => 0x44,
        F12 => 0x45,
        PrintScreen => 0x46,
        ScrollLock => 0x47,
        Pause => 0x48,
        Insert => 0x49,
        Home => 0x4a,
        PageUp => 0x4b,
        Delete => 0x4c,
        End => 0x4d,
        PageDown => 0x4e,
        ArrowRight => 0x4f,
        ArrowLeft => 0x50,
        ArrowDown => 0x51,
        ArrowUp => 0x52,
        NumLock => 0x53,
        NumpadDivide => 0x54,
        NumpadMultiply => 0x55,
        NumpadSubtract => 0x56,
        NumpadAdd => 0x57,
        NumpadEnter => 0x58,
        Numpad1 => 0x59,
        Numpad2 => 0x5a,
        Numpad3 => 0x5b,
        Numpad4 => 0x5c,
        Numpad5 => 0x5d,
        Numpad6 => 0x5e,
        Numpad7 => 0x5f,
        Numpad8 => 0x60,
        Numpad9 => 0x61,
        Numpad0 => 0x62,
        NumpadDecimal => 0x63,
        IntlBackslash => 0x64,
        ContextMenu => 0x65,
        ControlLeft => 0xe0,
        ShiftLeft => 0xe1,
        AltLeft => 0xe2,
        SuperLeft => 0xe3,
        ControlRight => 0xe4,
        ShiftRight => 0xe5,
        AltRight => 0xe6,
        SuperRight => 0xe7,
        _ => return None,
    })
}

fn set_auxiliary_button(
    back: &mut bool,
    forward: &mut bool,
    button: PointerButton,
    state: ButtonState,
) {
    let pressed = state == ButtonState::Pressed;
    match button {
        PointerButton::Back => *back = pressed,
        PointerButton::Forward => *forward = pressed,
        PointerButton::Primary | PointerButton::Middle | PointerButton::Secondary => {}
    }
}

fn set_pointer_button(buttons: &mut PointerButtons, button: PointerButton, state: ButtonState) {
    let pressed = state == ButtonState::Pressed;
    match button {
        PointerButton::Primary => buttons.primary = pressed,
        PointerButton::Middle => buttons.middle = pressed,
        PointerButton::Secondary => buttons.secondary = pressed,
        PointerButton::Back | PointerButton::Forward => {}
    }
}

#[cfg(test)]
mod tests {
    use frd_core::{
        ButtonState, ContentViewport, InputEvent, KeyState, PixelSize, PointerButton, SessionId,
    };

    use super::{
        hid_usage_from_key_code, InputGate, InputOwnership, InputRouter, KeyboardDomain,
        KeyboardPreDispatch,
    };
    use winit::keyboard::KeyCode;

    fn viewport() -> ContentViewport {
        ContentViewport::fit(
            PixelSize::new(100, 100).unwrap(),
            PixelSize::new(200, 100).unwrap(),
        )
    }

    #[test]
    fn winit_physical_keys_normalize_to_usb_hid_keyboard_usages() {
        assert_eq!(hid_usage_from_key_code(KeyCode::KeyA), Some(0x04));
        assert_eq!(hid_usage_from_key_code(KeyCode::Digit1), Some(0x1e));
        assert_eq!(hid_usage_from_key_code(KeyCode::Enter), Some(0x28));
        assert_eq!(hid_usage_from_key_code(KeyCode::ArrowUp), Some(0x52));
        assert_eq!(hid_usage_from_key_code(KeyCode::ControlRight), Some(0xe4));
        assert_eq!(hid_usage_from_key_code(KeyCode::AudioVolumeUp), None);
    }

    #[test]
    fn generation_focus_and_disconnect_emit_one_release_all_per_held_epoch() {
        let session_id = SessionId::allocate();
        let mut input = InputRouter::default();
        input.focus_gained();
        assert_eq!(
            input.set_gate(InputGate::Interactive {
                session_id,
                generation: 1,
            }),
            None
        );
        input.pointer_moved(100.0, 50.0, viewport(), InputOwnership::Remote);
        assert!(input
            .pointer_button(
                PointerButton::Primary,
                ButtonState::Pressed,
                viewport(),
                InputOwnership::Remote,
            )
            .is_some());

        assert_eq!(
            input.set_gate(InputGate::Interactive {
                session_id,
                generation: 2,
            }),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(input.focus_lost(), None);
        assert_eq!(input.set_gate(InputGate::Blocked), None);

        input.pointer_button(
            PointerButton::Primary,
            ButtonState::Released,
            viewport(),
            InputOwnership::Remote,
        );
        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id,
            generation: 2,
        });
        assert!(input
            .key(
                frd_core::PhysicalKeyCode::from_usb_hid_usage(7),
                KeyState::Pressed,
                InputOwnership::Remote,
            )
            .is_some());
        assert_eq!(input.focus_lost(), Some(InputEvent::ReleaseAll));
        assert_eq!(input.focus_lost(), None);
    }

    #[test]
    fn pointer_outside_the_physical_content_viewport_releases_and_stays_silent_until_rearmed() {
        let session_id = SessionId::allocate();
        let mut input = InputRouter::default();
        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id,
            generation: 1,
        });
        input.pointer_moved(100.0, 50.0, viewport(), InputOwnership::Remote);
        input.pointer_button(
            PointerButton::Primary,
            ButtonState::Pressed,
            viewport(),
            InputOwnership::Remote,
        );

        assert_eq!(
            input.pointer_moved(0.0, 50.0, viewport(), InputOwnership::Remote),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(
            input.pointer_moved(100.0, 50.0, viewport(), InputOwnership::Remote),
            None
        );
        assert_eq!(
            input.pointer_button(
                PointerButton::Primary,
                ButtonState::Released,
                viewport(),
                InputOwnership::Remote,
            ),
            None
        );
        assert!(input
            .pointer_moved(100.0, 50.0, viewport(), InputOwnership::Remote)
            .is_some());

        assert!(input
            .pointer_button(
                PointerButton::Back,
                ButtonState::Pressed,
                viewport(),
                InputOwnership::Remote,
            )
            .is_some());
        assert_eq!(
            input.pointer_moved(100.0, 10.0, viewport(), InputOwnership::Ui),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(
            input.pointer_button(
                PointerButton::Back,
                ButtonState::Released,
                viewport(),
                InputOwnership::Ui,
            ),
            None
        );
        assert!(input
            .pointer_moved(100.0, 50.0, viewport(), InputOwnership::Remote)
            .is_some());
    }

    #[test]
    fn pointer_drag_release_over_ui_emits_one_release_and_rearms_only_after_release_and_reentry() {
        let session_id = SessionId::allocate();
        let mut input = InputRouter::default();
        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id,
            generation: 1,
        });
        input.pointer_moved(100.0, 50.0, viewport(), InputOwnership::Remote);
        assert!(input
            .pointer_button(
                PointerButton::Primary,
                ButtonState::Pressed,
                viewport(),
                InputOwnership::Remote,
            )
            .is_some());

        assert_eq!(
            input.pointer_moved(100.0, 10.0, viewport(), InputOwnership::Ui),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(
            input.pointer_button(
                PointerButton::Primary,
                ButtonState::Released,
                viewport(),
                InputOwnership::Ui,
            ),
            None
        );
        assert!(input
            .pointer_moved(100.0, 50.0, viewport(), InputOwnership::Remote)
            .is_some());
    }

    #[test]
    fn keyboard_focus_transfer_to_ui_releases_once_and_waits_for_a_fresh_press() {
        let session_id = SessionId::allocate();
        let mut input = InputRouter::default();
        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id,
            generation: 1,
        });
        assert!(input
            .key(
                frd_core::PhysicalKeyCode::from_usb_hid_usage(7),
                KeyState::Pressed,
                InputOwnership::Remote,
            )
            .is_some());

        assert_eq!(
            input.key(
                frd_core::PhysicalKeyCode::from_usb_hid_usage(7),
                KeyState::Released,
                InputOwnership::Ui,
            ),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(
            input.key(
                frd_core::PhysicalKeyCode::from_usb_hid_usage(8),
                KeyState::Released,
                InputOwnership::Ui,
            ),
            None
        );
        assert!(input
            .key(
                frd_core::PhysicalKeyCode::from_usb_hid_usage(8),
                KeyState::Pressed,
                InputOwnership::Remote,
            )
            .is_some());
        assert_eq!(
            input.keyboard_capability_lost(),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(input.keyboard_capability_lost(), None);
    }

    fn interactive_input() -> InputRouter {
        let mut input = InputRouter::default();
        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id: SessionId::allocate(),
            generation: 1,
        });
        input
    }

    #[test]
    fn remote_surface_tab_is_dispatched_before_egui_even_when_egui_would_consume_it() {
        let mut input = interactive_input();
        let mut egui_called = false;

        let dispatch = input.pre_dispatch_key(
            frd_core::PhysicalKeyCode::from_usb_hid_usage(0x2b),
            KeyState::Pressed,
            false,
        );
        if dispatch == KeyboardPreDispatch::LocalChrome {
            egui_called = true;
        }

        assert!(!egui_called);
        assert!(matches!(
            dispatch,
            KeyboardPreDispatch::Remote(Some(InputEvent::PhysicalKey { code, .. }))
                if code.usb_hid_usage() == 0x2b
        ));
    }

    #[test]
    fn ctrl_alt_home_switches_local_without_sending_home_and_releases_remote_once() {
        let mut input = interactive_input();
        input.set_modifiers(frd_core::Modifiers {
            control: true,
            alt: true,
            ..Default::default()
        });
        let control = frd_core::PhysicalKeyCode::from_usb_hid_usage(0xe0);
        assert!(matches!(
            input.pre_dispatch_key(control, KeyState::Pressed, false),
            KeyboardPreDispatch::Remote(Some(_))
        ));

        let home = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x4a);
        assert_eq!(
            input.pre_dispatch_key(home, KeyState::Pressed, true),
            KeyboardPreDispatch::EnterLocalChrome(Some(InputEvent::ReleaseAll))
        );
        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
        assert_eq!(
            input.pre_dispatch_key(home, KeyState::Pressed, false),
            KeyboardPreDispatch::LocalChrome
        );
    }

    #[test]
    fn held_remote_input_is_reported_until_the_ownership_handoff_releases_it() {
        let mut input = interactive_input();
        let control = frd_core::PhysicalKeyCode::from_usb_hid_usage(0xe0);
        assert!(matches!(
            input.pre_dispatch_key(control, KeyState::Pressed, false),
            KeyboardPreDispatch::Remote(Some(_))
        ));
        assert!(input.has_remote_held_input());

        assert_eq!(input.enter_local_chrome(), Some(InputEvent::ReleaseAll));
        assert!(!input.has_remote_held_input());
    }

    #[test]
    fn complete_local_shortcut_chord_does_not_poison_the_next_remote_key_pair() {
        let mut input = interactive_input();
        let control = frd_core::PhysicalKeyCode::from_usb_hid_usage(0xe0);
        let alt = frd_core::PhysicalKeyCode::from_usb_hid_usage(0xe2);
        let home = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x4a);
        let a = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04);
        let mut remote = Vec::new();

        for key in [control, alt] {
            if let KeyboardPreDispatch::Remote(Some(event)) =
                input.dispatch_key_event(Some(key), KeyState::Pressed, false, true, false)
            {
                remote.push(event);
            }
        }
        input.set_modifiers(frd_core::Modifiers {
            control: true,
            alt: true,
            ..Default::default()
        });
        if let KeyboardPreDispatch::EnterLocalChrome(Some(event)) =
            input.dispatch_key_event(Some(home), KeyState::Pressed, false, true, true)
        {
            remote.push(event);
        }
        assert_eq!(
            input.dispatch_key_event(Some(home), KeyState::Pressed, false, true, false),
            KeyboardPreDispatch::LocalChrome
        );
        assert_eq!(input.key(home, KeyState::Pressed, InputOwnership::Ui), None);
        for state in [KeyState::Pressed, KeyState::Released] {
            assert_eq!(
                input.dispatch_key_event(Some(home), state, false, true, false),
                KeyboardPreDispatch::LocalChrome
            );
            assert_eq!(input.key(home, state, InputOwnership::Ui), None);
        }
        for key in [alt, control] {
            assert_eq!(
                input.dispatch_key_event(Some(key), KeyState::Released, false, true, false),
                KeyboardPreDispatch::LocalChrome
            );
            assert_eq!(input.key(key, KeyState::Released, InputOwnership::Ui), None);
        }
        assert_eq!(
            input.dispatch_key_event(
                Some(frd_core::PhysicalKeyCode::from_usb_hid_usage(0x29)),
                KeyState::Pressed,
                false,
                true,
                false,
            ),
            KeyboardPreDispatch::EnterRemoteSurface
        );
        for state in [KeyState::Pressed, KeyState::Released] {
            if let KeyboardPreDispatch::Remote(Some(event)) =
                input.dispatch_key_event(Some(a), state, false, true, false)
            {
                remote.push(event);
            }
        }

        assert_eq!(
            remote
                .iter()
                .filter(|event| matches!(event, InputEvent::ReleaseAll))
                .count(),
            1
        );
        assert!(!remote.iter().any(|event| matches!(
            event,
            InputEvent::PhysicalKey { code, .. } if code.usb_hid_usage() == 0x4a
        )));
        assert!(matches!(
            remote.as_slice(),
            [
                InputEvent::PhysicalKey { code: first, state: KeyState::Pressed, .. },
                InputEvent::PhysicalKey { code: second, state: KeyState::Pressed, .. },
                InputEvent::ReleaseAll,
                InputEvent::PhysicalKey { code: pressed, state: KeyState::Pressed, .. },
                InputEvent::PhysicalKey { code: released, state: KeyState::Released, .. },
            ] if *first == control && *second == alt && *pressed == a && *released == a
        ));
    }

    #[test]
    fn local_chrome_navigation_is_left_to_egui_and_never_sent_remote() {
        let mut input = interactive_input();
        assert_eq!(input.enter_local_chrome(), None);
        for usage in [0x2b, 0x28, 0x2c, 0x04] {
            assert_eq!(
                input.pre_dispatch_key(
                    frd_core::PhysicalKeyCode::from_usb_hid_usage(usage),
                    KeyState::Pressed,
                    false,
                ),
                KeyboardPreDispatch::LocalChrome
            );
        }
    }

    #[test]
    fn remote_click_and_escape_restore_remote_surface() {
        let mut input = interactive_input();
        input.enter_local_chrome();
        assert_eq!(
            input.pointer_pressed_for_domain(InputOwnership::Remote),
            None
        );
        assert_eq!(input.keyboard_domain(), KeyboardDomain::RemoteSurface);

        input.enter_local_chrome();
        assert_eq!(
            input.pre_dispatch_key(
                frd_core::PhysicalKeyCode::from_usb_hid_usage(0x29),
                KeyState::Pressed,
                false,
            ),
            KeyboardPreDispatch::EnterRemoteSurface
        );
        assert_eq!(input.keyboard_domain(), KeyboardDomain::RemoteSurface);
    }

    #[test]
    fn focus_loss_and_reconnect_reset_keyboard_domain_without_duplicate_releases() {
        let mut input = interactive_input();
        let key = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04);
        assert!(matches!(
            input.pre_dispatch_key(key, KeyState::Pressed, false),
            KeyboardPreDispatch::Remote(Some(_))
        ));
        assert_eq!(input.focus_lost(), Some(InputEvent::ReleaseAll));
        assert_eq!(input.focus_lost(), None);
        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
        assert_eq!(input.modifiers(), frd_core::Modifiers::default());

        input.set_gate(InputGate::Blocked);
        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id: SessionId::allocate(),
            generation: 2,
        });
        assert_eq!(input.keyboard_domain(), KeyboardDomain::RemoteSurface);
        assert!(matches!(
            input.pre_dispatch_key(key, KeyState::Pressed, false),
            KeyboardPreDispatch::Remote(Some(_))
        ));
        assert!(matches!(
            input.pre_dispatch_key(key, KeyState::Released, false),
            KeyboardPreDispatch::Remote(Some(_))
        ));
        assert!(matches!(
            input.pre_dispatch_key(key, KeyState::Released, false),
            KeyboardPreDispatch::Remote(None)
        ));
    }

    #[test]
    fn clean_remote_keyboard_domain_is_restored_after_focus_returns_in_same_epoch() {
        let mut input = interactive_input();
        let key = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04);

        assert_eq!(input.focus_lost(), None);
        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
        assert_eq!(
            input.pre_dispatch_key(key, KeyState::Pressed, false),
            KeyboardPreDispatch::LocalChrome
        );

        input.focus_gained();

        assert_eq!(input.keyboard_domain(), KeyboardDomain::RemoteSurface);
    }

    #[test]
    fn local_keyboard_domain_stays_local_after_focus_returns() {
        let mut input = interactive_input();
        assert_eq!(input.enter_local_chrome(), None);

        assert_eq!(input.focus_lost(), None);
        input.focus_gained();

        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
    }

    #[test]
    fn held_key_at_focus_loss_releases_remote_and_does_not_restore_its_domain() {
        let mut input = interactive_input();
        let key = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04);
        assert!(matches!(
            input.pre_dispatch_key(key, KeyState::Pressed, false),
            KeyboardPreDispatch::Remote(Some(InputEvent::PhysicalKey { .. }))
        ));

        assert_eq!(input.focus_lost(), Some(InputEvent::ReleaseAll));
        input.focus_gained();

        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
    }

    #[test]
    fn local_key_residue_at_focus_loss_prevents_remote_domain_restore() {
        let mut input = interactive_input();
        let key = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04);
        assert_eq!(input.key(key, KeyState::Pressed, InputOwnership::Ui), None);

        assert_eq!(input.focus_lost(), None);
        input.focus_gained();

        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
    }

    #[test]
    fn new_generation_in_same_session_clears_pending_remote_domain_restore() {
        let mut input = interactive_input();
        let (session_id, generation) = input.interactive_epoch().unwrap();
        assert_eq!(input.focus_lost(), None);

        input.set_gate(InputGate::Interactive {
            session_id,
            generation: generation + 1,
        });
        input.focus_gained();

        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
    }

    #[test]
    fn new_session_at_same_generation_clears_pending_remote_domain_restore() {
        let mut input = interactive_input();
        let (_, generation) = input.interactive_epoch().unwrap();
        assert_eq!(input.focus_lost(), None);

        input.set_gate(InputGate::Interactive {
            session_id: SessionId::allocate(),
            generation,
        });
        input.focus_gained();

        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
    }

    #[test]
    fn blocked_gate_clears_pending_remote_domain_restore() {
        let mut input = interactive_input();
        assert_eq!(input.focus_lost(), None);

        assert_eq!(input.set_gate(InputGate::Blocked), None);
        input.focus_gained();

        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
    }

    #[test]
    fn interactive_gate_entered_while_unfocused_stays_local_after_focus_returns() {
        let mut input = InputRouter::default();

        assert_eq!(input.set_gate(InputGate::Blocked), None);
        assert_eq!(
            input.set_gate(InputGate::Interactive {
                session_id: SessionId::allocate(),
                generation: 1,
            }),
            None
        );
        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);

        input.focus_gained();

        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
    }

    #[test]
    fn synthetic_keyboard_events_are_consumed_without_touching_domain_or_ledgers() {
        let mut input = interactive_input();
        let a = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04);
        for state in [KeyState::Pressed, KeyState::Released] {
            assert_eq!(
                input.dispatch_key_event(Some(a), state, true, true, true),
                KeyboardPreDispatch::Consume
            );
            assert_eq!(input.keyboard_domain(), KeyboardDomain::RemoteSurface);
        }
        assert_eq!(input.focus_lost(), None);

        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id: SessionId::allocate(),
            generation: 2,
        });
        assert!(matches!(
            input.dispatch_key_event(Some(a), KeyState::Pressed, false, true, false),
            KeyboardPreDispatch::Remote(Some(InputEvent::PhysicalKey {
                state: KeyState::Pressed,
                ..
            }))
        ));
    }

    #[test]
    fn blocked_login_and_disabled_remote_keyboard_stay_local_without_remote_held_state() {
        let mut input = InputRouter::default();
        let a = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04);
        let escape = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x29);

        assert_eq!(
            input.pointer_pressed_for_domain(InputOwnership::Remote),
            None
        );
        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
        assert_eq!(
            input.dispatch_key_event(Some(escape), KeyState::Pressed, false, true, false),
            KeyboardPreDispatch::LocalChrome
        );
        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);

        assert_eq!(
            input.dispatch_key_event(Some(a), KeyState::Pressed, false, true, false),
            KeyboardPreDispatch::LocalChrome
        );
        assert_eq!(input.key(a, KeyState::Pressed, InputOwnership::Ui), None);

        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id: SessionId::allocate(),
            generation: 1,
        });
        assert_eq!(
            input.dispatch_key_event(Some(a), KeyState::Released, false, false, false),
            KeyboardPreDispatch::Consume
        );
        assert_eq!(input.focus_lost(), None);

        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id: SessionId::allocate(),
            generation: 2,
        });
        assert!(matches!(
            input.dispatch_key_event(Some(a), KeyState::Pressed, false, true, false),
            KeyboardPreDispatch::Remote(Some(_))
        ));
        assert_eq!(
            input.set_gate(InputGate::Blocked),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(input.keyboard_domain(), KeyboardDomain::LocalChrome);
        assert_eq!(input.set_gate(InputGate::Blocked), None);
    }

    #[test]
    fn focus_loss_clears_missing_keyup_before_synthetic_replay_and_fresh_remote_input() {
        let mut input = interactive_input();
        let a = frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04);
        input.set_modifiers(frd_core::Modifiers {
            control: true,
            ..Default::default()
        });
        assert!(matches!(
            input.dispatch_key_event(Some(a), KeyState::Pressed, false, true, false),
            KeyboardPreDispatch::Remote(Some(_))
        ));
        assert_eq!(input.focus_lost(), Some(InputEvent::ReleaseAll));
        assert_eq!(input.focus_lost(), None);
        assert_eq!(input.modifiers(), frd_core::Modifiers::default());

        input.focus_gained();
        assert_eq!(
            input.dispatch_key_event(Some(a), KeyState::Pressed, true, true, false),
            KeyboardPreDispatch::Consume
        );
        assert_eq!(
            input.dispatch_key_event(Some(a), KeyState::Released, false, true, false),
            KeyboardPreDispatch::LocalChrome
        );
        assert_eq!(input.key(a, KeyState::Released, InputOwnership::Ui), None);
        assert_eq!(
            input.pointer_pressed_for_domain(InputOwnership::Remote),
            None
        );
        assert!(matches!(
            input.dispatch_key_event(Some(a), KeyState::Pressed, false, true, false),
            KeyboardPreDispatch::Remote(Some(InputEvent::PhysicalKey {
                state: KeyState::Pressed,
                ..
            }))
        ));
        assert!(matches!(
            input.dispatch_key_event(Some(a), KeyState::Released, false, true, false),
            KeyboardPreDispatch::Remote(Some(InputEvent::PhysicalKey {
                state: KeyState::Released,
                ..
            }))
        ));
    }
}
