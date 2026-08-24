//! minifb 按键 → X11 keysym 映射。RFB 的 KeyEvent 使用 X11 键码空间，
//! 远端的 macOS 会把它转换成对应的 Mac 按键。

use minifb::Key;

pub fn to_keysym(key: Key, shift: bool) -> Option<u32> {
    use Key::*;
    let ks = match key {
        // 字母
        A => letter(0x61, shift),
        B => letter(0x62, shift),
        C => letter(0x63, shift),
        D => letter(0x64, shift),
        E => letter(0x65, shift),
        F => letter(0x66, shift),
        G => letter(0x67, shift),
        H => letter(0x68, shift),
        I => letter(0x69, shift),
        J => letter(0x6a, shift),
        K => letter(0x6b, shift),
        L => letter(0x6c, shift),
        M => letter(0x6d, shift),
        N => letter(0x6e, shift),
        O => letter(0x6f, shift),
        P => letter(0x70, shift),
        Q => letter(0x71, shift),
        R => letter(0x72, shift),
        S => letter(0x73, shift),
        T => letter(0x74, shift),
        U => letter(0x75, shift),
        V => letter(0x76, shift),
        W => letter(0x77, shift),
        X => letter(0x78, shift),
        Y => letter(0x79, shift),
        Z => letter(0x7a, shift),
        // 主键盘数字（含 Shift 符号）
        Key0 => {
            if shift {
                0x29
            } else {
                0x30
            }
        } // )
        Key1 => {
            if shift {
                0x21
            } else {
                0x31
            }
        } // !
        Key2 => {
            if shift {
                0x40
            } else {
                0x32
            }
        } // @
        Key3 => {
            if shift {
                0x23
            } else {
                0x33
            }
        } // #
        Key4 => {
            if shift {
                0x24
            } else {
                0x34
            }
        } // $
        Key5 => {
            if shift {
                0x25
            } else {
                0x35
            }
        } // %
        Key6 => {
            if shift {
                0x5e
            } else {
                0x36
            }
        } // ^
        Key7 => {
            if shift {
                0x26
            } else {
                0x37
            }
        } // &
        Key8 => {
            if shift {
                0x2a
            } else {
                0x38
            }
        } // *
        Key9 => {
            if shift {
                0x28
            } else {
                0x39
            }
        } // (
        // 标点
        Space => 0x20,
        Apostrophe => {
            if shift {
                0x22
            } else {
                0x27
            }
        } // " '
        Comma => {
            if shift {
                0x3c
            } else {
                0x2c
            }
        } // < ,
        Minus => {
            if shift {
                0x5f
            } else {
                0x2d
            }
        } // _ -
        Period => {
            if shift {
                0x3e
            } else {
                0x2e
            }
        } // > .
        Slash => {
            if shift {
                0x3f
            } else {
                0x2f
            }
        } // ? /
        Semicolon => {
            if shift {
                0x3a
            } else {
                0x3b
            }
        } // : ;
        Equal => {
            if shift {
                0x2b
            } else {
                0x3d
            }
        } // + =
        LeftBracket => {
            if shift {
                0x7b
            } else {
                0x5b
            }
        } // { [
        Backslash => {
            if shift {
                0x7c
            } else {
                0x5c
            }
        } // | \
        RightBracket => {
            if shift {
                0x7d
            } else {
                0x5d
            }
        } // } ]
        Backquote => {
            if shift {
                0x7e
            } else {
                0x60
            }
        } // ~ `
        // 控制键
        Enter => 0xff0d,
        Backspace => 0xff08,
        Tab => 0xff09,
        Escape => 0xff1b,
        Insert => 0xff63,
        Delete => 0xffff,
        Home => 0xff50,
        End => 0xff57,
        PageUp => 0xff55,
        PageDown => 0xff56,
        Left => 0xff51,
        Up => 0xff52,
        Right => 0xff53,
        Down => 0xff54,
        // 功能键 F1..F12 = 0xffbe..
        F1 => 0xffbe,
        F2 => 0xffbf,
        F3 => 0xffc0,
        F4 => 0xffc1,
        F5 => 0xffc2,
        F6 => 0xffc3,
        F7 => 0xffc4,
        F8 => 0xffc5,
        F9 => 0xffc6,
        F10 => 0xffc7,
        F11 => 0xffc8,
        F12 => 0xffc9,
        // 修饰键（macOS 上 Ctrl=Control, Alt/Option=Option, Super=Cmd）
        LeftShift => 0xffe1,
        RightShift => 0xffe2,
        LeftCtrl => 0xffe3,
        RightCtrl => 0xffe4,
        LeftAlt => 0xffe9,
        RightAlt => 0xffea,
        LeftSuper => 0xffe7,
        RightSuper => 0xffe8,
        // 小键盘
        NumPad0 => 0xffb0,
        NumPad1 => 0xffb1,
        NumPad2 => 0xffb2,
        NumPad3 => 0xffb3,
        NumPad4 => 0xffb4,
        NumPad5 => 0xffb5,
        NumPad6 => 0xffb6,
        NumPad7 => 0xffb7,
        NumPad8 => 0xffb8,
        NumPad9 => 0xffb9,
        NumPadDot => 0xffae,
        NumPadSlash => 0xffaf,
        NumPadAsterisk => 0xffaa,
        NumPadMinus => 0xffad,
        NumPadPlus => 0xffab,
        NumPadEnter => 0xff8d,
        _ => return None,
    };
    Some(ks)
}

fn letter(lower: u32, shift: bool) -> u32 {
    if shift {
        lower - 0x20
    } else {
        lower
    }
}
