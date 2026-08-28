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

#[derive(Default)]
pub struct InputRouter {
    gate: InputGate,
    focused: bool,
    pointer_position: Option<(f32, f32)>,
    local_buttons: PointerButtons,
    remote_buttons: PointerButtons,
    held_keys: BTreeSet<u32>,
    armed: bool,
    modifiers: Modifiers,
}

impl InputRouter {
    pub fn set_gate(&mut self, gate: InputGate) -> Option<InputEvent> {
        if self.gate == gate {
            return None;
        }
        let release = self.release_remote_state();
        self.gate = gate;
        self.armed = self.is_interactive() && self.focused && !self.local_buttons.any_pressed();
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
        self.armed = self.is_interactive() && !self.local_buttons.any_pressed();
    }

    pub fn focus_lost(&mut self) -> Option<InputEvent> {
        self.focused = false;
        self.armed = false;
        self.release_remote_state()
    }

    pub fn cursor_left(&mut self) -> Option<InputEvent> {
        self.pointer_position = None;
        self.armed = false;
        self.release_remote_state()
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
    ) -> Option<InputEvent> {
        self.pointer_position = Some((drawable_x, drawable_y));
        let Some(remote) = self.map_current_pointer(viewport) else {
            self.armed = false;
            return self.release_remote_state();
        };
        if !self.armed {
            if self.local_buttons.any_pressed() || !self.is_interactive_and_focused() {
                return None;
            }
            self.armed = true;
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
    ) -> Option<InputEvent> {
        set_pointer_button(&mut self.local_buttons, button, state);
        let Some(remote) = self.map_current_pointer(viewport) else {
            if !self.local_buttons.any_pressed() && self.is_interactive_and_focused() {
                self.armed = true;
            }
            return self.release_remote_state();
        };
        if !self.is_interactive_and_focused() {
            return self.release_remote_state();
        }
        if !self.armed {
            if !self.local_buttons.any_pressed() {
                self.armed = true;
            }
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
                Some(InputEvent::PointerButton { button, state })
            }
        }
    }

    pub fn wheel(
        &mut self,
        horizontal: i8,
        vertical: i8,
        viewport: ContentViewport,
    ) -> Option<InputEvent> {
        if !self.is_interactive_and_focused() || !self.armed {
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

    pub fn key(&mut self, code: u32, state: KeyState) -> Option<InputEvent> {
        if !self.is_interactive_and_focused() {
            return None;
        }
        match state {
            KeyState::Pressed => {
                self.held_keys.insert(code);
            }
            KeyState::Released => {
                self.held_keys.remove(&code);
            }
        }
        Some(InputEvent::PhysicalKey {
            code: PhysicalKeyCode(code),
            state,
            modifiers: self.modifiers,
        })
    }

    pub fn text(&self, utf8: String) -> Option<InputEvent> {
        (self.is_interactive_and_focused() && !utf8.is_empty()).then_some(InputEvent::Text { utf8 })
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
        let held = self.remote_buttons.any_pressed() || !self.held_keys.is_empty();
        self.remote_buttons = PointerButtons::default();
        self.held_keys.clear();
        held.then_some(InputEvent::ReleaseAll)
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

    use super::{InputGate, InputRouter};

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
        input.pointer_moved(100.0, 50.0, viewport());
        assert!(input
            .pointer_button(PointerButton::Primary, ButtonState::Pressed, viewport())
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

        input.pointer_button(PointerButton::Primary, ButtonState::Released, viewport());
        input.focus_gained();
        input.set_gate(InputGate::Interactive {
            session_id,
            generation: 2,
        });
        assert!(input.key(7, KeyState::Pressed).is_some());
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
        input.pointer_moved(100.0, 50.0, viewport());
        input.pointer_button(PointerButton::Primary, ButtonState::Pressed, viewport());

        assert_eq!(
            input.pointer_moved(0.0, 50.0, viewport()),
            Some(InputEvent::ReleaseAll)
        );
        assert_eq!(input.pointer_moved(100.0, 50.0, viewport()), None);
        assert_eq!(
            input.pointer_button(PointerButton::Primary, ButtonState::Released, viewport()),
            None
        );
        assert!(input.pointer_moved(100.0, 50.0, viewport()).is_some());
    }
}
