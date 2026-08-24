use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError, TrySendError};

use crate::app::connection::ValidatedConnection;
use crate::core::{FrameRect, RemotePixelFormat, RemoteSurfaceState, RenderUpdate};
use crate::protocols::ProtocolAdapter;
use crate::session::backpressure::{QueuePushOutcome, RenderUpdateQueue};

const DEFAULT_COMMAND_CAPACITY: usize = 256;
const DEFAULT_CONTROL_EVENT_CAPACITY: usize = 256;
const DEFAULT_RENDER_EVENT_CAPACITY: usize = 256;
const DEFAULT_RENDER_BYTE_BUDGET: usize = 64 * 1024 * 1024 * 4;

#[derive(Debug)]
enum MailboxEvent {
    Control(SessionEvent),
    RenderReady,
}

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
    control_event_capacity: usize,
    render_event_capacity: usize,
    render_byte_budget: usize,
}

impl SessionMailboxLimits {
    pub fn new(
        command_capacity: usize,
        control_event_capacity: usize,
        render_event_capacity: usize,
        render_byte_budget: usize,
    ) -> Result<Self, SessionError> {
        if command_capacity == 0 {
            return Err(SessionError::new("session_command_capacity_invalid"));
        }
        if control_event_capacity == 0 {
            return Err(SessionError::new("session_control_event_capacity_invalid"));
        }
        RenderUpdateQueue::with_limits(render_event_capacity, render_byte_budget)
            .map_err(|error| SessionError::new(error.code()))?;
        Ok(Self {
            command_capacity,
            control_event_capacity,
            render_event_capacity,
            render_byte_budget,
        })
    }

    fn production_defaults() -> Self {
        Self::new(
            DEFAULT_COMMAND_CAPACITY,
            DEFAULT_CONTROL_EVENT_CAPACITY,
            DEFAULT_RENDER_EVENT_CAPACITY,
            DEFAULT_RENDER_BYTE_BUDGET,
        )
        .expect("production mailbox limits are valid")
    }
}

#[derive(Clone)]
pub struct SessionEventSink {
    sender: Sender<MailboxEvent>,
    render_queue: Arc<Mutex<RenderUpdateQueue>>,
    wake: Arc<dyn UiWakeHandle>,
}

impl SessionEventSink {
    pub fn emit(&self, event: SessionEvent) -> Result<(), SessionError> {
        match event {
            SessionEvent::Render(update) => self.emit_render(update),
            control_event => self.emit_control(control_event),
        }
    }

    fn emit_control(&self, event: SessionEvent) -> Result<(), SessionError> {
        self.sender
            .try_send(MailboxEvent::Control(event))
            .map_err(|error| match error {
                TrySendError::Full(_) => SessionError::new("session_event_channel_full"),
                TrySendError::Disconnected(_) => SessionError::new("session_event_channel_closed"),
            })?;
        self.wake.wake()
    }

    fn emit_render(&self, update: RenderUpdate) -> Result<(), SessionError> {
        let mut queue = self.render_queue.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => SessionError::new("render_queue_busy"),
            TryLockError::Poisoned(_) => SessionError::new("render_queue_poisoned"),
        })?;
        let outcome = queue
            .push(update)
            .map_err(|error| SessionError::new(error.code()))?;
        if outcome == QueuePushOutcome::Queued {
            self.sender
                .try_send(MailboxEvent::RenderReady)
                .map_err(|error| {
                    let _ = queue.pop_back();
                    match error {
                        TrySendError::Full(_) => SessionError::new("session_event_channel_full"),
                        TrySendError::Disconnected(_) => {
                            SessionError::new("session_event_channel_closed")
                        }
                    }
                })?;
        }
        drop(queue);
        if outcome == QueuePushOutcome::Queued {
            self.wake.wake()?;
        }
        Ok(())
    }
}

pub struct SessionEngine {
    commands: Sender<SessionCommand>,
    events: Receiver<MailboxEvent>,
    render_queue: Arc<Mutex<RenderUpdateQueue>>,
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
        let (event_sender, event_receiver) = bounded(limits.control_event_capacity);
        let render_queue = Arc::new(Mutex::new(
            RenderUpdateQueue::with_limits(limits.render_event_capacity, limits.render_byte_budget)
                .map_err(|error| SessionError::new(error.code()))?,
        ));
        let sink = SessionEventSink {
            sender: event_sender,
            render_queue: Arc::clone(&render_queue),
            wake,
        };
        let failure_sink = sink.clone();
        let worker = thread::Builder::new()
            .name("freeremote-protocol".to_owned())
            .spawn(move || {
                if let Err(error) = adapter.run(context, command_receiver, sink) {
                    let _ = failure_sink.emit(SessionEvent::Failed { code: error.code() });
                }
            })
            .map_err(|_| SessionError::new("protocol_thread_spawn_failed"))?;

        Ok(Self {
            commands: command_sender,
            events: event_receiver,
            render_queue,
            worker: Some(worker),
        })
    }

    pub fn send(&self, command: SessionCommand) -> Result<(), SessionError> {
        self.commands
            .send(command)
            .map_err(|_| SessionError::new("session_command_channel_closed"))
    }

    pub fn try_next_event(&self) -> Result<Option<SessionEvent>, SessionError> {
        match self.events.try_recv() {
            Ok(MailboxEvent::Control(event)) => Ok(Some(event)),
            Ok(MailboxEvent::RenderReady) => self.take_ready_render(),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(SessionError::new("session_event_channel_closed"))
            }
        }
    }

    fn take_ready_render(&self) -> Result<Option<SessionEvent>, SessionError> {
        let mut render_queue = self
            .render_queue
            .lock()
            .map_err(|_| SessionError::new("render_queue_poisoned"))?;
        let update = render_queue
            .pop_front()
            .ok_or_else(|| SessionError::new("render_queue_signal_missing"))?;
        Ok(Some(SessionEvent::Render(update)))
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
