use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{bounded, Sender, TrySendError};

use crate::app::connection::ValidatedConnection;
use crate::core::{FrameRect, RemotePixelFormat, RemoteSurfaceState, RenderUpdate};
use crate::protocols::ProtocolAdapter;
use crate::session::backpressure::{QueuePushOutcome, SessionEventMailbox};

const DEFAULT_COMMAND_CAPACITY: usize = 256;
const DEFAULT_EVENT_CAPACITY: usize = 256;
const DEFAULT_RENDER_BYTE_BUDGET: usize = 64 * 1024 * 1024 * 4;

#[derive(Debug)]
pub struct ProtocolContext {
    connection: ValidatedConnection,
}

impl ProtocolContext {
    pub fn new(connection: ValidatedConnection) -> Self {
        Self { connection }
    }

    pub fn connection(&self) -> &ValidatedConnection {
        &self.connection
    }

    pub fn into_connection(self) -> ValidatedConnection {
        self.connection
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionCommand {
    Pointer {
        x: u32,
        y: u32,
        buttons: u8,
    },
    Key {
        physical_code: Option<u32>,
        keysym: Option<u32>,
        pressed: bool,
    },
    Resize {
        width: u32,
        height: u32,
    },
    ClipboardText(String),
    RequestFullFrame,
    Disconnect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Connecting,
    SurfaceReset {
        generation: u64,
        width: u32,
        height: u32,
        format: RemotePixelFormat,
    },
    Render(RenderUpdate),
    ClipboardText(String),
    Bell,
    Connected {
        generation: u64,
    },
    Disconnecting,
    Disconnected,
    Failed {
        code: &'static str,
    },
}

pub trait UiWakeHandle: Send + Sync + 'static {
    fn wake(&self) -> Result<(), SessionError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionMailboxLimits {
    command_capacity: usize,
    event_capacity: usize,
    render_byte_budget: usize,
}

impl SessionMailboxLimits {
    pub fn new(
        command_capacity: usize,
        event_capacity: usize,
        render_event_capacity: usize,
        render_byte_budget: usize,
    ) -> Result<Self, SessionError> {
        if command_capacity == 0 {
            return Err(SessionError::new("session_command_capacity_invalid"));
        }
        if event_capacity == 0 || render_event_capacity == 0 {
            return Err(SessionError::new("session_event_capacity_invalid"));
        }
        if event_capacity != render_event_capacity {
            return Err(SessionError::new("session_event_capacity_mismatch"));
        }
        SessionEventMailbox::with_limits(event_capacity, render_byte_budget)
            .map_err(|error| SessionError::new(error.code()))?;
        Ok(Self {
            command_capacity,
            event_capacity,
            render_byte_budget,
        })
    }

    fn production_defaults() -> Self {
        Self::new(
            DEFAULT_COMMAND_CAPACITY,
            DEFAULT_EVENT_CAPACITY,
            DEFAULT_EVENT_CAPACITY,
            DEFAULT_RENDER_BYTE_BUDGET,
        )
        .expect("production mailbox limits are valid")
    }
}

#[derive(Clone)]
pub struct SessionEventSink {
    mailbox: Arc<Mutex<SessionEventMailbox>>,
    wake: Arc<dyn UiWakeHandle>,
}

impl SessionEventSink {
    pub fn emit(&self, event: SessionEvent) -> Result<(), SessionError> {
        let outcome = self
            .mailbox
            .lock()
            .map_err(|_| SessionError::new("session_mailbox_poisoned"))?
            .push(event)
            .map_err(|error| SessionError::new(error.code()))?;
        if outcome == QueuePushOutcome::Queued {
            self.wake.wake()?;
        }
        Ok(())
    }
}

struct WorkerCompletion(Arc<AtomicBool>);

impl Drop for WorkerCompletion {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub struct SessionEngine {
    commands: Sender<SessionCommand>,
    mailbox: Arc<Mutex<SessionEventMailbox>>,
    worker_finished: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SessionEngine {
    pub fn spawn(
        adapter: Box<dyn ProtocolAdapter>,
        context: ProtocolContext,
        wake: Arc<dyn UiWakeHandle>,
    ) -> Result<Self, SessionError> {
        Self::spawn_with_mailbox_limits(
            adapter,
            context,
            wake,
            SessionMailboxLimits::production_defaults(),
        )
    }

    pub fn spawn_with_mailbox_limits(
        adapter: Box<dyn ProtocolAdapter>,
        context: ProtocolContext,
        wake: Arc<dyn UiWakeHandle>,
        limits: SessionMailboxLimits,
    ) -> Result<Self, SessionError> {
        let (command_sender, command_receiver) = bounded(limits.command_capacity);
        let mailbox = Arc::new(Mutex::new(
            SessionEventMailbox::with_limits(limits.event_capacity, limits.render_byte_budget)
                .map_err(|error| SessionError::new(error.code()))?,
        ));
        let sink = SessionEventSink {
            mailbox: Arc::clone(&mailbox),
            wake,
        };
        let failure_sink = sink.clone();
        let worker_finished = Arc::new(AtomicBool::new(false));
        let completion = Arc::clone(&worker_finished);
        let worker = thread::Builder::new()
            .name("freeremote-protocol".to_owned())
            .spawn(move || {
                let _completion = WorkerCompletion(completion);
                if let Err(error) = adapter.run(context, command_receiver, sink) {
                    let _ = failure_sink.emit(SessionEvent::Failed { code: error.code() });
                }
            })
            .map_err(|_| SessionError::new("protocol_thread_spawn_failed"))?;

        Ok(Self {
            commands: command_sender,
            mailbox,
            worker_finished,
            worker: Some(worker),
        })
    }

    pub fn send(&self, command: SessionCommand) -> Result<(), SessionError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => SessionError::new("session_command_channel_full"),
                TrySendError::Disconnected(_) => {
                    SessionError::new("session_command_channel_closed")
                }
            })
    }

    pub fn try_next_event(&self) -> Result<Option<SessionEvent>, SessionError> {
        let event = self
            .mailbox
            .lock()
            .map_err(|_| SessionError::new("session_mailbox_poisoned"))?
            .pop_front();
        if event.is_some() {
            return Ok(event);
        }
        if self.worker_finished.load(Ordering::Acquire) {
            Err(SessionError::new("session_event_channel_closed"))
        } else {
            Ok(None)
        }
    }

    pub fn join(mut self) -> Result<(), SessionError> {
        drop(self.commands);
        let worker = self.worker.take().expect("worker is present until join");
        worker
            .join()
            .map_err(|_| SessionError::new("protocol_thread_panicked"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Idle,
    Connecting,
    SurfaceReady,
    Connected,
    Disconnecting,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSnapshot {
    phase: SessionPhase,
    surface: Option<RemoteSurfaceState>,
}

impl SessionSnapshot {
    pub const fn phase(self) -> SessionPhase {
        self.phase
    }

    pub const fn surface(self) -> Option<RemoteSurfaceState> {
        self.surface
    }
}

#[derive(Debug)]
pub struct SessionModel {
    phase: SessionPhase,
    surface: Option<RemoteSurfaceState>,
}

impl Default for SessionModel {
    fn default() -> Self {
        Self {
            phase: SessionPhase::Idle,
            surface: None,
        }
    }
}

impl SessionModel {
    pub fn apply(&mut self, event: SessionEvent) -> Result<(), SessionError> {
        match event {
            SessionEvent::Connecting if self.phase == SessionPhase::Idle => {
                self.phase = SessionPhase::Connecting;
                self.surface = None;
            }
            SessionEvent::SurfaceReset {
                generation,
                width,
                height,
                ..
            } if matches!(
                self.phase,
                SessionPhase::Connecting | SessionPhase::SurfaceReady | SessionPhase::Connected
            ) =>
            {
                let surface = RemoteSurfaceState::new(generation, width, height)
                    .map_err(|_| SessionError::new("surface_reset_invalid"))?;
                if self
                    .surface
                    .is_some_and(|current| generation < current.generation())
                {
                    return Err(SessionError::new("surface_reset_stale"));
                }
                self.surface = Some(surface);
                self.phase = SessionPhase::SurfaceReady;
            }
            SessionEvent::Connected { generation } => {
                let surface = self
                    .surface
                    .ok_or_else(|| SessionError::new("connected_without_surface"))?;
                if surface.generation() != generation {
                    return Err(SessionError::new("connected_generation_mismatch"));
                }
                if self.phase != SessionPhase::SurfaceReady {
                    return Err(SessionError::new("session_transition_invalid"));
                }
                self.phase = SessionPhase::Connected;
            }
            SessionEvent::Render(update) => self.validate_render_update(&update)?,
            SessionEvent::ClipboardText(_) | SessionEvent::Bell => {}
            SessionEvent::Disconnecting
                if matches!(
                    self.phase,
                    SessionPhase::Connecting | SessionPhase::SurfaceReady | SessionPhase::Connected
                ) =>
            {
                self.phase = SessionPhase::Disconnecting;
            }
            SessionEvent::Disconnected if self.phase == SessionPhase::Disconnecting => {
                self.phase = SessionPhase::Idle;
                self.surface = None;
            }
            SessionEvent::Failed { .. } => {
                self.phase = SessionPhase::Failed;
                self.surface = None;
            }
            _ => return Err(SessionError::new("session_transition_invalid")),
        }
        Ok(())
    }

    pub const fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            phase: self.phase,
            surface: self.surface,
        }
    }

    fn validate_render_update(&self, update: &RenderUpdate) -> Result<(), SessionError> {
        let surface = self
            .surface
            .ok_or_else(|| SessionError::new("render_without_surface"))?;
        if update.generation() != surface.generation() {
            return Err(SessionError::new("render_generation_mismatch"));
        }
        if let RenderUpdate::DirtyRect { rect, .. } = update {
            validate_rect(surface, *rect)?;
        }
        Ok(())
    }
}

fn validate_rect(surface: RemoteSurfaceState, rect: FrameRect) -> Result<(), SessionError> {
    if surface.contains(rect) {
        Ok(())
    } else {
        Err(SessionError::new("render_rect_out_of_bounds"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionError {
    code: &'static str,
}

impl SessionError {
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "远程会话状态无效 ({})", self.code)
    }
}

impl Error for SessionError {}
