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
    /// 一次 UI 采样对应一个原子的协议中立指针状态。
    PointerSample(PointerSample),
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PointerButtons {
    pub primary: bool,
    pub middle: bool,
    pub secondary: bool,
}

impl PointerButtons {
    pub const fn any_pressed(self) -> bool {
        self.primary || self.middle || self.secondary
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WheelDelta {
    pub horizontal: i8,
    pub vertical: i8,
}

impl WheelDelta {
    pub const fn is_empty(self) -> bool {
        self.horizontal == 0 && self.vertical == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointerSample {
    pub remote: PixelPoint,
    pub buttons: PointerButtons,
    pub wheel: WheelDelta,
}

impl PointerSample {
    pub const fn new(remote: PixelPoint, buttons: PointerButtons, wheel: WheelDelta) -> Self {
        Self {
            remote,
            buttons,
            wheel,
        }
    }

    fn released(self) -> Self {
        Self {
            buttons: PointerButtons {
                primary: false,
                middle: false,
                secondary: false,
            },
            wheel: WheelDelta {
                horizontal: 0,
                vertical: 0,
            },
            ..self
        }
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
                .filter(|last| last.buttons.any_pressed() || !last.wheel.is_empty())
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
    use super::{PointerButtons, PointerInputState, PointerSample, WheelDelta};
    use crate::PixelPoint;

    #[test]
    fn gate_confines_pointer_input_to_content_and_safely_rearms() {
        let mut state = PointerInputState::default();
        let idle = PointerSample::new(
            PixelPoint { x: 10, y: 20 },
            PointerButtons::default(),
            WheelDelta::default(),
        );
        let held = PointerSample::new(
            PixelPoint { x: 11, y: 21 },
            PointerButtons {
                primary: true,
                ..Default::default()
            },
            WheelDelta {
                vertical: 1,
                ..Default::default()
            },
        );
        let released = PointerSample::new(
            PixelPoint { x: 11, y: 21 },
            PointerButtons::default(),
            WheelDelta::default(),
        );

        assert_eq!(state.next_event(Some(idle), false), Some(idle));
        assert_eq!(state.next_event(Some(idle), false), None);

        assert_eq!(state.next_event(None, false), None);
        assert_eq!(state.next_event(None, false), None);
        assert_eq!(state.next_event(Some(idle), false), Some(idle));

        assert_eq!(state.next_event(Some(held), true), Some(held));
        let drag_out_release = state.next_event(None, false);
        assert_eq!(drag_out_release, Some(released));
        let drag_out_release = drag_out_release.expect("拖出必须生成中性释放");
        assert_eq!(drag_out_release.buttons, PointerButtons::default());
        assert_eq!(drag_out_release.wheel, WheelDelta::default());
        assert_eq!(state.next_event(None, false), None);

        assert_eq!(state.next_event(Some(held), true), None);
        assert_eq!(state.next_event(Some(released), false), Some(released));

        assert_eq!(state.next_event(None, false), None);
    }
}
