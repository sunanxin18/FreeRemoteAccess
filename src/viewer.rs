//! 实时查看器：一条读线程阻塞解析 RFB 服务器消息并更新共享帧缓冲；
//! 主线程跑 minifb 窗口，负责渲染并把键鼠事件编码成 RFB 消息回传。

use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{bail, Context, Result};
use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};

use crate::framebuffer::Framebuffer;
use crate::keysym;
use crate::pointer_input::{PointerInputState, PointerSample};
use crate::vnc::client::{self, ServerEvent, VncClient};
use crate::vnc::protocol;
use crate::vnc::session::SessionCrypto;

/// 统一发送：加密会话时封帧（与 RfbConn 共享同一发送状态），明文直写
fn send(
    write_stream: &Mutex<std::net::TcpStream>,
    crypto: &Option<Arc<Mutex<SessionCrypto>>>,
    msg: &[u8],
) -> Result<()> {
    let mut w = write_stream.lock().unwrap();
    match crypto {
        Some(c) => {
            let wire = c.lock().unwrap().seal(msg)?;
            w.write_all(&wire).context("写入失败（连接中断？）")
        }
        None => w.write_all(msg).context("写入失败（连接中断？）"),
    }
}

pub fn run(client: VncClient, scale: f32) -> Result<()> {
    let VncClient {
        conn,
        width,
        height,
        name,
        ..
    } = client;

    let crypto = conn.crypto_handle();
    let write_stream = Arc::new(Mutex::new(conn.try_clone().context("无法复制 socket")?));
    let fb = Arc::new(Mutex::new(Framebuffer::new(
        width as usize,
        height as usize,
    )?));
    let closing = Arc::new(AtomicBool::new(false));
    let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // 像素格式 / 编码 / 初始全量更新请求。
    // 加密会话（Apple 会话层）内不发 SetPixelFormat/SetEncodings（会被服务器拒绝，
    // 编码表已随 cmd=1 协商消息下发）。
    {
        if crypto.is_none() {
            let mut w = write_stream.lock().unwrap();
            w.write_all(&protocol::msg_set_pixel_format(
                &protocol::PixelFormat::OURS,
            ))?;
            w.write_all(&protocol::msg_set_encodings(protocol::SUPPORTED_ENCODINGS)?)?;
        }
        send(
            &write_stream,
            &crypto,
            &protocol::msg_fb_update_request(false, 0, 0, width, height),
        )?;
    }

    // 读线程：驱动 “收到更新 -> 继续请求增量更新” 的循环
    {
        let fb = fb.clone();
        let write_stream = write_stream.clone();
        let crypto = crypto.clone();
        let closing = closing.clone();
        let error_slot = error_slot.clone();
        thread::spawn(move || {
            let mut conn = conn;
            loop {
                match client::read_server_message(&mut conn) {
                    Ok(ServerEvent::Update(ops)) => {
                        fb.lock().unwrap().apply(&ops);
                        let req = protocol::msg_fb_update_request(true, 0, 0, width, height);
                        if let Err(e) = send(&write_stream, &crypto, &req) {
                            if !closing.load(Ordering::Relaxed) {
                                *error_slot.lock().unwrap() =
                                    Some(format!("发送更新请求失败: {e}"));
                            }
                            break;
                        }
                    }
                    Ok(ServerEvent::Bell) => eprintln!("\x07[远程] 响铃"),
                    Ok(ServerEvent::ServerCutText(t)) => eprintln!("[远程] 剪贴板文本: {t}"),
                    Ok(ServerEvent::Ignored) => {}
                    // HPSS 编码：viewer 不渲染，忽略（HPSS 由 hpss 子命令消费）
                    Err(e) => {
                        if !closing.load(Ordering::Relaxed) {
                            *error_slot.lock().unwrap() = Some(format!("{e:#}"));
                        }
                        break;
                    }
                }
            }
        });
    }

    let scale = if scale <= 0.0 { 1.0 } else { scale };
    let vw = ((width as f32 * scale).ceil() as usize).max(1);
    let vh = ((height as f32 * scale).ceil() as usize).max(1);
    let mut window = Window::new(
        &format!("FreeRemoteDesk — {name}  [{width}x{height}  Ctrl+Q 退出]"),
        vw,
        vh,
        WindowOptions {
            ..Default::default()
        },
    )?;
    window.set_target_fps(60);

    let mut scaled: Vec<u32> = vec![0; vw * vh];
    let mut pressed: HashSet<Key> = HashSet::new();
    let mut pointer_input = PointerInputState::default();

    loop {
        // 渲染最新一帧
        {
            let fb = fb.lock().unwrap();
            if scale == 1.0 && fb.width == vw && fb.height == vh {
                window.update_with_buffer(fb.pixels(), vw, vh)?;
            } else {
                downsample(fb.pixels(), fb.width, fb.height, &mut scaled, vw, vh);
                window.update_with_buffer(&scaled, vw, vh)?;
            }
        }

        let quit_hotkey = {
            let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
            ctrl && window.is_key_pressed(Key::Q, minifb::KeyRepeat::No)
        };
        if !window.is_open() || quit_hotkey {
            break;
        }
        if let Some(err) = error_slot.lock().unwrap().take() {
            bail!("远程连接中断: {err}");
        }

        // 键盘：与上一帧做差分，按下/抬起各发一次 KeyEvent
        let now: HashSet<Key> = window.get_keys().into_iter().collect();
        let shift = now.contains(&Key::LeftShift) || now.contains(&Key::RightShift);
        let mut key_msgs = Vec::new();
        for k in &now {
            if !pressed.contains(k) {
                if let Some(ks) = keysym::to_keysym(*k, shift) {
                    key_msgs.push(protocol::msg_key_event(true, ks));
                }
            }
        }
        for k in &pressed {
            if !now.contains(k) {
                if let Some(ks) = keysym::to_keysym(*k, false) {
                    key_msgs.push(protocol::msg_key_event(false, ks));
                }
            }
        }
        pressed = now;
        for m in &key_msgs {
            send(&write_stream, &crypto, m)?;
        }

        // 鼠标：仅窗口激活且指针位于客户区内时采样输入。
        let pointer_position = if window.is_active() {
            window.get_mouse_pos(MouseMode::Discard)
        } else {
            None
        };
        let (sample, local_buttons_down) = if let Some((mx, my)) = pointer_position {
            let mut mask = 0u8;
            if window.get_mouse_down(MouseButton::Left) {
                mask |= protocol::pointer::PRIMARY;
            }
            if window.get_mouse_down(MouseButton::Middle) {
                mask |= protocol::pointer::MIDDLE;
            }
            if window.get_mouse_down(MouseButton::Right) {
                mask |= protocol::pointer::SECONDARY;
            }
            let local_buttons_down = mask != 0;
            if let Some((wx, wy)) = window.get_scroll_wheel() {
                if wy > 0.0 {
                    mask |= protocol::pointer::WHEEL_UP;
                } else if wy < 0.0 {
                    mask |= protocol::pointer::WHEEL_DOWN;
                }
                if wx > 0.0 {
                    mask |= protocol::pointer::WHEEL_RIGHT;
                } else if wx < 0.0 {
                    mask |= protocol::pointer::WHEEL_LEFT;
                }
            }
            let x = ((mx / scale) as i32).clamp(0, width as i32 - 1) as u16;
            let y = ((my / scale) as i32).clamp(0, height as i32 - 1) as u16;
            (Some(PointerSample::new(x, y, mask)), local_buttons_down)
        } else {
            (None, false)
        };
        if let Some(event) = pointer_input.next_event(sample, local_buttons_down) {
            let msg = protocol::msg_pointer_event(event.mask, event.x, event.y);
            send(&write_stream, &crypto, &msg)?;
        }
    }

    closing.store(true, Ordering::Relaxed);
    Ok(())
}

/// 最近邻降采样（远端分辨率大于本地窗口时使用）
fn downsample(src: &[u32], sw: usize, sh: usize, dst: &mut [u32], dw: usize, dh: usize) {
    for y in 0..dh {
        let sy = (y * sh / dh).min(sh - 1);
        let row = sy * sw;
        for x in 0..dw {
            let sx = (x * sw / dw).min(sw - 1);
            dst[y * dw + x] = src[row + sx];
        }
    }
}
