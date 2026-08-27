use crate::{PixelPoint, SessionId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerButton {
    Primary,
    Middle,
    Secondary,
    Back,
    Forward,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalKeyCode(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyState {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    PointerMove {
        remote: PixelPoint,
    },
    PointerButton {
        button: PointerButton,
        state: ButtonState,
    },
    Wheel {
        delta_x: f32,
        delta_y: f32,
    },
    PhysicalKey {
        code: PhysicalKeyCode,
        state: KeyState,
        modifiers: Modifiers,
    },
    Text {
        utf8: String,
    },
    ReleaseAll,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionInput {
    pub session_id: SessionId,
    pub generation: u64,
    pub event: InputEvent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointerSample {
    pub remote: PixelPoint,
    pub mask: u8,
}

impl PointerSample {
    pub const fn new(remote: PixelPoint, mask: u8) -> Self {
        Self { remote, mask }
    }

    fn released(self) -> Self {
        Self { mask: 0, ..self }
    }
}

#[derive(Debug, Default)]
pub struct PointerInputState {
    last_sent: Option<PointerSample>,
    armed: bool,
}

impl PointerInputState {
    pub fn next_event(
        &mut self,
        sample: Option<PointerSample>,
        local_buttons_down: bool,
    ) -> Option<PointerSample> {
        let Some(sample) = sample else {
            self.armed = false;
            return self
                .last_sent
                .take()
                .filter(|last| last.mask != 0)
                .map(PointerSample::released);
        };

        if !self.armed {
            if local_buttons_down {
                return None;
            }
            self.armed = true;
        }

        if self.last_sent == Some(sample) {
            return None;
        }
        self.last_sent = Some(sample);
        Some(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::{PointerInputState, PointerSample};
    use crate::PixelPoint;

    #[test]
    fn gate_confines_pointer_input_to_content_and_safely_rearms() {
        let mut state = PointerInputState::default();
        let idle = PointerSample::new(PixelPoint { x: 10, y: 20 }, 0);
        let held = PointerSample::new(PixelPoint { x: 11, y: 21 }, 1);
        let released = PointerSample::new(PixelPoint { x: 11, y: 21 }, 0);

        assert_eq!(state.next_event(Some(idle), false), Some(idle));
        assert_eq!(state.next_event(Some(idle), false), None);

        assert_eq!(state.next_event(None, false), None);
        assert_eq!(state.next_event(None, false), None);
        assert_eq!(state.next_event(Some(idle), false), Some(idle));

        assert_eq!(state.next_event(Some(held), true), Some(held));
        assert_eq!(state.next_event(None, false), Some(released));
        assert_eq!(state.next_event(None, false), None);

        assert_eq!(state.next_event(Some(held), true), None);
        assert_eq!(state.next_event(Some(released), false), Some(released));

        assert_eq!(state.next_event(None, false), None);
    }
}
