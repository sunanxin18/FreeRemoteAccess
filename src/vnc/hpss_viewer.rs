//! Legacy minifb presentation wrapper for the Apple adapter runtime.
//!
//! HPSS/MVS/dynamic/media state lives in `frd-protocol-apple`. This module is
//! limited to platform presentation, device playback, and translation of
//! minifb events into protocol-neutral commands.

use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use frd_core::{
    ButtonState, ContentViewport, InputEvent, PhysicalViewport, PixelSize, PointerButton,
    PointerButtons, PointerInputState, PointerSample, SessionId, SessionInput, WheelDelta,
};
use frd_frame::{FrameMailbox, PixelFormat, SurfaceUpdate};
use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};
use frd_protocol_api::{
    MailboxSurfacePublisher, ProtocolError, ProtocolExit, ProtocolRuntime, RuntimeEventSink,
    RuntimeWake, SessionCommand, SessionEvent,
};
use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};

use crate::framebuffer::Framebuffer;
use crate::vnc::audio_io::AudioPlayback;
use crate::vnc::client::RfbConn;
use crate::vnc::media_negotiation::AudioMediaFlow;

const FRAME_MAILBOX_ENTRIES: usize = 64;
const FRAME_MAILBOX_PIXEL_BYTES: usize = 256 * 1024 * 1024;
const MEDIA_QUEUE_ENTRIES: usize = 16;

struct EventChannel(mpsc::Sender<SessionEvent>);

impl RuntimeEventSink for EventChannel {
    fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
        self.0.send(event).map_err(|_| ProtocolError::Terminal)
    }
}

struct NoopWake;

impl RuntimeWake for NoopWake {
    fn wake(&self) -> Result<(), ProtocolError> {
        Ok(())
    }
}

struct MediaChannel(SyncSender<MediaFrame>);

impl MediaPublisher for MediaChannel {
    fn publish(&self, frame: MediaFrame) -> Result<(), MediaPublishError> {
        self.0.try_send(frame).map_err(|error| match error {
            TrySendError::Full(_) => MediaPublishError::Full,
            TrySendError::Disconnected(_) => MediaPublishError::Closed,
        })
    }
}

fn send_input(
    commands: &mpsc::Sender<SessionCommand>,
    session_id: SessionId,
    generation: u64,
    event: InputEvent,
) -> Result<()> {
    commands
        .send(SessionCommand::Input(SessionInput {
            session_id,
            generation,
            event,
        }))
        .context("Apple adapter input channel closed")
}

fn send_pointer_sample(
    commands: &mpsc::Sender<SessionCommand>,
    session_id: SessionId,
    generation: u64,
    sample: PointerSample,
    previous_buttons: &mut PointerButtons,
) -> Result<()> {
    send_input(
        commands,
        session_id,
        generation,
        InputEvent::PointerMove {
            remote: sample.remote,
        },
    )?;
    for (button, before, after) in [
        (
            PointerButton::Primary,
            previous_buttons.primary,
            sample.buttons.primary,
        ),
        (
            PointerButton::Middle,
            previous_buttons.middle,
            sample.buttons.middle,
        ),
        (
            PointerButton::Secondary,
            previous_buttons.secondary,
            sample.buttons.secondary,
        ),
    ] {
        if before != after {
            send_input(
                commands,
                session_id,
                generation,
                InputEvent::PointerButton {
                    button,
                    state: if after {
                        ButtonState::Pressed
                    } else {
                        ButtonState::Released
                    },
                },
            )?;
        }
    }
    if !sample.wheel.is_empty() {
        send_input(
            commands,
            session_id,
            generation,
            InputEvent::Wheel {
                delta_x: f32::from(sample.wheel.horizontal),
                delta_y: f32::from(sample.wheel.vertical),
            },
        )?;
    }
    *previous_buttons = sample.buttons;
    Ok(())
}

fn apply_mailbox_updates(
    mailbox: &Arc<Mutex<FrameMailbox>>,
    framebuffer: &mut Framebuffer,
    generation: &mut u64,
) -> Result<()> {
    loop {
        let update = mailbox.lock().unwrap().pop();
        let Some(update) = update else {
            return Ok(());
        };
        match update {
            SurfaceUpdate::Reset {
                generation: next,
                size,
                format,
                ..
            } => {
                if format != PixelFormat::Bgrx8UnormSrgb {
                    bail!("legacy HPSS viewer only accepts BGRX surfaces");
                }
                *framebuffer = Framebuffer::new(size.width as usize, size.height as usize)?;
                *generation = next;
            }
            SurfaceUpdate::Damage {
                generation: update_generation,
                patches,
                ..
            } if update_generation == *generation => {
                for patch in patches {
                    let rect = patch.rect;
                    let stride = usize::try_from(patch.stride_bytes)
                        .context("BGRX patch stride cannot fit usize")?;
                    let bytes = patch.pixels.as_bytes();
                    for row in 0..rect.height as usize {
                        let source = &bytes[row * stride..row * stride + rect.width as usize * 4];
                        let destination_start =
                            (rect.y as usize + row) * framebuffer.width + rect.x as usize;
                        let destination = &mut framebuffer.pixels_mut()
                            [destination_start..destination_start + rect.width as usize];
                        for (pixel, bgrx) in destination.iter_mut().zip(source.chunks_exact(4)) {
                            *pixel = (u32::from(bgrx[2]) << 16)
                                | (u32::from(bgrx[1]) << 8)
                                | u32::from(bgrx[0]);
                        }
                    }
                }
            }
            SurfaceUpdate::Damage { .. } | SurfaceUpdate::FrameBoundary { .. } => {}
        }
    }
}

fn render_nearest(framebuffer: &Framebuffer, drawable: PixelSize, output: &mut Vec<u32>) {
    output.clear();
    output.resize(drawable.width as usize * drawable.height as usize, 0);
    let remote = PixelSize {
        width: framebuffer.width as u32,
        height: framebuffer.height as u32,
    };
    let viewport = ContentViewport::fit(remote, drawable);
    for y in 0..viewport.content.height as usize {
        let source_y = y * framebuffer.height / viewport.content.height as usize;
        let destination_y = viewport.content.y as usize + y;
        for x in 0..viewport.content.width as usize {
            let source_x = x * framebuffer.width / viewport.content.width as usize;
            let destination_x = viewport.content.x as usize + x;
            output[destination_y * drawable.width as usize + destination_x] =
                framebuffer.pixels()[source_y * framebuffer.width + source_x];
        }
    }
}

fn drain_media(receiver: &Receiver<MediaFrame>, playback: &mut Option<AudioPlayback>) {
    loop {
        match receiver.try_recv() {
            Ok(MediaFrame::Pcm {
                sample_rate_hz,
                channels,
                samples,
            }) if sample_rate_hz == 48_000 && channels == 2 => {
                if playback.is_none() {
                    match AudioPlayback::open_default() {
                        Ok(output) => {
                            eprintln!("[audio] 使用输出设备: {}", output.device_description());
                            *playback = Some(output);
                        }
                        Err(error) => {
                            eprintln!("[audio] 默认输出不可用，继续无声会话: {error:#}");
                            continue;
                        }
                    }
                }
                if let Some(output) = playback {
                    if let Err(error) = output.enqueue_interleaved_stereo(&samples) {
                        eprintln!("[audio] 播放队列拒绝 PCM，继续无声会话: {error:#}");
                        *playback = None;
                    }
                }
            }
            Ok(MediaFrame::Pcm { .. } | MediaFrame::EncodedVideo { .. }) => {}
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
        }
    }
}

/// Run the legacy minifb lab UI while the leaf Apple crate owns the complete
/// authenticated HPSS/MVS/media runtime.
pub fn run_viewer(
    connection: RfbConn,
    display_name: &str,
    init_w: u16,
    init_h: u16,
    scale: f32,
    dynamic_resolution_enabled: bool,
    audio_flow: AudioMediaFlow,
) -> Result<()> {
    if !scale.is_finite() || !(0.0 < scale && scale <= 1.0) {
        bail!("显示缩放必须是有限数值，且满足 0 < scale <= 1");
    }
    let session_id = SessionId::allocate();
    let initial_size =
        PixelSize::new(init_w.into(), init_h.into()).context("HPSS 初始显示尺寸无效")?;
    let mailbox = Arc::new(Mutex::new(FrameMailbox::new(
        FRAME_MAILBOX_ENTRIES,
        FRAME_MAILBOX_PIXEL_BYTES,
    )));
    let (commands_tx, commands_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::channel();
    let (media_tx, media_rx) = mpsc::sync_channel(MEDIA_QUEUE_ENTRIES);
    let runtime = ProtocolRuntime::new(
        session_id,
        commands_rx,
        Box::new(EventChannel(events_tx)),
        Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
        Some(Box::new(MediaChannel(media_tx))),
        Box::new(NoopWake),
    );
    let display_name = display_name.to_owned();
    let worker = std::thread::spawn(move || {
        frd_protocol_apple::run_established_hpss_session(
            connection,
            display_name,
            initial_size,
            runtime,
            session_id,
            dynamic_resolution_enabled,
            audio_flow,
        )
    });

    let ui_result = (|| -> Result<()> {
        let window_width = ((f32::from(init_w) * scale).ceil() as usize).max(1);
        let window_height = ((f32::from(init_h) * scale).ceil() as usize).max(1);
        let mut window = Window::new(
            &format!("FreeRemoteDesk HPSS — [{init_w}x{init_h}  Ctrl+Q 退出]"),
            window_width,
            window_height,
            WindowOptions {
                resize: true,
                ..Default::default()
            },
        )?;
        window.set_target_fps(60);

        let mut framebuffer = Framebuffer::new(init_w.into(), init_h.into())?;
        let mut generation = 0u64;
        let mut scaled = Vec::new();
        let mut pointer_input = PointerInputState::default();
        let mut previous_buttons = PointerButtons::default();
        let mut last_window_size = window.get_size();
        let mut playback = None;

        while window.is_open() && !worker.is_finished() {
            apply_mailbox_updates(&mailbox, &mut framebuffer, &mut generation)?;
            while let Ok(event) = events_rx.try_recv() {
                if let SessionEvent::Error(error) = event {
                    eprintln!("[hpss-view] adapter error: {}", error.code());
                }
            }
            drain_media(&media_rx, &mut playback);

            let window_size = window.get_size();
            let drawable = PixelSize::new(window_size.0.max(1) as u32, window_size.1.max(1) as u32)
                .expect("minifb drawable is clamped non-zero");
            render_nearest(&framebuffer, drawable, &mut scaled);
            window.update_with_buffer(
                &scaled,
                drawable.width as usize,
                drawable.height as usize,
            )?;

            if generation != 0 && dynamic_resolution_enabled && window_size != last_window_size {
                last_window_size = window_size;
                let remote = PixelSize::new(framebuffer.width as u32, framebuffer.height as u32)
                    .expect("legacy framebuffer dimensions are non-zero");
                let viewport = ContentViewport::fit(remote, drawable);
                let viewport = PhysicalViewport::new(drawable, viewport.content, remote)
                    .expect("ContentViewport produces valid physical geometry");
                commands_tx
                    .send(SessionCommand::ViewportChanged {
                        session_id,
                        generation,
                        viewport,
                    })
                    .context("Apple adapter command channel closed")?;
            }

            let quit_hotkey = {
                let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
                ctrl && window.is_key_pressed(Key::Q, minifb::KeyRepeat::No)
            };
            if quit_hotkey {
                break;
            }

            if generation != 0 {
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
                    let remote =
                        PixelSize::new(framebuffer.width as u32, framebuffer.height as u32)
                            .expect("legacy framebuffer dimensions are non-zero");
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
                    send_pointer_sample(
                        &commands_tx,
                        session_id,
                        generation,
                        sample,
                        &mut previous_buttons,
                    )?;
                }
            }
        }

        if generation != 0 && previous_buttons.any_pressed() {
            let _ = send_input(&commands_tx, session_id, generation, InputEvent::ReleaseAll);
        }
        Ok(())
    })();
    let _ = commands_tx.send(SessionCommand::Disconnect);
    drop(commands_tx);
    let worker_result = match worker.join() {
        Ok(ProtocolExit::Closed) => Ok(()),
        Ok(ProtocolExit::Failed(error)) => {
            Err(anyhow::anyhow!("Apple adapter 已失败: {}", error.code()))
        }
        Err(_) => Err(anyhow::anyhow!("Apple adapter worker 异常退出")),
    };
    match (ui_result, worker_result) {
        (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
