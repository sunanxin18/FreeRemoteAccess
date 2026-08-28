use std::collections::BTreeSet;

use frd_core::{
    ButtonState, ContentViewport, InputEvent, KeyState, Modifiers, PhysicalKeyCode, PointerButton,
    PointerButtons, PointerSample, SessionId, WheelDelta,
};

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
    local_keys: BTreeSet<u32>,
    remote_keys: BTreeSet<u32>,
    pointer_armed: bool,
    keyboard_armed: bool,
    modifiers: Modifiers,
}

impl InputRouter {
    pub fn set_gate(&mut self, gate: InputGate) -> Option<InputEvent> {
        if self.gate == gate {
            return None;
        }
        let release = self.release_remote_state();
        self.gate = gate;
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
        self.pointer_armed = self.is_interactive() && !self.any_local_button_pressed();
        self.keyboard_armed = self.is_interactive() && self.local_keys.is_empty();
    }

    pub fn focus_lost(&mut self) -> Option<InputEvent> {
        self.focused = false;
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
        code: u32,
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
            code: PhysicalKeyCode(code),
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
        let held = self.remote_buttons.any_pressed()
            || self.remote_back_pressed
            || self.remote_forward_pressed
            || !self.remote_keys.is_empty();
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

    use super::{InputGate, InputOwnership, InputRouter};

    fn viewport() -> ContentViewport {
        ContentViewport::fit(
            PixelSize::new(100, 100).unwrap(),
            PixelSize::new(200, 100).unwrap(),
        )
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
            .key(7, KeyState::Pressed, InputOwnership::Remote)
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
            .key(7, KeyState::Pressed, InputOwnership::Remote)
            .is_some());

        assert_eq!(
            input.key(7, KeyState::Released, InputOwnership::Ui),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(input.key(8, KeyState::Released, InputOwnership::Ui), None);
        assert!(input
            .key(8, KeyState::Pressed, InputOwnership::Remote)
            .is_some());
        assert_eq!(
            input.keyboard_capability_lost(),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(input.keyboard_capability_lost(), None);
    }
}
