use frd_core::{
    ButtonState, InputEvent, KeyState, PixelPoint, PointerButton, PointerSample, WheelDelta,
};
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp::pdu::input::fast_path::FastPathInputEvent;

const WHEEL_NOTCH_UNITS: i16 = 120;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RdpInputError {
    InvalidScancode,
    InvalidPointerPosition,
    InvalidWheelDelta,
    Stopped,
}

pub(crate) struct RdpInputState {
    database: Database,
    accepting: bool,
}

impl RdpInputState {
    pub(crate) fn new() -> Self {
        Self {
            database: Database::new(),
            accepting: true,
        }
    }

    pub(crate) fn translate(
        &mut self,
        event: InputEvent,
    ) -> Result<Vec<FastPathInputEvent>, RdpInputError> {
        if !self.accepting {
            return Err(RdpInputError::Stopped);
        }
        translate_input(&mut self.database, event)
    }

    pub(crate) fn stop(&mut self) -> Vec<FastPathInputEvent> {
        self.accepting = false;
        self.database.release_all().into_vec()
    }

    pub(crate) fn resume(&mut self) {
        self.accepting = true;
    }
}

pub(crate) fn translate_input(
    database: &mut Database,
    event: InputEvent,
) -> Result<Vec<FastPathInputEvent>, RdpInputError> {
    if event == InputEvent::ReleaseAll {
        return Ok(database.release_all().into_vec());
    }

    let operations = operations_for_event(database, event)?;
    Ok(database.apply(operations).into_vec())
}

fn operations_for_event(
    database: &Database,
    event: InputEvent,
) -> Result<Vec<Operation>, RdpInputError> {
    match event {
        InputEvent::PointerSample(sample) => operations_for_pointer_sample(database, sample),
        InputEvent::PointerMove { remote } => {
            Ok(vec![Operation::MouseMove(mouse_position(remote)?)])
        }
        InputEvent::PointerButton { button, state } => Ok(vec![button_operation(button, state)]),
        InputEvent::Wheel { delta_x, delta_y } => wheel_operations(delta_x, delta_y),
        InputEvent::PhysicalKey { code, state, .. } => {
            let code = u16::try_from(code.0).map_err(|_| RdpInputError::InvalidScancode)?;
            if code > 0x00ff && code & 0xff00 != 0xe000 {
                return Err(RdpInputError::InvalidScancode);
            }
            let scancode = Scancode::from_u16(code);
            Ok(vec![match state {
                KeyState::Pressed => Operation::KeyPressed(scancode),
                KeyState::Released => Operation::KeyReleased(scancode),
            }])
        }
        InputEvent::Text { utf8 } => {
            let mut operations = Vec::with_capacity(utf8.chars().count().saturating_mul(2));
            for character in utf8.chars() {
                operations.push(Operation::UnicodeKeyPressed(character));
                operations.push(Operation::UnicodeKeyReleased(character));
            }
            Ok(operations)
        }
        InputEvent::ReleaseAll => unreachable!("ReleaseAll is handled before operation mapping"),
    }
}

fn operations_for_pointer_sample(
    database: &Database,
    sample: PointerSample,
) -> Result<Vec<Operation>, RdpInputError> {
    let mut operations = Vec::with_capacity(6);
    operations.push(Operation::MouseMove(mouse_position(sample.remote)?));
    append_button_state(
        &mut operations,
        database,
        MouseButton::Left,
        sample.buttons.primary,
    );
    append_button_state(
        &mut operations,
        database,
        MouseButton::Middle,
        sample.buttons.middle,
    );
    append_button_state(
        &mut operations,
        database,
        MouseButton::Right,
        sample.buttons.secondary,
    );
    append_normalized_wheel(&mut operations, sample.wheel);
    Ok(operations)
}

fn append_button_state(
    operations: &mut Vec<Operation>,
    database: &Database,
    button: MouseButton,
    pressed: bool,
) {
    if database.is_mouse_button_pressed(button) == pressed {
        return;
    }
    operations.push(if pressed {
        Operation::MouseButtonPressed(button)
    } else {
        Operation::MouseButtonReleased(button)
    });
}

fn append_normalized_wheel(operations: &mut Vec<Operation>, wheel: WheelDelta) {
    if wheel.horizontal != 0 {
        operations.push(Operation::WheelRotations(WheelRotations {
            is_vertical: false,
            rotation_units: signed_notch(i16::from(wheel.horizontal)),
        }));
    }
    if wheel.vertical != 0 {
        operations.push(Operation::WheelRotations(WheelRotations {
            is_vertical: true,
            rotation_units: signed_notch(i16::from(wheel.vertical)),
        }));
    }
}

fn wheel_operations(delta_x: f32, delta_y: f32) -> Result<Vec<Operation>, RdpInputError> {
    if !delta_x.is_finite() || !delta_y.is_finite() {
        return Err(RdpInputError::InvalidWheelDelta);
    }
    let mut operations = Vec::with_capacity(2);
    if delta_x != 0.0 {
        operations.push(Operation::WheelRotations(WheelRotations {
            is_vertical: false,
            rotation_units: if delta_x.is_sign_positive() {
                WHEEL_NOTCH_UNITS
            } else {
                -WHEEL_NOTCH_UNITS
            },
        }));
    }
    if delta_y != 0.0 {
        operations.push(Operation::WheelRotations(WheelRotations {
            is_vertical: true,
            rotation_units: if delta_y.is_sign_positive() {
                WHEEL_NOTCH_UNITS
            } else {
                -WHEEL_NOTCH_UNITS
            },
        }));
    }
    Ok(operations)
}

fn signed_notch(value: i16) -> i16 {
    if value.is_positive() {
        WHEEL_NOTCH_UNITS
    } else {
        -WHEEL_NOTCH_UNITS
    }
}

fn mouse_position(point: PixelPoint) -> Result<MousePosition, RdpInputError> {
    Ok(MousePosition {
        x: u16::try_from(point.x).map_err(|_| RdpInputError::InvalidPointerPosition)?,
        y: u16::try_from(point.y).map_err(|_| RdpInputError::InvalidPointerPosition)?,
    })
}

fn button_operation(button: PointerButton, state: ButtonState) -> Operation {
    let button = match button {
        PointerButton::Primary => MouseButton::Left,
        PointerButton::Middle => MouseButton::Middle,
        PointerButton::Secondary => MouseButton::Right,
        PointerButton::Back => MouseButton::X1,
        PointerButton::Forward => MouseButton::X2,
    };
    match state {
        ButtonState::Pressed => Operation::MouseButtonPressed(button),
        ButtonState::Released => Operation::MouseButtonReleased(button),
    }
}

#[cfg(test)]
mod tests {
    use frd_core::{
        ButtonState, InputEvent, KeyState, Modifiers, PhysicalKeyCode, PixelPoint, PointerButton,
        PointerButtons, PointerSample, WheelDelta,
    };
    use ironrdp::input::Database;
    use ironrdp::pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};
    use ironrdp::pdu::input::mouse::PointerFlags;
    use ironrdp::pdu::input::mouse_x::PointerXFlags;

    use super::{translate_input, RdpInputError, RdpInputState};

    #[test]
    fn input_scan_code_press_and_release_preserve_key_state() {
        let mut database = Database::new();

        let pressed = translate_input(
            &mut database,
            InputEvent::PhysicalKey {
                code: PhysicalKeyCode(0x001e),
                state: KeyState::Pressed,
                modifiers: Modifiers::default(),
            },
        )
        .expect("set-1 scan code is valid");
        let released = translate_input(
            &mut database,
            InputEvent::PhysicalKey {
                code: PhysicalKeyCode(0x001e),
                state: KeyState::Released,
                modifiers: Modifiers::default(),
            },
        )
        .expect("set-1 scan code is valid");

        assert_eq!(
            pressed,
            vec![FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::empty(),
                0x1e,
            )]
        );
        assert_eq!(
            released,
            vec![FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::RELEASE,
                0x1e,
            )]
        );
    }

    #[test]
    fn input_e0_scan_code_sets_the_extended_flag() {
        let mut database = Database::new();

        let events = translate_input(
            &mut database,
            InputEvent::PhysicalKey {
                code: PhysicalKeyCode(0xe01d),
                state: KeyState::Pressed,
                modifiers: Modifiers {
                    control: true,
                    ..Modifiers::default()
                },
            },
        )
        .expect("E0 scan code is valid");

        assert_eq!(
            events,
            vec![FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::EXTENDED,
                0x1d,
            )]
        );
    }

    #[test]
    fn input_rejects_scan_codes_outside_the_windows_extended_domain() {
        let mut database = Database::new();

        assert_eq!(
            translate_input(
                &mut database,
                InputEvent::PhysicalKey {
                    code: PhysicalKeyCode(0x1_0000),
                    state: KeyState::Pressed,
                    modifiers: Modifiers::default(),
                },
            ),
            Err(RdpInputError::InvalidScancode)
        );
    }

    #[test]
    fn input_unicode_text_emits_press_and_release_for_every_utf16_unit() {
        let mut database = Database::new();

        let events = translate_input(
            &mut database,
            InputEvent::Text {
                utf8: "A😀".to_owned(),
            },
        )
        .expect("Rust text is valid Unicode");

        assert_eq!(
            events,
            vec![
                FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::empty(), 0x0041),
                FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::RELEASE, 0x0041),
                FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::empty(), 0xd83d),
                FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::empty(), 0xde00),
                FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::RELEASE, 0xd83d),
                FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::RELEASE, 0xde00),
            ]
        );
    }

    #[test]
    fn input_pointer_move_preserves_remote_coordinates() {
        let mut database = Database::new();

        let events = translate_input(
            &mut database,
            InputEvent::PointerMove {
                remote: PixelPoint { x: 640, y: 360 },
            },
        )
        .expect("coordinates fit the RDP desktop");

        let [FastPathInputEvent::MouseEvent(mouse)] = events.as_slice() else {
            panic!("pointer movement must emit one standard mouse event");
        };
        assert_eq!(mouse.flags, PointerFlags::MOVE);
        assert_eq!(mouse.number_of_wheel_rotation_units, 0);
        assert_eq!((mouse.x_position, mouse.y_position), (640, 360));
    }

    #[test]
    fn input_rejects_pointer_coordinates_outside_the_rdp_wire_domain() {
        let mut database = Database::new();

        assert_eq!(
            translate_input(
                &mut database,
                InputEvent::PointerMove {
                    remote: PixelPoint { x: 65_536, y: 0 },
                },
            ),
            Err(RdpInputError::InvalidPointerPosition)
        );
    }

    #[test]
    fn input_primary_middle_and_secondary_buttons_use_standard_mouse_events() {
        for (button, flag) in [
            (PointerButton::Primary, PointerFlags::LEFT_BUTTON),
            (PointerButton::Middle, PointerFlags::MIDDLE_BUTTON_OR_WHEEL),
            (PointerButton::Secondary, PointerFlags::RIGHT_BUTTON),
        ] {
            let mut database = Database::new();
            let pressed = translate_input(
                &mut database,
                InputEvent::PointerButton {
                    button,
                    state: ButtonState::Pressed,
                },
            )
            .expect("button is supported");
            let released = translate_input(
                &mut database,
                InputEvent::PointerButton {
                    button,
                    state: ButtonState::Released,
                },
            )
            .expect("button is supported");

            let [FastPathInputEvent::MouseEvent(press)] = pressed.as_slice() else {
                panic!("standard press must emit one standard mouse event");
            };
            let [FastPathInputEvent::MouseEvent(release)] = released.as_slice() else {
                panic!("standard release must emit one standard mouse event");
            };
            assert_eq!(press.flags, PointerFlags::DOWN | flag);
            assert_eq!(release.flags, flag);
        }
    }

    #[test]
    fn input_x1_and_x2_buttons_use_extended_mouse_events() {
        for (button, flag) in [
            (PointerButton::Back, PointerXFlags::BUTTON1),
            (PointerButton::Forward, PointerXFlags::BUTTON2),
        ] {
            let mut database = Database::new();
            let pressed = translate_input(
                &mut database,
                InputEvent::PointerButton {
                    button,
                    state: ButtonState::Pressed,
                },
            )
            .expect("extra button is supported");
            let released = translate_input(
                &mut database,
                InputEvent::PointerButton {
                    button,
                    state: ButtonState::Released,
                },
            )
            .expect("extra button is supported");

            let [FastPathInputEvent::MouseEventEx(press)] = pressed.as_slice() else {
                panic!("extra press must emit one extended mouse event");
            };
            let [FastPathInputEvent::MouseEventEx(release)] = released.as_slice() else {
                panic!("extra release must emit one extended mouse event");
            };
            assert_eq!(press.flags, PointerXFlags::DOWN | flag);
            assert_eq!(release.flags, flag);
        }
    }

    #[test]
    fn input_vertical_and_horizontal_wheel_use_one_notch_units() {
        let mut database = Database::new();

        let events = translate_input(
            &mut database,
            InputEvent::Wheel {
                delta_x: -1.0,
                delta_y: 1.0,
            },
        )
        .expect("finite wheel directions are valid");

        let [FastPathInputEvent::MouseEvent(horizontal), FastPathInputEvent::MouseEvent(vertical)] =
            events.as_slice()
        else {
            panic!("two wheel axes must emit two standard mouse events");
        };
        assert_eq!(horizontal.flags, PointerFlags::HORIZONTAL_WHEEL);
        assert_eq!(horizontal.number_of_wheel_rotation_units, -120);
        assert_eq!(vertical.flags, PointerFlags::VERTICAL_WHEEL);
        assert_eq!(vertical.number_of_wheel_rotation_units, 120);
    }

    #[test]
    fn input_pointer_sample_applies_move_buttons_and_wheel_in_one_transaction() {
        let mut database = Database::new();

        let events = translate_input(
            &mut database,
            InputEvent::PointerSample(PointerSample::new(
                PixelPoint { x: 12, y: 34 },
                PointerButtons {
                    primary: true,
                    middle: false,
                    secondary: true,
                },
                WheelDelta {
                    horizontal: -1,
                    vertical: 1,
                },
            )),
        )
        .expect("normalized pointer sample is valid");

        assert_eq!(events.len(), 5);
        assert!(matches!(
            &events[0],
            FastPathInputEvent::MouseEvent(mouse)
                if mouse.flags == PointerFlags::MOVE
                    && (mouse.x_position, mouse.y_position) == (12, 34)
        ));
        assert!(matches!(
            &events[1],
            FastPathInputEvent::MouseEvent(mouse)
                if mouse.flags == PointerFlags::DOWN | PointerFlags::LEFT_BUTTON
        ));
        assert!(matches!(
            &events[2],
            FastPathInputEvent::MouseEvent(mouse)
                if mouse.flags == PointerFlags::DOWN | PointerFlags::RIGHT_BUTTON
        ));
        assert!(matches!(
            &events[3],
            FastPathInputEvent::MouseEvent(mouse)
                if mouse.flags == PointerFlags::HORIZONTAL_WHEEL
                    && mouse.number_of_wheel_rotation_units == -120
        ));
        assert!(matches!(
            &events[4],
            FastPathInputEvent::MouseEvent(mouse)
                if mouse.flags == PointerFlags::VERTICAL_WHEEL
                    && mouse.number_of_wheel_rotation_units == 120
        ));
    }

    #[test]
    fn input_release_all_after_focus_loss_releases_every_held_input() {
        let mut state = RdpInputState::new();
        state
            .translate(InputEvent::PhysicalKey {
                code: PhysicalKeyCode(0x001e),
                state: KeyState::Pressed,
                modifiers: Modifiers::default(),
            })
            .expect("key press is valid");
        state
            .translate(InputEvent::PointerButton {
                button: PointerButton::Forward,
                state: ButtonState::Pressed,
            })
            .expect("button press is valid");

        let releases = state
            .translate(InputEvent::ReleaseAll)
            .expect("focus-loss release is valid");

        assert_eq!(releases.len(), 2);
        assert!(matches!(
            &releases[0],
            FastPathInputEvent::MouseEventEx(mouse)
                if mouse.flags == PointerXFlags::BUTTON2
        ));
        assert!(matches!(
            &releases[1],
            FastPathInputEvent::KeyboardEvent(flags, 0x1e)
                if flags == &KeyboardFlags::RELEASE
        ));
    }

    #[test]
    fn input_stop_for_disconnect_releases_held_state_once_and_rejects_later_input() {
        let mut state = RdpInputState::new();
        state
            .translate(InputEvent::PhysicalKey {
                code: PhysicalKeyCode(0xe01d),
                state: KeyState::Pressed,
                modifiers: Modifiers::default(),
            })
            .expect("extended key press is valid");

        let releases = state.stop();

        assert_eq!(
            releases,
            vec![FastPathInputEvent::KeyboardEvent(
                KeyboardFlags::RELEASE | KeyboardFlags::EXTENDED,
                0x1d,
            )]
        );
        assert!(state.stop().is_empty());
        assert_eq!(
            state.translate(InputEvent::PointerMove {
                remote: PixelPoint { x: 1, y: 1 },
            }),
            Err(RdpInputError::Stopped)
        );
    }
}
