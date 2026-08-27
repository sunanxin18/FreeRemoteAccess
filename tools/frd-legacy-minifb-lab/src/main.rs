//! 显式启动的旧 minifb/CPAL 串行对照工具。
//!
//! 此二进制只消费公共协议/帧/媒体端口。Apple socket、认证、密码学、
//! HPSS/MVS、UDP/SRTP/AAC 与动态分辨率状态全部由 `frd-protocol-apple` 拥有。

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use frd_core::{
    ContentViewport, Endpoint, InputEvent, PixelSize, PointerButtons, PointerInputState,
    PointerSample, SecretBuffer, SessionId, WheelDelta,
};
use frd_frame::FrameMailbox;
use frd_media_api::MediaFrame;
use frd_protocol_api::{
    ConnectRequest, Credentials, ProtocolExit, ProtocolFactory, ProtocolId, SessionCommand,
    SessionEvent,
};
use frd_protocol_apple::AppleProtocolFactory;
use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};

use frd_legacy_minifb_lab::audio_output::AudioPlayback;
use frd_legacy_minifb_lab::presenter::{render_nearest, LegacySurface};
use frd_legacy_minifb_lab::runtime_ports::{
    create_runtime_ports, send_input, send_pointer_sample, LabRuntimePorts,
};

const DEFAULT_USERNAME_ENV: &str = "FRD_USERNAME";
const DEFAULT_PASSWORD_ENV: &str = "FRD_PASSWORD";

#[derive(Parser)]
#[command(
    name = "frd-legacy-minifb-lab",
    version,
    about = "显式启动的 Apple HPSS/MVS minifb 串行对照工具（非产品入口）"
)]
struct Cli {
    /// Apple 屏幕共享主机；不得与另一 FreeRemoteDesk 实例并行运行
    host: String,
    #[arg(long, default_value_t = 5900)]
    port: u16,
    /// 提供 Mac 用户名的环境变量名；用户名本身不得放入 argv
    #[arg(long, value_name = "ENV", default_value = DEFAULT_USERNAME_ENV)]
    username_env: String,
    /// 提供 Mac 登录密码的环境变量名；密码本身不得放入 argv
    #[arg(long, value_name = "ENV", default_value = DEFAULT_PASSWORD_ENV)]
    password_env: String,
    /// 初始显示缩放：1.0 原始大小，0.5 为半尺寸
    #[arg(long, default_value_t = 1.0, value_parser = parse_display_scale)]
    scale: f32,
}

fn parse_display_scale(value: &str) -> std::result::Result<f32, String> {
    let scale = value
        .parse::<f32>()
        .map_err(|_| "显示缩放必须是有限数值，且满足 0 < scale <= 1".to_owned())?;
    if !(scale.is_finite() && 0.0 < scale && scale <= 1.0) {
        return Err("显示缩放必须是有限数值，且满足 0 < scale <= 1".to_owned());
    }
    Ok(scale)
}

fn read_required_credential_environment(name: &str, description: &str) -> Result<String> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest = chars.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if !valid_start || !valid_rest {
        bail!("{description}环境变量名无效");
    }

    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(std::env::VarError::NotPresent) => {
            bail!("缺少{description}：请通过指定的环境变量提供")
        }
        Err(std::env::VarError::NotUnicode(_)) => bail!("{description}环境变量不是有效 UTF-8"),
    }
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("legacy lab 错误: {error:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let username = read_required_credential_environment(&cli.username_env, "用户名")?;
    let password = read_required_credential_environment(&cli.password_env, "密码")?;
    let endpoint = Endpoint::new(cli.host, cli.port).context("Apple endpoint 无效")?;
    let session_id = SessionId::allocate();
    let mut password = SecretBuffer::new(password.into_bytes());
    let request = ConnectRequest {
        session_id,
        endpoint,
        protocol_id: ProtocolId::apple_hpss_mvs(),
        credentials: Some(Credentials {
            username,
            password: password.take(),
        }),
        saved_server_pin: None,
    };
    let LabRuntimePorts {
        runtime,
        commands,
        events,
        media,
        mailbox,
    } = create_runtime_ports(session_id);
    let session = AppleProtocolFactory
        .create(request, runtime)
        .map_err(|error| anyhow::anyhow!("Apple adapter 构造失败: {}", error.code()))?;
    let worker = std::thread::spawn(move || session.run());

    let ui_result = run_window(
        session_id, cli.scale, &commands, &events, &media, &mailbox, &worker,
    );
    let _ = commands.send(SessionCommand::Disconnect);
    drop(commands);
    let worker_result = join_worker(worker);
    match (ui_result, worker_result) {
        (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn run_window(
    session_id: SessionId,
    scale: f32,
    commands: &std::sync::mpsc::Sender<SessionCommand>,
    events: &Receiver<SessionEvent>,
    media: &Receiver<MediaFrame>,
    mailbox: &Arc<Mutex<FrameMailbox>>,
    worker: &JoinHandle<ProtocolExit>,
) -> Result<()> {
    let mut surface = LegacySurface::empty();
    let mut playback = None;
    while surface.size().is_none() && !worker.is_finished() {
        drain_mailbox(mailbox, &mut surface)?;
        drain_events(events);
        drain_media(media, &mut playback);
        if surface.size().is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let initial_size = surface
        .size()
        .context("Apple adapter 未发布初始 surface 即退出")?;
    let window_width = ((initial_size.width as f32 * scale).ceil() as usize).max(1);
    let window_height = ((initial_size.height as f32 * scale).ceil() as usize).max(1);
    let mut window = Window::new(
        "FreeRemoteDesk legacy minifb lab — 仅鼠标 · Ctrl+Q 退出",
        window_width,
        window_height,
        WindowOptions {
            resize: true,
            ..Default::default()
        },
    )?;
    window.set_target_fps(60);

    let mut scaled = Vec::new();
    let mut pointer_input = PointerInputState::default();
    let mut previous_buttons = PointerButtons::default();

    while window.is_open() && !worker.is_finished() {
        if drain_mailbox(mailbox, &mut surface)? {
            pointer_input = PointerInputState::default();
            previous_buttons = PointerButtons::default();
        }
        drain_events(events);
        drain_media(media, &mut playback);

        let window_size = window.get_size();
        let drawable = PixelSize::new(window_size.0.max(1) as u32, window_size.1.max(1) as u32)
            .expect("minifb drawable 已保证非零");
        render_nearest(&surface, drawable, &mut scaled);
        window.update_with_buffer(&scaled, drawable.width as usize, drawable.height as usize)?;

        let control_down = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
        if control_down && window.is_key_pressed(Key::Q, minifb::KeyRepeat::No) {
            break;
        }

        let generation = surface.generation();
        if generation == 0 {
            continue;
        }
        let pointer_position = if window.is_active() {
            window.get_mouse_pos(MouseMode::Discard)
        } else {
            None
        };
        let (sample, local_buttons_down) = if let Some((x, y)) = pointer_position {
            let buttons = PointerButtons {
                primary: window.get_mouse_down(MouseButton::Left),
                middle: window.get_mouse_down(MouseButton::Middle),
                secondary: window.get_mouse_down(MouseButton::Right),
            };
            let mut wheel = WheelDelta::default();
            if let Some((horizontal, vertical)) = window.get_scroll_wheel() {
                wheel.horizontal = horizontal.signum() as i8;
                wheel.vertical = vertical.signum() as i8;
            }
            let remote = surface.size().expect("generation 非零时 surface 已 reset");
            let viewport = ContentViewport::fit(remote, drawable);
            (
                viewport
                    .map_pointer(x, y)
                    .map(|remote| PointerSample::new(remote, buttons, wheel)),
                buttons.any_pressed(),
            )
        } else {
            (None, false)
        };
        if let Some(sample) = pointer_input.next_event(sample, local_buttons_down) {
            send_pointer_sample(commands, session_id, generation, sample)?;
            previous_buttons = sample.buttons;
        }
    }

    if surface.generation() != 0 && previous_buttons.any_pressed() {
        let _ = send_input(
            commands,
            session_id,
            surface.generation(),
            InputEvent::ReleaseAll,
        );
    }
    Ok(())
}

fn drain_mailbox(mailbox: &Arc<Mutex<FrameMailbox>>, surface: &mut LegacySurface) -> Result<bool> {
    let mut generation_changed = false;
    loop {
        let update = mailbox
            .lock()
            .map_err(|_| anyhow::anyhow!("frame mailbox 锁已损坏"))?
            .pop();
        let Some(update) = update else {
            return Ok(generation_changed);
        };
        generation_changed |= surface.apply(update)?;
    }
}

fn drain_events(events: &Receiver<SessionEvent>) {
    loop {
        match events.try_recv() {
            Ok(SessionEvent::Error(error)) => {
                eprintln!("[legacy-lab] Apple adapter error: {}", error.code());
            }
            Ok(_) => {}
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
        }
    }
}

fn drain_media(media: &Receiver<MediaFrame>, playback: &mut Option<AudioPlayback>) {
    loop {
        match media.try_recv() {
            Ok(MediaFrame::Pcm {
                sample_rate_hz: 48_000,
                channels: 2,
                samples,
            }) => {
                if playback.is_none() {
                    match AudioPlayback::open_default() {
                        Ok(output) => {
                            eprintln!(
                                "[legacy-audio] 使用输出设备: {}",
                                output.device_description()
                            );
                            *playback = Some(output);
                        }
                        Err(error) => {
                            eprintln!("[legacy-audio] 默认输出不可用，继续无声会话: {error:#}");
                            continue;
                        }
                    }
                }
                if let Some(output) = playback {
                    if let Err(error) = output.enqueue_interleaved_stereo(&samples) {
                        eprintln!("[legacy-audio] 播放队列拒绝 PCM，继续无声会话: {error:#}");
                        *playback = None;
                    }
                }
            }
            Ok(MediaFrame::Pcm { .. } | MediaFrame::EncodedVideo { .. }) => {}
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
        }
    }
}

fn join_worker(worker: JoinHandle<ProtocolExit>) -> Result<()> {
    match worker.join() {
        Ok(ProtocolExit::Closed) => Ok(()),
        Ok(ProtocolExit::Failed(error)) => {
            Err(anyhow::anyhow!("Apple adapter 已失败: {}", error.code()))
        }
        Err(_) => Err(anyhow::anyhow!("Apple adapter worker 异常退出")),
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::Cli;

    #[test]
    fn cli_has_no_literal_username_or_password_option() {
        let command = Cli::command();
        let ids = command
            .get_arguments()
            .map(|argument| argument.get_id().as_str().to_owned())
            .collect::<Vec<_>>();

        assert!(!ids.iter().any(|id| id == "username" || id == "password"));
        assert!(ids.iter().any(|id| id == "username_env"));
        assert!(ids.iter().any(|id| id == "password_env"));
    }

    #[test]
    fn cli_rejects_out_of_range_display_scale() {
        assert!(
            Cli::try_parse_from(["frd-legacy-minifb-lab", "example.invalid", "--scale", "0"])
                .is_err()
        );
    }
}
