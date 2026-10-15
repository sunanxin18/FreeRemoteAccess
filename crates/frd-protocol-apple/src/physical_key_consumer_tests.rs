// Task5C1A：有限 US 样式键盘合同的真实 writer 消费回归。
//
// 独立 oracle 来源（2026-10-15 核对，原始资料及 hash 存作者证据目录）：
// - USB HID Usage Tables 1.5，第 10 节 Keyboard/Keypad Page (0x07)：
//   https://usb.org/sites/default/files/hut1_5.pdf
// - X11 keysym 正式定义（固定 libxkbcommon 1.7.0 头，值兼容 X11）：
//   https://raw.githubusercontent.com/xkbcommon/libxkbcommon/xkbcommon-1.7.0/include/xkbcommon/xkbcommon-keysyms.h
// - RFC 6143 §7.5.4/7.5.5：KeyEvent/PointerEvent 字段及大端整数：
//   https://www.rfc-editor.org/rfc/rfc6143.html#section-7.5.4
//
// USB 只定义实体 usage；下表显式固定当前 US 样式合同，不证明国际布局、
// 系统锁态、设备输入、Mac 执行或 Text capability。期望不调用生产映射或 encoder。

use super::{read_encrypted_test_message, Duration, Read, TcpListener, TcpStream};
use crate::connection::AppleWriterHandle;
use crate::runtime::PointerWireState;
use crate::session::SessionCrypto;
use frd_core::{
    ButtonState, InputEvent, KeyState, Modifiers, PhysicalKeyCode, PixelPoint, PointerButton,
    ProtocolId,
};

// 每项为 usage、plain keysym、shift keysym；明确逐项列出全部 101 个 usage。
const US_101_ORACLE: [(u16, u32, u32); 101] = [
    (0x04, 0x0061, 0x0041), // a A
    (0x05, 0x0062, 0x0042), // b B
    (0x06, 0x0063, 0x0043), // c C
    (0x07, 0x0064, 0x0044), // d D
    (0x08, 0x0065, 0x0045), // e E
    (0x09, 0x0066, 0x0046), // f F
    (0x0a, 0x0067, 0x0047), // g G
    (0x0b, 0x0068, 0x0048), // h H
    (0x0c, 0x0069, 0x0049), // i I
    (0x0d, 0x006a, 0x004a), // j J
    (0x0e, 0x006b, 0x004b), // k K
    (0x0f, 0x006c, 0x004c), // l L
    (0x10, 0x006d, 0x004d), // m M
    (0x11, 0x006e, 0x004e), // n N
    (0x12, 0x006f, 0x004f), // o O
    (0x13, 0x0070, 0x0050), // p P
    (0x14, 0x0071, 0x0051), // q Q
    (0x15, 0x0072, 0x0052), // r R
    (0x16, 0x0073, 0x0053), // s S
    (0x17, 0x0074, 0x0054), // t T
    (0x18, 0x0075, 0x0055), // u U
    (0x19, 0x0076, 0x0056), // v V
    (0x1a, 0x0077, 0x0057), // w W
    (0x1b, 0x0078, 0x0058), // x X
    (0x1c, 0x0079, 0x0059), // y Y
    (0x1d, 0x007a, 0x005a), // z Z
    (0x1e, 0x0031, 0x0021), // 1 !
    (0x1f, 0x0032, 0x0040), // 2 @
    (0x20, 0x0033, 0x0023), // 3 #
    (0x21, 0x0034, 0x0024), // 4 $
    (0x22, 0x0035, 0x0025), // 5 %
    (0x23, 0x0036, 0x005e), // 6 ^
    (0x24, 0x0037, 0x0026), // 7 &
    (0x25, 0x0038, 0x002a), // 8 *
    (0x26, 0x0039, 0x0028), // 9 (
    (0x27, 0x0030, 0x0029), // 0 )
    (0x28, 0xff0d, 0xff0d), // Return
    (0x29, 0xff1b, 0xff1b), // Escape
    (0x2a, 0xff08, 0xff08), // BackSpace
    (0x2b, 0xff09, 0xff09), // Tab
    (0x2c, 0x0020, 0x0020), // space
    (0x2d, 0x002d, 0x005f), // - _
    (0x2e, 0x003d, 0x002b), // = +
    (0x2f, 0x005b, 0x007b), // [ {
    (0x30, 0x005d, 0x007d), // ] }
    (0x31, 0x005c, 0x007c), // backslash bar
    (0x33, 0x003b, 0x003a), // ; :
    (0x34, 0x0027, 0x0022), // apostrophe quotedbl
    (0x35, 0x0060, 0x007e), // grave asciitilde
    (0x36, 0x002c, 0x003c), // , <
    (0x37, 0x002e, 0x003e), // . >
    (0x38, 0x002f, 0x003f), // / ?
    (0x39, 0xffe5, 0xffe5), // Caps_Lock
    (0x3a, 0xffbe, 0xffbe), // F1
    (0x3b, 0xffbf, 0xffbf), // F2
    (0x3c, 0xffc0, 0xffc0), // F3
    (0x3d, 0xffc1, 0xffc1), // F4
    (0x3e, 0xffc2, 0xffc2), // F5
    (0x3f, 0xffc3, 0xffc3), // F6
    (0x40, 0xffc4, 0xffc4), // F7
    (0x41, 0xffc5, 0xffc5), // F8
    (0x42, 0xffc6, 0xffc6), // F9
    (0x43, 0xffc7, 0xffc7), // F10
    (0x44, 0xffc8, 0xffc8), // F11
    (0x45, 0xffc9, 0xffc9), // F12
    (0x46, 0xff61, 0xff61), // Print
    (0x47, 0xff14, 0xff14), // Scroll_Lock
    (0x49, 0xff63, 0xff63), // Insert
    (0x4a, 0xff50, 0xff50), // Home
    (0x4b, 0xff55, 0xff55), // Prior
    (0x4c, 0xffff, 0xffff), // Delete
    (0x4d, 0xff57, 0xff57), // End
    (0x4e, 0xff56, 0xff56), // Next
    (0x4f, 0xff53, 0xff53), // Right
    (0x50, 0xff51, 0xff51), // Left
    (0x51, 0xff54, 0xff54), // Down
    (0x52, 0xff52, 0xff52), // Up
    (0x53, 0xff7f, 0xff7f), // Num_Lock
    (0x54, 0xffaf, 0xffaf), // KP_Divide
    (0x55, 0xffaa, 0xffaa), // KP_Multiply
    (0x56, 0xffad, 0xffad), // KP_Subtract
    (0x57, 0xffab, 0xffab), // KP_Add
    (0x58, 0xff8d, 0xff8d), // KP_Enter
    (0x59, 0xffb1, 0xffb1), // KP_1
    (0x5a, 0xffb2, 0xffb2), // KP_2
    (0x5b, 0xffb3, 0xffb3), // KP_3
    (0x5c, 0xffb4, 0xffb4), // KP_4
    (0x5d, 0xffb5, 0xffb5), // KP_5
    (0x5e, 0xffb6, 0xffb6), // KP_6
    (0x5f, 0xffb7, 0xffb7), // KP_7
    (0x60, 0xffb8, 0xffb8), // KP_8
    (0x61, 0xffb9, 0xffb9), // KP_9
    (0x62, 0xffb0, 0xffb0), // KP_0
    (0x63, 0xffae, 0xffae), // KP_Decimal
    (0x65, 0xff67, 0xff67), // Menu
    (0xe0, 0xffe3, 0xffe3), // Control_L
    (0xe1, 0xffe1, 0xffe1), // Shift_L
    (0xe2, 0xffe9, 0xffe9), // Alt_L
    (0xe4, 0xffe4, 0xffe4), // Control_R
    (0xe5, 0xffe2, 0xffe2), // Shift_R
    (0xe6, 0xffea, 0xffea), // Alt_R
];

struct KeyConsumer {
    _connection: crate::AppleConnection,
    writer: AppleWriterHandle,
    state: PointerWireState,
    peer: TcpStream,
    peer_crypto: Option<SessionCrypto>,
}

impl KeyConsumer {
    fn new(encrypted: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut connection = crate::AppleConnection::new(stream);
        let peer_crypto = encrypted.then(|| {
            connection
                .set_crypto(SessionCrypto::from_key_iv([0x31; 16], [0x42; 16]))
                .unwrap();
            SessionCrypto::from_key_iv([0x31; 16], [0x42; 16])
        });
        let protocol = if encrypted {
            ProtocolId::apple_high_performance()
        } else {
            ProtocolId::apple_hpss_mvs()
        };
        Self {
            writer: connection.writer_handle().unwrap(),
            _connection: connection,
            state: PointerWireState::for_protocol(&protocol),
            peer,
            peer_crypto,
        }
    }

    fn handle(&mut self, event: InputEvent) {
        self.state.handle(event, &self.writer).unwrap();
    }

    fn key(&mut self, usage: u16, state: KeyState, modifiers: Modifiers) {
        self.handle(InputEvent::PhysicalKey {
            code: PhysicalKeyCode::from_usb_hid_usage(usage),
            state,
            modifiers,
        });
    }

    fn expect_message(&mut self, expected: &[u8]) {
        let actual = if let Some(crypto) = &mut self.peer_crypto {
            // 仅复用既有加密合同；不将其 inverse 当作独立密码学证明。
            read_encrypted_test_message(&mut self.peer, crypto)
        } else {
            let mut bytes = vec![0; expected.len()];
            self.peer.read_exact(&mut bytes).unwrap();
            bytes
        };
        assert_eq!(actual, expected);
    }

    fn expect_key(&mut self, down: bool, keysym: u32) {
        // 手写消费端 RFC 6143 布局，禁止调用生产 msg_key_event/encode_key_event。
        self.expect_message(&[
            4,
            u8::from(down),
            0,
            0,
            (keysym >> 24) as u8,
            (keysym >> 16) as u8,
            (keysym >> 8) as u8,
            keysym as u8,
        ]);
    }

    fn expect_quiet(&mut self) {
        // handle 等待真实 worker 写入结果；timeout 检查实际流，拒绝多余包及 EOF。
        self.peer
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut byte = [0];
        match self.peer.read(&mut byte) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            other => panic!("不应有旁侧输出或流关闭，实际: {other:?}，首字节: {byte:?}"),
        }
        self.peer
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
    }
}

impl Drop for KeyConsumer {
    fn drop(&mut self) {
        self.writer.shutdown().unwrap();
    }
}

fn shifted(shift: bool) -> Modifiers {
    Modifiers {
        shift,
        ..Modifiers::default()
    }
}

fn all_101_plain_and_shift(encrypted: bool) {
    let mut consumer = KeyConsumer::new(encrypted);
    let usages: Vec<_> = US_101_ORACLE.iter().map(|row| row.0).collect();
    let profile: Vec<_> = (0x04..=0x31)
        .chain(0x33..=0x47)
        .chain(0x49..=0x63)
        .chain([0x65, 0xe0, 0xe1, 0xe2, 0xe4, 0xe5, 0xe6])
        .collect();
    assert_eq!(usages, profile, "禁止扩展 Pause、国际/ISO、Meta 等资格");
    for shift in [false, true] {
        for &(usage, plain, shifted) in &US_101_ORACLE {
            let expected = if shift { shifted } else { plain };
            consumer.key(usage, KeyState::Pressed, self::shifted(shift));
            consumer.expect_key(true, expected);
            consumer.key(usage, KeyState::Released, self::shifted(shift));
            consumer.expect_key(false, expected);
        }
    }
    consumer.expect_quiet();
}

fn held_repeat_uses_initial_keysym(encrypted: bool) {
    let mut c = KeyConsumer::new(encrypted);
    c.key(0x04, KeyState::Pressed, shifted(false));
    c.expect_key(true, 0x61);
    c.key(0xe1, KeyState::Pressed, shifted(true));
    c.expect_key(true, 0xffe1);
    c.key(0x04, KeyState::Pressed, shifted(true));
    c.expect_key(true, 0x61);
    c.key(0xe1, KeyState::Released, shifted(false));
    c.expect_key(false, 0xffe1);
    c.key(0x04, KeyState::Released, shifted(true));
    c.expect_key(false, 0x61);
    c.key(0x04, KeyState::Released, shifted(false));
    c.expect_quiet();
    c.key(0x05, KeyState::Pressed, shifted(true));
    c.expect_key(true, 0x42);
    c.key(0x05, KeyState::Pressed, shifted(false));
    c.expect_key(true, 0x42);
    c.key(0x05, KeyState::Released, shifted(false));
    c.expect_key(false, 0x42);
    c.key(0x05, KeyState::Pressed, shifted(false));
    c.expect_key(true, 0x62);
    c.key(0x05, KeyState::Released, shifted(true));
    c.expect_key(false, 0x62);
    c.expect_quiet();
}

fn release_all_orders_keys_and_includes_pointer_tail(encrypted: bool) {
    let mut c = KeyConsumer::new(encrypted);
    for (usage, shift, keysym) in [
        (0xe1, true, 0xffe1),
        (0x05, true, 0x42),
        (0x04, false, 0x61),
    ] {
        c.key(usage, KeyState::Pressed, shifted(shift));
        c.expect_key(true, keysym);
    }
    c.handle(InputEvent::PointerMove {
        remote: PixelPoint {
            x: 0x1234,
            y: 0x0102,
        },
    });
    c.expect_message(&[5, 0, 0x12, 0x34, 1, 2]);
    c.handle(InputEvent::PointerButton {
        button: PointerButton::Primary,
        state: ButtonState::Pressed,
    });
    c.expect_message(&[5, 1, 0x12, 0x34, 1, 2]);
    c.handle(InputEvent::ReleaseAll);
    for keysym in [0x61, 0x42, 0xffe1] {
        c.expect_key(false, keysym);
    }
    c.expect_message(&[5, 0, 0x12, 0x34, 1, 2]);
    c.expect_quiet();
    c.handle(InputEvent::ReleaseAll);
    c.expect_message(&[5, 0, 0, 0, 0, 0]);
    c.expect_quiet();
    for usage in [0x04, 0x05, 0xe1] {
        c.key(usage, KeyState::Released, shifted(false));
    }
    c.expect_quiet();
    c.key(0x05, KeyState::Pressed, shifted(false));
    c.expect_key(true, 0x62);
    c.key(0x05, KeyState::Released, shifted(false));
    c.expect_key(false, 0x62);
    c.expect_quiet();
}

fn modifier_metadata_and_lock_keys_do_not_create_side_effects(encrypted: bool) {
    let mut c = KeyConsumer::new(encrypted);
    for shift in [false, true] {
        let metadata = Modifiers {
            shift,
            control: true,
            alt: true,
            meta: true,
        };
        c.key(0x04, KeyState::Pressed, metadata);
        c.expect_key(true, if shift { 0x41 } else { 0x61 });
        c.key(0x04, KeyState::Released, metadata);
        c.expect_key(false, if shift { 0x41 } else { 0x61 });
        c.expect_quiet();
    }
    for (usage, keysym) in [
        (0xe0, 0xffe3),
        (0xe1, 0xffe1),
        (0xe2, 0xffe9),
        (0xe4, 0xffe4),
        (0xe5, 0xffe2),
        (0xe6, 0xffea),
        (0x39, 0xffe5),
        (0x47, 0xff14),
        (0x53, 0xff7f),
    ] {
        c.key(usage, KeyState::Pressed, shifted(false));
        c.expect_key(true, keysym);
        c.key(usage, KeyState::Released, shifted(false));
        c.expect_key(false, keysym);
    }
    // 锁键按过后仍按显式 shift 元数据，主键盘与小键盘合同保持区分。
    for (usage, shift, keysym) in [
        (0x04, false, 0x61),
        (0x04, true, 0x41),
        (0x1e, false, 0x31),
        (0x1e, true, 0x21),
        (0x59, false, 0xffb1),
        (0x59, true, 0xffb1),
    ] {
        c.key(usage, KeyState::Pressed, shifted(shift));
        c.expect_key(true, keysym);
        c.key(usage, KeyState::Released, shifted(shift));
        c.expect_key(false, keysym);
    }
    c.expect_quiet();
}

fn unknown_orphan_and_text_do_not_pollute_held_keys(encrypted: bool) {
    let mut c = KeyConsumer::new(encrypted);
    c.key(0x04, KeyState::Pressed, shifted(false));
    c.expect_key(true, 0x61);
    for usage in [0x00, 0x03, 0x0100, 0xffff] {
        c.key(usage, KeyState::Pressed, shifted(true));
        c.key(usage, KeyState::Released, shifted(false));
    }
    c.key(0x05, KeyState::Released, shifted(true));
    c.handle(InputEvent::Text {
        utf8: "a中🙂\n".into(),
    });
    c.expect_quiet();
    c.key(0x04, KeyState::Pressed, shifted(true));
    c.expect_key(true, 0x61);
    c.handle(InputEvent::ReleaseAll);
    c.expect_key(false, 0x61);
    c.expect_message(&[5, 0, 0, 0, 0, 0]);
    c.expect_quiet();
    c.key(0x05, KeyState::Pressed, shifted(false));
    c.expect_key(true, 0x62);
    c.key(0x05, KeyState::Released, shifted(false));
    c.expect_key(false, 0x62);
    c.expect_quiet();
}

#[test]
fn standard_101_plain_and_shift_reach_writer_bytes() {
    all_101_plain_and_shift(false);
}

#[test]
fn hp_101_plain_and_shift_reach_writer_encrypted_plaintext() {
    all_101_plain_and_shift(true);
}

#[test]
fn standard_held_repeat_uses_initial_keysym() {
    held_repeat_uses_initial_keysym(false);
}

#[test]
fn hp_held_repeat_uses_initial_keysym() {
    held_repeat_uses_initial_keysym(true);
}

#[test]
fn standard_release_all_orders_keys_and_includes_pointer_tail() {
    release_all_orders_keys_and_includes_pointer_tail(false);
}

#[test]
fn hp_release_all_orders_keys_and_includes_pointer_tail() {
    release_all_orders_keys_and_includes_pointer_tail(true);
}

#[test]
fn standard_modifier_metadata_and_lock_keys_do_not_create_side_effects() {
    modifier_metadata_and_lock_keys_do_not_create_side_effects(false);
}

#[test]
fn hp_modifier_metadata_and_lock_keys_do_not_create_side_effects() {
    modifier_metadata_and_lock_keys_do_not_create_side_effects(true);
}

#[test]
fn standard_unknown_orphan_and_text_do_not_pollute_held_keys() {
    unknown_orphan_and_text_do_not_pollute_held_keys(false);
}

#[test]
fn hp_unknown_orphan_and_text_do_not_pollute_held_keys() {
    unknown_orphan_and_text_do_not_pollute_held_keys(true);
}
