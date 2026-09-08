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

fn named(name: &[u8; 4]) -> Option<u16> {
    frd_shell_gtk::input_keymap::physical_key_from_xkb_name(name).map(|key| key.usb_hid_usage())
}

#[test]
fn xkb_names_follow_positions_instead_of_numeric_codes_or_layout_symbols() {
    // 两个宿主表的数字编号不同，symbols 可分别是 a/q；只使用实际表中的物理键名。
    let first = [(38, *b"AC01"), (24, *b"AD01")];
    let custom = [(200, *b"AC01"), (38, *b"AD01")];
    let lookup = |table: &[(u32, [u8; 4])], code| {
        table
            .iter()
            .find(|(candidate, _)| *candidate == code)
            .and_then(|(_, name)| named(name))
    };
    assert_eq!(lookup(&first, 38), Some(0x04));
    assert_eq!(lookup(&custom, 200), Some(0x04));
    assert_eq!(lookup(&custom, 38), Some(0x14));
    assert_eq!(lookup(&custom, 24), None);
}

#[test]
fn xkb_modifiers_keypad_and_navigation_keep_distinct_physical_identities() {
    for (index, name) in [
        b"LCTL", b"LFSH", b"LALT", b"LWIN", b"RCTL", b"RTSH", b"RALT", b"RWIN",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(named(name), Some(0xe0 + index as u16));
    }
    for (main, keypad, expected) in [
        (b"RTRN", b"KPEN", 0x58),
        (b"AE01", b"KP1\0", 0x59),
        (b"AB10", b"KPDV", 0x54),
    ] {
        assert_ne!(named(main), named(keypad));
        assert_eq!(named(keypad), Some(expected));
    }
    assert_eq!(named(b"UP\0\0"), Some(0x52));
    assert_eq!(named(b"HOME"), Some(0x4a));
}

#[test]
fn xkb_fixed_fields_reject_unknown_aliases_and_noncanonical_padding() {
    assert_eq!(named(b"AC01"), Some(4)); // 四个字节全部有效，无终止符。
    assert_eq!(named(b"ESC\0"), Some(0x29));
    for name in [
        b"ESC ",
        b"ES\0C",
        b"\0\0\0\0",
        b"I123",
        b"HZTG",
        b"ALGR",
        b"ac01",
        b"AE00",
        b"AD13",
        b"FK00",
        b"FK25",
        b"FK99",
    ] {
        assert_eq!(named(name), None, "{name:?}");
    }
    assert_eq!(named(b"BKSL"), Some(0x31));
    assert_eq!(named(b"LSGT"), Some(0x64));
    assert_eq!(named(b"AB11"), Some(0x87));
    assert_eq!(named(b"AE13"), Some(0x89));
}

#[test]
fn xkb_function_names_preserve_both_function_ranges() {
    for number in 1..=24 {
        let name: [u8; 4] = format!("FK{number:02}").as_bytes().try_into().unwrap();
        let expected = if number <= 12 {
            0x39 + number
        } else {
            0x68 + number - 13
        };
        assert_eq!(named(&name), Some(expected));
    }
}

#[test]
fn supported_xkb_names_are_unique_bounded_and_all_resolve() {
    use frd_shell_gtk::input_keymap::supported_xkb_key_names;
    let mut unique = std::collections::HashSet::new();
    let names = supported_xkb_key_names();
    let declared_len = names.len();
    assert!(declared_len > 100 && declared_len < 256);
    for name in names {
        assert!(unique.insert(*name), "duplicate {name:?}");
        assert!(named(name).is_some(), "unmapped {name:?}");
        let end = name.iter().position(|byte| *byte == 0).unwrap_or(4);
        assert!(name[..end].iter().all(u8::is_ascii_alphanumeric));
        assert!(name[end..].iter().all(|byte| *byte == 0));
    }
    assert_eq!(unique.len(), declared_len);
    assert_eq!(supported_xkb_key_names().next(), Some(b"AE01"));
    assert_eq!(supported_xkb_key_names().next_back(), Some(b"FK24"));
    assert!(!unique.contains(b"FK25"));
    assert!(!unique.contains(b"HZTG"));
}

#[test]
fn xkb_canonical_compose_and_language_names_match_explicit_evdev_usages() {
    // xkeyboard-config 2.41 keycodes/evdev：这里列真实键名，不把 aliases 加进名称表。
    for (name, code, expected) in [
        (b"COMP", 127, 0x65),
        (b"HNGL", 122, 0x90),
        (b"HJCV", 123, 0x91),
        (b"KATA", 90, 0x92),
        (b"HIRA", 91, 0x93),
        (b"JPCM", 95, 0x8c),
    ] {
        assert_eq!(named(name), Some(expected), "{name:?}");
        assert_eq!(named(name), usage(code));
        assert!(frd_shell_gtk::input_keymap::supported_xkb_key_names()
            .any(|candidate| candidate == name));
    }
    for name in [
        b"MENU", b"I130", b"I131", b"I135", b"I147", b"HNGX", b"JPCX",
    ] {
        assert_eq!(named(name), None, "{name:?}");
        assert!(!frd_shell_gtk::input_keymap::supported_xkb_key_names()
            .any(|candidate| candidate == name));
    }
}
