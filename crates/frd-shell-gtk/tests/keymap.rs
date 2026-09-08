use frd_shell_gtk::input_keymap::LinuxKeycodePolicy;

fn usage(evdev: u32) -> Option<u16> {
    LinuxKeycodePolicy::LinuxEvdevPlus8
        .decode(evdev + 8)
        .map(|key| key.usb_hid_usage())
}

#[test]
fn alphabet_and_number_row_preserve_physical_positions() {
    let alphabet = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    for (index, evdev) in alphabet.into_iter().enumerate() {
        assert_eq!(usage(evdev), Some(4 + index as u16));
    }
    for evdev in 2..=11 {
        assert_eq!(usage(evdev), Some((evdev + 28) as u16));
    }
}

#[test]
fn left_and_right_modifiers_remain_distinct() {
    for (index, evdev) in [29, 42, 56, 125, 97, 54, 100, 126].into_iter().enumerate() {
        assert_eq!(usage(evdev), Some(0xe0 + index as u16));
    }
}

#[test]
fn function_key_ranges_include_both_ends_and_gap() {
    for evdev in 59..=68 {
        assert_eq!(usage(evdev), Some((evdev - 59 + 0x3a) as u16));
    }
    assert_eq!(usage(87), Some(0x44));
    assert_eq!(usage(88), Some(0x45));
    for evdev in 183..=194 {
        assert_eq!(usage(evdev), Some((evdev - 183 + 0x68) as u16));
    }
    assert_eq!(usage(182), None);
    assert_eq!(usage(195), None);
}

#[test]
fn navigation_and_keypad_are_not_layout_aliases() {
    for (evdev, expected) in [
        (110, 0x49),
        (102, 0x4a),
        (104, 0x4b),
        (111, 0x4c),
        (107, 0x4d),
        (109, 0x4e),
        (106, 0x4f),
        (105, 0x50),
        (108, 0x51),
        (103, 0x52),
        (69, 0x53),
        (98, 0x54),
        (55, 0x55),
        (74, 0x56),
        (78, 0x57),
        (96, 0x58),
        (79, 0x59),
        (80, 0x5a),
        (81, 0x5b),
        (75, 0x5c),
        (76, 0x5d),
        (77, 0x5e),
        (71, 0x5f),
        (72, 0x60),
        (73, 0x61),
        (82, 0x62),
        (83, 0x63),
        (117, 0x67),
        (121, 0x85),
    ] {
        assert_eq!(usage(evdev), Some(expected));
    }
    for (main, keypad) in [(28, 96), (53, 98), (12, 74), (2, 79), (11, 82), (52, 83)] {
        assert_ne!(usage(main), usage(keypad));
    }
}

#[test]
fn international_keys_and_ambiguous_backslash_have_explicit_policy() {
    for (evdev, expected) in [
        (86, 0x64),
        (89, 0x87),
        (93, 0x88),
        (124, 0x89),
        (92, 0x8a),
        (94, 0x8b),
        (95, 0x8c),
        (122, 0x90),
        (123, 0x91),
        (90, 0x92),
        (91, 0x93),
        (85, 0x94),
        (43, 0x31),
    ] {
        assert_eq!(usage(evdev), Some(expected));
    }
    assert!(!(0..=255).any(|code| usage(code) == Some(0x32)));
}

#[test]
fn invalid_offset_unknown_and_consumer_codes_are_rejected() {
    for raw in 0..=8 {
        assert_eq!(LinuxKeycodePolicy::LinuxEvdevPlus8.decode(raw), None);
    }
    assert_eq!(LinuxKeycodePolicy::LinuxEvdevPlus8.decode(u32::MAX), None);
    for evdev in [0, 84, 101, 112, 113, 114, 115, 120, 255, 256, 0x2ff] {
        assert_eq!(usage(evdev), None);
    }
    assert_eq!(
        LinuxKeycodePolicy::LinuxEvdevPlus8
            .decode(9)
            .unwrap()
            .usb_hid_usage(),
        0x29
    );
}
