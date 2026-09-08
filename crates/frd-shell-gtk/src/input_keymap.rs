//! GTK 硬件键码到 USB HID Keyboard/Keypad usage 的显式映射。
//!
//! LinuxKeycodePolicy 仅适用于宿主已确认采用 Linux evdev + 8 的配置；
//! 不根据 X11/Wayland 名称推断键码体系。physical_key_from_xkb_name 则接受
//! 宿主实际 XKB 表中的物理位置名称，支持自定义数字 keycodes。
//! 固定证据：
//! - https://github.com/GNOME/gtk/blob/4.14.0/gdk/wayland/gdkseat-wayland.c#L1437
//! - https://github.com/GNOME/gtk/blob/4.14.0/gdk/x11/gdkdevicemanager-xi2.c#L1584
//! - https://github.com/torvalds/linux/blob/v6.8/drivers/hid/hid-input.c#L27
//! - https://github.com/torvalds/linux/blob/v6.8/include/uapi/linux/input-event-codes.h
//!
//! 内核 HID→evdev 表不是单射：KEY_BACKSLASH 43 同时来自 usage 0x31/0x32。
//! 此处明确选择标准 Keyboard Backslash 0x31，不能恢复原设备的 0x32 身份。
//! KEY_DELETE 明确选择标准 Forward Delete 0x4c；不推断重复的扩展 usage。
//! 显式 evdev 路径的国际键只覆盖表中明确的 102ND/RO/YEN/HENKAN/MUHENKAN/KATAKANAHIRAGANA、
//! HANGEUL/HANJA/KATAKANA/HIRAGANA/ZENKAKUHANKAKU。消费页媒体键及未知键拒绝。
//! XKB 路径仅接受下表的 canonical 名称（包括 COMP、HNGL/HJCV/KATA/HIRA/JPCM）；
//! MENU 等 aliases 不接受，因此两路径不承诺覆盖完全相同的输入名称。

use frd_core::PhysicalKeyCode;

/// 调用方必须先确认键码体系；没有自动选择或默认实现。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxKeycodePolicy {
    LinuxEvdevPlus8,
}

impl LinuxKeycodePolicy {
    pub fn decode(self, hardware_keycode: u32) -> Option<PhysicalKeyCode> {
        match self {
            Self::LinuxEvdevPlus8 => evdev_usage(hardware_keycode.checked_sub(8)?)
                .map(PhysicalKeyCode::from_usb_hid_usage),
        }
    }
}

/// 标准 XKB 物理位置名到 HID；输入是 X11 的完整四字节字段，不要求 NUL 结尾。
/// 短名称使用末尾 NUL 填充（例如 ESC\0），不接受空格填充或内嵌 NUL 后的垃圾。
/// 名称依据 xkeyboard-config 的物理位置约定，不读取 keyval、symbols 或数字 keycode。
/// https://xkeyboard-config.pages.freedesktop.org/website/doc/enhancing/
/// 不解析用户 aliases：调用方不能把冲突 alias 覆盖到标准名称；未知名称返回 None。
/// BKSL 明确使用标准 Keyboard Backslash 0x31，无法恢复设备级 0x32 身份。
pub fn physical_key_from_xkb_name(name: &[u8; 4]) -> Option<PhysicalKeyCode> {
    XKB_PHYSICAL_KEYS
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, usage)| PhysicalKeyCode::from_usb_hid_usage(*usage))
}

/// 受支持的标准名称，和映射函数共用唯一表；不包含推测的用户别名。
/// 四字节字段不是 C 字符串，原生 key_by_name 调用方须自行附加终止符。
pub fn supported_xkb_key_names(
) -> impl ExactSizeIterator<Item = &'static [u8; 4]> + DoubleEndedIterator {
    XKB_PHYSICAL_KEYS.iter().map(|(name, _)| name)
}

const XKB_PHYSICAL_KEYS: &[([u8; 4], u16)] = &[
    (*b"AE01", 0x1e),
    (*b"AE02", 0x1f),
    (*b"AE03", 0x20),
    (*b"AE04", 0x21),
    (*b"AE05", 0x22),
    (*b"AE06", 0x23),
    (*b"AE07", 0x24),
    (*b"AE08", 0x25),
    (*b"AE09", 0x26),
    (*b"AE10", 0x27),
    (*b"AE11", 0x2d),
    (*b"AE12", 0x2e),
    (*b"AD01", 0x14),
    (*b"AD02", 0x1a),
    (*b"AD03", 0x08),
    (*b"AD04", 0x15),
    (*b"AD05", 0x17),
    (*b"AD06", 0x1c),
    (*b"AD07", 0x18),
    (*b"AD08", 0x0c),
    (*b"AD09", 0x12),
    (*b"AD10", 0x13),
    (*b"AD11", 0x2f),
    (*b"AD12", 0x30),
    (*b"AC01", 0x04),
    (*b"AC02", 0x16),
    (*b"AC03", 0x07),
    (*b"AC04", 0x09),
    (*b"AC05", 0x0a),
    (*b"AC06", 0x0b),
    (*b"AC07", 0x0d),
    (*b"AC08", 0x0e),
    (*b"AC09", 0x0f),
    (*b"AC10", 0x33),
    (*b"AC11", 0x34),
    (*b"AB01", 0x1d),
    (*b"AB02", 0x1b),
    (*b"AB03", 0x06),
    (*b"AB04", 0x19),
    (*b"AB05", 0x05),
    (*b"AB06", 0x11),
    (*b"AB07", 0x10),
    (*b"AB08", 0x36),
    (*b"AB09", 0x37),
    (*b"AB10", 0x38),
    (*b"ESC\0", 0x29),
    (*b"BKSP", 0x2a),
    (*b"TAB\0", 0x2b),
    (*b"RTRN", 0x28),
    (*b"TLDE", 0x35),
    (*b"BKSL", 0x31),
    (*b"SPCE", 0x2c),
    (*b"CAPS", 0x39),
    (*b"LCTL", 0xe0),
    (*b"LFSH", 0xe1),
    (*b"LALT", 0xe2),
    (*b"LWIN", 0xe3),
    (*b"RCTL", 0xe4),
    (*b"RTSH", 0xe5),
    (*b"RALT", 0xe6),
    (*b"RWIN", 0xe7),
    (*b"PRSC", 0x46),
    (*b"SCLK", 0x47),
    (*b"PAUS", 0x48),
    (*b"INS\0", 0x49),
    (*b"HOME", 0x4a),
    (*b"PGUP", 0x4b),
    (*b"DELE", 0x4c),
    (*b"END\0", 0x4d),
    (*b"PGDN", 0x4e),
    (*b"RGHT", 0x4f),
    (*b"LEFT", 0x50),
    (*b"DOWN", 0x51),
    (*b"UP\0\0", 0x52),
    (*b"NMLK", 0x53),
    (*b"KPDV", 0x54),
    (*b"KPMU", 0x55),
    (*b"KPSU", 0x56),
    (*b"KPAD", 0x57),
    (*b"KPEN", 0x58),
    (*b"KPEQ", 0x67),
    (*b"KP1\0", 0x59),
    (*b"KP2\0", 0x5a),
    (*b"KP3\0", 0x5b),
    (*b"KP4\0", 0x5c),
    (*b"KP5\0", 0x5d),
    (*b"KP6\0", 0x5e),
    (*b"KP7\0", 0x5f),
    (*b"KP8\0", 0x60),
    (*b"KP9\0", 0x61),
    (*b"KP0\0", 0x62),
    (*b"KPDL", 0x63),
    (*b"LSGT", 0x64),
    (*b"COMP", 0x65),
    (*b"AB11", 0x87),
    (*b"AE13", 0x89),
    (*b"HKTG", 0x88),
    (*b"HENK", 0x8a),
    (*b"MUHE", 0x8b),
    // 固定 xkeyboard-config 2.41 keycodes/evdev：98/99/103/130/131，减8对应旧evdev表。
    // https://gitlab.freedesktop.org/xkeyboard-config/xkeyboard-config/-/blob/xkeyboard-config-2.41/keycodes/evdev
    (*b"KATA", 0x92),
    (*b"HIRA", 0x93),
    (*b"JPCM", 0x8c),
    (*b"HNGL", 0x90),
    (*b"HJCV", 0x91),
    (*b"FK01", 0x3a),
    (*b"FK02", 0x3b),
    (*b"FK03", 0x3c),
    (*b"FK04", 0x3d),
    (*b"FK05", 0x3e),
    (*b"FK06", 0x3f),
    (*b"FK07", 0x40),
    (*b"FK08", 0x41),
    (*b"FK09", 0x42),
    (*b"FK10", 0x43),
    (*b"FK11", 0x44),
    (*b"FK12", 0x45),
    (*b"FK13", 0x68),
    (*b"FK14", 0x69),
    (*b"FK15", 0x6a),
    (*b"FK16", 0x6b),
    (*b"FK17", 0x6c),
    (*b"FK18", 0x6d),
    (*b"FK19", 0x6e),
    (*b"FK20", 0x6f),
    (*b"FK21", 0x70),
    (*b"FK22", 0x71),
    (*b"FK23", 0x72),
    (*b"FK24", 0x73),
];

fn evdev_usage(code: u32) -> Option<u16> {
    let usage = match code {
        // 字母：保留物理位置，不使用布局产生的 keyval/Unicode。
        30 => 0x04,
        48 => 0x05,
        46 => 0x06,
        32 => 0x07,
        18 => 0x08,
        33 => 0x09,
        34 => 0x0a,
        35 => 0x0b,
        23 => 0x0c,
        36 => 0x0d,
        37 => 0x0e,
        38 => 0x0f,
        50 => 0x10,
        49 => 0x11,
        24 => 0x12,
        25 => 0x13,
        16 => 0x14,
        19 => 0x15,
        31 => 0x16,
        20 => 0x17,
        22 => 0x18,
        47 => 0x19,
        17 => 0x1a,
        45 => 0x1b,
        21 => 0x1c,
        44 => 0x1d,
        2..=11 => (code - 2 + 0x1e) as u16,
        28 => 0x28,
        1 => 0x29,
        14 => 0x2a,
        15 => 0x2b,
        57 => 0x2c,
        12 => 0x2d,
        13 => 0x2e,
        26 => 0x2f,
        27 => 0x30,
        43 => 0x31,
        39 => 0x33,
        40 => 0x34,
        41 => 0x35,
        51 => 0x36,
        52 => 0x37,
        53 => 0x38,
        58 => 0x39,
        // F1–F10、F11/F12、F13–F24。
        59..=68 => (code - 59 + 0x3a) as u16,
        87 => 0x44,
        88 => 0x45,
        183..=194 => (code - 183 + 0x68) as u16,
        99 => 0x46,
        70 => 0x47,
        119 => 0x48,
        110 => 0x49,
        102 => 0x4a,
        104 => 0x4b,
        111 => 0x4c,
        107 => 0x4d,
        109 => 0x4e,
        106 => 0x4f,
        105 => 0x50,
        108 => 0x51,
        103 => 0x52,
        // 小键盘不合并到主键区，即使 NumLock 改变了 keyval。
        69 => 0x53,
        98 => 0x54,
        55 => 0x55,
        74 => 0x56,
        78 => 0x57,
        96 => 0x58,
        79 => 0x59,
        80 => 0x5a,
        81 => 0x5b,
        75 => 0x5c,
        76 => 0x5d,
        77 => 0x5e,
        71 => 0x5f,
        72 => 0x60,
        73 => 0x61,
        82 => 0x62,
        83 => 0x63,
        117 => 0x67,
        121 => 0x85,
        86 => 0x64,
        127 => 0x65,
        116 => 0x66,
        // 明确的国际/语言 usage；不是输入法或字符转换。
        89 => 0x87,
        93 => 0x88,
        124 => 0x89,
        92 => 0x8a,
        94 => 0x8b,
        95 => 0x8c,
        122 => 0x90,
        123 => 0x91,
        90 => 0x92,
        91 => 0x93,
        85 => 0x94,
        29 => 0xe0,
        42 => 0xe1,
        56 => 0xe2,
        125 => 0xe3,
        97 => 0xe4,
        54 => 0xe5,
        100 => 0xe6,
        126 => 0xe7,
        _ => return None,
    };
    Some(usage)
}
