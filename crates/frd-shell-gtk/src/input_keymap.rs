//! GTK 硬件键码到 USB HID Keyboard/Keypad usage 的显式映射。
//!
//! 仅适用于宿主已确认采用 Linux evdev + 8 的配置；不是根据 X11/Wayland
//! 名称自动推断键码体系。自定义 XKB keycodes 需要单独的宿主映射。
//! 固定证据：
//! - https://github.com/GNOME/gtk/blob/4.14.0/gdk/wayland/gdkseat-wayland.c#L1437
//! - https://github.com/GNOME/gtk/blob/4.14.0/gdk/x11/gdkdevicemanager-xi2.c#L1584
//! - https://github.com/torvalds/linux/blob/v6.8/drivers/hid/hid-input.c#L27
//! - https://github.com/torvalds/linux/blob/v6.8/include/uapi/linux/input-event-codes.h
//!
//! 内核 HID→evdev 表不是单射：KEY_BACKSLASH 43 同时来自 usage 0x31/0x32。
//! 此处明确选择标准 Keyboard Backslash 0x31，不能恢复原设备的 0x32 身份。
//! KEY_DELETE 明确选择标准 Forward Delete 0x4c；不推断重复的扩展 usage。
//! 国际键只覆盖表中明确的 102ND/RO/YEN/HENKAN/MUHENKAN/KATAKANAHIRAGANA、
//! HANGEUL/HANJA/KATAKANA/HIRAGANA/ZENKAKUHANKAKU。消费页媒体键及未知键拒绝。

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
