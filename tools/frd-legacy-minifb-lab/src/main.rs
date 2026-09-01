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
    ConnectRequest, Credentials, ProtocolExit, ProtocolFactory, ProtocolId, ProtocolRuntime,
    ProtocolSession, SessionCommand, SessionEvent,
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

fn validate_environment_name(name: &str, description: &str) -> Result<()> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest = chars.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if !valid_start || !valid_rest {
        bail!("{description}环境变量名无效");
    }
    Ok(())
}

fn read_required_username_environment(name: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(std::env::VarError::NotPresent) => {
            bail!("缺少用户名：请通过指定的环境变量提供")
        }
        Err(std::env::VarError::NotUnicode(_)) => bail!("用户名环境变量不是有效 UTF-8"),
    }
}

fn read_required_password_environment(name: &str) -> Result<SecretBuffer> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(SecretBuffer::new(value.into_bytes())),
        Ok(_) | Err(std::env::VarError::NotPresent) => {
            bail!("缺少密码：请通过指定的环境变量提供")
        }
        Err(std::env::VarError::NotUnicode(_)) => bail!("密码环境变量不是有效 UTF-8"),
    }
}

fn build_connect_request_with<U, P>(
    cli: &Cli,
    username_provider: U,
    password_provider: P,
) -> Result<ConnectRequest>
where
    U: FnOnce(&str) -> Result<String>,
    P: FnOnce(&str) -> Result<SecretBuffer>,
{
    let endpoint = Endpoint::new(cli.host.clone(), cli.port).context("Apple endpoint 无效")?;
    validate_environment_name(&cli.username_env, "用户名")?;
    validate_environment_name(&cli.password_env, "密码")?;
    let username = username_provider(&cli.username_env)
        .map_err(|_| anyhow::anyhow!("读取用户名环境变量失败"))?;
    let mut password = password_provider(&cli.password_env)
        .map_err(|_| anyhow::anyhow!("读取密码环境变量失败"))?;
    Ok(ConnectRequest {
        session_id: SessionId::allocate(),
        endpoint,
        protocol_id: ProtocolId::apple_hpss_mvs(),
        credentials: Some(Credentials {
            username,
            password: password.take(),
        }),
        saved_server_pin: None,
    })
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("legacy lab 错误: {error:#}");
        std::process::exit(1);
    }
}

fn create_apple_session(
    request: ConnectRequest,
    runtime: ProtocolRuntime,
) -> Result<Box<dyn ProtocolSession>> {
    AppleProtocolFactory
        .create(request, runtime)
        .map_err(|error| anyhow::anyhow!("Apple adapter 构造失败: {}", error.code()))
}

fn run(cli: Cli) -> Result<()> {
    let request = build_connect_request_with(
        &cli,
        read_required_username_environment,
        read_required_password_environment,
    )?;
    let session_id = request.session_id;
    let LabRuntimePorts {
        runtime,
        commands,
        events,
        media,
        mailbox,
    } = create_runtime_ports(session_id);
    let session = create_apple_session(request, runtime)?;
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
        let generation_changed = drain_mailbox(mailbox, &mut surface)?;
        reset_pointer_gate_after_generation_change(
            generation_changed,
            &mut pointer_input,
            &mut previous_buttons,
        );
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

fn reset_pointer_gate_after_generation_change(
    generation_changed: bool,
    pointer_input: &mut PointerInputState,
    previous_buttons: &mut PointerButtons,
) {
    if generation_changed {
        *pointer_input = PointerInputState::default();
        *previous_buttons = PointerButtons::default();
    }
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
            Ok(
                MediaFrame::Pcm { .. } | MediaFrame::VideoConfig(_) | MediaFrame::EncodedVideo(_),
            ) => {}
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
    use std::cell::Cell;
    use std::sync::{mpsc, Arc, Mutex};

    use clap::{CommandFactory, Parser};
    use frd_core::secret::wipe_test_observer::{reset_wipe_observation, take_wipe_observation};
    use frd_core::{
        InputEvent, PixelPoint, PixelSize, PointerButtons, PointerInputState, PointerSample,
        SecretBuffer, SessionId, WheelDelta,
    };
    use frd_frame::{FrameMailbox, PixelFormat, PushOutcome, SurfaceUpdate};
    use frd_protocol_api::SessionCommand;

    use frd_legacy_minifb_lab::presenter::LegacySurface;
    use frd_legacy_minifb_lab::runtime_ports::{create_runtime_ports, send_pointer_sample};

    use super::{
        build_connect_request_with, create_apple_session, drain_mailbox,
        reset_pointer_gate_after_generation_change, Cli,
    };

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

    #[test]
    fn endpoint_validation_precedes_all_credential_provider_access() {
        let cli = Cli::try_parse_from(["frd-legacy-minifb-lab", "example.invalid", "--port", "0"])
            .unwrap();
        let provider_accesses = Cell::new(0_u8);

        let result = build_connect_request_with(
            &cli,
            |_| {
                provider_accesses.set(provider_accesses.get() + 1);
                Ok("developer".to_owned())
            },
            |_| {
                provider_accesses.set(provider_accesses.get() + 1);
                Ok(SecretBuffer::new(b"password-canary".to_vec()))
            },
        );
        let error = match result {
            Ok(_) => panic!("非法 endpoint 不得读取凭据"),
            Err(error) => error,
        };

        assert_eq!(provider_accesses.get(), 0);
        assert_eq!(error.to_string(), "Apple endpoint 无效");
    }

    #[test]
    fn post_provider_failure_drops_zeroized_secret_without_error_canary() {
        let cli = Cli::try_parse_from(["frd-legacy-minifb-lab", "example.invalid"]).unwrap();
        let canary = b"post-provider-password-canary";
        let mut request = build_connect_request_with(
            &cli,
            |_| Ok("developer".to_owned()),
            |_| Ok(SecretBuffer::new(canary.to_vec())),
        )
        .expect("有效的公共参数与 provider 必须产生连接请求");
        let session_id = request.session_id;
        request.protocol_id = frd_core::ProtocolId::rdp();
        let ports = create_runtime_ports(session_id);

        reset_wipe_observation();
        let error = match create_apple_session(request, ports.runtime) {
            Ok(_) => panic!("错误协议必须在 provider 后由 Apple factory 拒绝"),
            Err(error) => error,
        };

        assert_eq!(take_wipe_observation(), Some(vec![0; canary.len()]));
        assert!(!error.to_string().contains("post-provider-password-canary"));
    }

    #[test]
    fn generation_reset_disarms_held_pointer_until_release_and_new_press() {
        let session_id = SessionId::allocate();
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(8, 1024)));
        let mut surface = LegacySurface::empty();
        let mut pointer_input = PointerInputState::default();
        let mut previous_buttons = PointerButtons::default();
        let (commands, received) = mpsc::channel();

        assert_eq!(
            mailbox.lock().unwrap().push(SurfaceUpdate::Reset {
                session_id,
                generation: 1,
                size: PixelSize::new(2, 2).unwrap(),
                format: PixelFormat::Bgrx8UnormSrgb,
            }),
            PushOutcome::Queued
        );
        let generation_changed = drain_mailbox(&mailbox, &mut surface).unwrap();
        reset_pointer_gate_after_generation_change(
            generation_changed,
            &mut pointer_input,
            &mut previous_buttons,
        );
        assert_eq!(surface.generation(), 1);

        let released_gen1 = PointerSample::new(
            PixelPoint { x: 1, y: 1 },
            PointerButtons::default(),
            WheelDelta::default(),
        );
        let pressed_gen1 = PointerSample::new(
            PixelPoint { x: 1, y: 1 },
            PointerButtons {
                primary: true,
                ..Default::default()
            },
            WheelDelta::default(),
        );
        let sample = pointer_input
            .next_event(Some(released_gen1), false)
            .unwrap();
        send_pointer_sample(&commands, session_id, surface.generation(), sample).unwrap();
        previous_buttons = sample.buttons;
        assert_eq!(previous_buttons, PointerButtons::default());
        let sample = pointer_input.next_event(Some(pressed_gen1), true).unwrap();
        send_pointer_sample(&commands, session_id, surface.generation(), sample).unwrap();
        previous_buttons = sample.buttons;
        assert!(matches!(received.recv().unwrap(), SessionCommand::Input(_)));
        assert!(matches!(
            received.recv().unwrap(),
            SessionCommand::Input(frd_core::SessionInput {
                generation: 1,
                event: InputEvent::PointerSample(observed),
                ..
            }) if observed == pressed_gen1
        ));
        assert!(previous_buttons.primary);

        assert_eq!(
            mailbox.lock().unwrap().push(SurfaceUpdate::Reset {
                session_id,
                generation: 2,
                size: PixelSize::new(4, 4).unwrap(),
                format: PixelFormat::Bgrx8UnormSrgb,
            }),
            PushOutcome::Queued
        );
        let generation_changed = drain_mailbox(&mailbox, &mut surface).unwrap();
        reset_pointer_gate_after_generation_change(
            generation_changed,
            &mut pointer_input,
            &mut previous_buttons,
        );
        assert!(generation_changed);
        assert_eq!(surface.generation(), 2);
        assert_eq!(previous_buttons, PointerButtons::default());

        let held_gen2 = PointerSample::new(
            PixelPoint { x: 3, y: 3 },
            PointerButtons {
                primary: true,
                ..Default::default()
            },
            WheelDelta::default(),
        );
        assert_eq!(pointer_input.next_event(Some(held_gen2), true), None);
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let released_gen2 = PointerSample::new(
            PixelPoint { x: 3, y: 3 },
            PointerButtons::default(),
            WheelDelta::default(),
        );
        let sample = pointer_input
            .next_event(Some(released_gen2), false)
            .expect("观察到全释放后必须重新 arm");
        send_pointer_sample(&commands, session_id, surface.generation(), sample).unwrap();
        previous_buttons = sample.buttons;
        assert_eq!(previous_buttons, PointerButtons::default());
        assert!(matches!(
            received.recv().unwrap(),
            SessionCommand::Input(frd_core::SessionInput {
                generation: 2,
                event: InputEvent::PointerSample(observed),
                ..
            }) if observed == released_gen2
        ));

        let new_press_gen2 = PointerSample::new(
            PixelPoint { x: 3, y: 3 },
            PointerButtons {
                primary: true,
                ..Default::default()
            },
            WheelDelta::default(),
        );
        let sample = pointer_input
            .next_event(Some(new_press_gen2), true)
            .expect("只有新的本地按下边沿可以进入 generation 2");
        send_pointer_sample(&commands, session_id, surface.generation(), sample).unwrap();
        previous_buttons = sample.buttons;
        assert!(matches!(
            received.recv().unwrap(),
            SessionCommand::Input(frd_core::SessionInput {
                generation: 2,
                event: InputEvent::PointerSample(observed),
                ..
            }) if observed == new_press_gen2
        ));
        assert!(previous_buttons.primary);
    }
}
