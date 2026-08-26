#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PointerSample {
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) mask: u8,
}

impl PointerSample {
    pub(crate) const fn new(x: u16, y: u16, mask: u8) -> Self {
        Self { x, y, mask }
    }

    fn released(self) -> Self {
        Self { mask: 0, ..self }
    }
}

#[derive(Debug, Default)]
pub(crate) struct PointerInputState {
    last_sent: Option<PointerSample>,
    armed: bool,
}

impl PointerInputState {
    pub(crate) fn next_event(
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

    #[test]
    fn gate_confines_pointer_input_to_active_window_and_safely_rearms() {
        let mut state = PointerInputState::default();
        let idle = PointerSample::new(10, 20, 0);
        let held = PointerSample::new(11, 21, 1);
        let released = PointerSample::new(11, 21, 0);

        assert_eq!(state.next_event(Some(idle), false), Some(idle));
        assert_eq!(state.next_event(Some(idle), false), None);

        // Pointer left the client area: no movement is forwarded and re-entry
        // starts with a fresh coordinate synchronization.
        assert_eq!(state.next_event(None, false), None);
        assert_eq!(state.next_event(None, false), None);
        assert_eq!(state.next_event(Some(idle), false), Some(idle));

        // Losing the input gate while dragging releases the remote button once.
        assert_eq!(state.next_event(Some(held), true), Some(held));
        assert_eq!(state.next_event(None, false), Some(released));
        assert_eq!(state.next_event(None, false), None);

        // A held button cannot be reintroduced from outside. The pointer is
        // re-armed only after all local buttons have been observed released.
        assert_eq!(state.next_event(Some(held), true), None);
        assert_eq!(state.next_event(Some(released), false), Some(released));

        // An inactive window is represented by the same closed input gate,
        // even when its last geometric coordinate remains inside the client.
        assert_eq!(state.next_event(None, false), None);
    }
}
