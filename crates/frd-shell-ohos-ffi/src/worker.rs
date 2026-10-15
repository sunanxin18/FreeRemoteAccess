use crate::*;
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

// Backend 无 Send/Sync 要求：在唯一 worker 上创建、使用、销毁，不进入 registry。
pub(crate) trait Backend {
    fn attach(&mut self, window: usize, width: u32, height: u32) -> Result<(), i32>;
    fn resize(&mut self, width: u32, height: u32) -> Result<(), i32>;
    fn pause(&mut self);
    fn render_poc(&mut self) -> Result<bool, i32>;
    fn detach(&mut self);
    fn window_surface_id(&mut self) -> Result<u64, i32> {
        Err(UNSUPPORTED_PLATFORM)
    }
    fn reset_remote(&mut self) {}
    fn apply(&mut self, _: Vec<frd_frame::FrameTransaction>) -> Result<(), i32> {
        Err(UNSUPPORTED_PLATFORM)
    }
    fn render_remote(
        &mut self,
        _: frd_core::TransformedViewport,
        _: bool,
    ) -> Result<Option<frd_protocol_api::PresentationEvent>, i32> {
        Err(UNSUPPORTED_PLATFORM)
    }
}
#[cfg(not(target_env = "ohos"))]
pub(crate) struct UnavailableBackend;
#[cfg(not(target_env = "ohos"))]
impl Backend for UnavailableBackend {
    fn attach(&mut self, _: usize, _: u32, _: u32) -> Result<(), i32> {
        Err(UNSUPPORTED_PLATFORM)
    }
    fn resize(&mut self, _: u32, _: u32) -> Result<(), i32> {
        Err(UNSUPPORTED_PLATFORM)
    }
    fn pause(&mut self) {}
    fn render_poc(&mut self) -> Result<bool, i32> {
        Err(UNSUPPORTED_PLATFORM)
    }
    fn detach(&mut self) {}
}
pub(crate) enum Command {
    Attach {
        window: usize,
        width: u32,
        height: u32,
    },
    Resize {
        generation: u64,
        width: u32,
        height: u32,
    },
    Detach(u64),
    Foreground(bool),
    Tick(u64),
    WindowSurfaceId(u64),
    Shutdown,
}
struct Message {
    command: Command,
    reply: Option<mpsc::Sender<Result<u64, i32>>>,
}
struct Gate {
    sender: Option<SyncSender<Message>>,
    join: Option<JoinHandle<()>>,
}
pub(crate) struct Engine {
    gate: Mutex<Gate>,
    status: Arc<Mutex<PocStatus>>,
    pub(crate) link: Arc<crate::presentation::Link>,
}
impl Engine {
    pub(crate) fn spawn(
        factory: impl FnOnce() -> Box<dyn Backend> + Send + 'static,
    ) -> Result<Self, i32> {
        let (sender, receiver) = mpsc::sync_channel::<Message>(4);
        let status = Arc::new(Mutex::new(PocStatus {
            foreground: 1,
            ..Default::default()
        }));
        let published = status.clone();
        let link = Arc::new(crate::presentation::Link::default());
        let worker_link = link.clone();
        let join = thread::Builder::new()
            .name("frd-ohos-render".into())
            .spawn(move || {
                // 受控范围覆盖 factory、所有命令和最终 native/GPU 清理，先于 catch。
                let _panic_scope = crate::panic_scope::Scope::enter();
                // 含 GPU/native 所有权的状态在此线程创建并销毁；panic 也不会移到 UI。
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let mut state = Some(State {
                        backend: factory(),
                        status: PocStatus {
                            foreground: 1,
                            ..Default::default()
                        },
                    });
                    let mut consumer = crate::presentation::Consumer::default();
                    loop {
                        let message =
                            match receiver.recv_timeout(std::time::Duration::from_millis(15)) {
                                Ok(m) => Some(m),
                                Err(mpsc::RecvTimeoutError::Timeout) => None,
                                Err(_) => break,
                            };
                        if let Some(message) = message {
                            let readonly = matches!(message.command, Command::WindowSurfaceId(_));
                            let lifecycle = !matches!(
                                message.command,
                                Command::Tick(_) | Command::WindowSurfaceId(_)
                            );
                            let shutdown = matches!(message.command, Command::Shutdown);
                            if lifecycle {
                                if let Ok(mut shared) = worker_link.0.lock() {
                                    shared.surface.lifecycle =
                                        shared.surface.lifecycle.saturating_add(1);
                                    shared.surface.attached = false;
                                }
                            }
                            // reply 留在 unwind 作用域外；异常路径也先清理 owned 窗口再 ack。
                            let executed = catch_unwind(AssertUnwindSafe(|| {
                                let bound = worker_link.0.lock().map_err(|_| PANIC)?.bound;
                                if bound && matches!(message.command, Command::Tick(_)) {
                                    return Ok(0);
                                }
                                state.as_mut().unwrap().execute(message.command)
                            }));
                            let panicked = executed.is_err();
                            let outcome = executed.unwrap_or(Err(PANIC));
                            if panicked {
                                let _ = catch_unwind(AssertUnwindSafe(|| drop(state.take())));
                                if let Ok(mut shared) = published.lock() {
                                    shared.last_error = PANIC;
                                    shared.attached = 0;
                                }
                            } else if !readonly {
                                if let Some(state) = &mut state {
                                    if let Err(code) = outcome {
                                        state.status.last_error = code;
                                    }
                                    if let Ok(mut shared) = published.lock() {
                                        *shared = state.status;
                                    }
                                }
                            }
                            if lifecycle {
                                if let Some(state) = &state {
                                    worker_link.surface(state.status);
                                }
                            }
                            if panicked {
                                worker_link.fail(PANIC);
                            }
                            if let Err(
                                code @ (GPU_FAILED
                                | PANIC
                                | WINDOW_REFERENCE_FAILED
                                | WORKER_STOPPED),
                            ) = outcome
                            {
                                if !readonly {
                                    worker_link.fail(code);
                                }
                            }
                            if let Some(reply) = message.reply {
                                let _ = reply.send(outcome);
                            }
                            if shutdown || panicked {
                                break;
                            }
                        }
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            consumer.poll(&worker_link, state.as_mut().unwrap().backend.as_mut())
                        }));
                        match result {
                            Ok(Ok(true)) => {
                                if let Some(state) = &mut state {
                                    state.status.presented_frames += 1;
                                    if let Ok(mut shared) = published.lock() {
                                        *shared = state.status;
                                    }
                                }
                            }
                            Ok(Ok(false)) => {}
                            Ok(Err(code)) => {
                                worker_link.fail(code);
                                if let Ok(mut shared) = published.lock() {
                                    shared.last_error = code;
                                }
                            }
                            Err(_) => {
                                worker_link.fail(PANIC);
                                if let Ok(mut shared) = published.lock() {
                                    shared.last_error = PANIC;
                                    shared.attached = 0;
                                }
                                break;
                            }
                        }
                    }
                }));
                if result.is_err() {
                    worker_link.fail(PANIC);
                    if let Ok(mut shared) = published.lock() {
                        shared.last_error = PANIC;
                        shared.attached = 0;
                    }
                }
            })
            .map_err(|_| WORKER_STOPPED)?;
        Ok(Self {
            gate: Mutex::new(Gate {
                sender: Some(sender),
                join: Some(join),
            }),
            status,
            link,
        })
    }
    pub(crate) fn call(&self, command: Command) -> Result<u64, i32> {
        let gate = self.gate.lock().map_err(|_| PANIC)?;
        let sender = gate.sender.as_ref().ok_or(INVALID_HANDLE)?;
        let (reply, receiver) = mpsc::channel();
        sender
            .send(Message {
                command,
                reply: Some(reply),
            })
            .map_err(|_| WORKER_STOPPED)?;
        // 无超时：attach 的借用地址在 ack 或 worker 全部清理完成前不能失效。
        receiver.recv().map_err(|_| WORKER_STOPPED)?
    }
    pub(crate) fn tick(&self, generation: u64) -> Result<(), i32> {
        let gate = match self.gate.try_lock() {
            Ok(gate) => gate,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(_) => return Err(PANIC),
        };
        let sender = gate.sender.as_ref().ok_or(INVALID_HANDLE)?;
        match sender.try_send(Message {
            command: Command::Tick(generation),
            reply: None,
        }) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(WORKER_STOPPED),
        }
    }
    pub(crate) fn status(&self) -> Result<PocStatus, i32> {
        self.status.lock().map(|status| *status).map_err(|_| PANIC)
    }
    pub(crate) fn shutdown(&self) -> Result<(), i32> {
        let mut gate = self.gate.lock().map_err(|_| PANIC)?;
        let sender = gate.sender.take().ok_or(INVALID_HANDLE)?;
        let (reply, receiver) = mpsc::channel();
        let result = sender
            .send(Message {
                command: Command::Shutdown,
                reply: Some(reply),
            })
            .map_err(|_| WORKER_STOPPED)
            .and_then(|()| receiver.recv().map_err(|_| WORKER_STOPPED)?.map(|_| ()));
        drop(sender);
        if let Some(join) = gate.join.take() {
            join.join().map_err(|_| PANIC)?;
        }
        result
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
struct State {
    backend: Box<dyn Backend>,
    status: PocStatus,
}
impl State {
    fn current(&self, generation: u64) -> Result<(), i32> {
        if generation == 0 || generation != self.status.surface_generation {
            Err(STALE_GENERATION)
        } else {
            Ok(())
        }
    }
    fn detach(&mut self) {
        if self.status.attached != 0 {
            self.status.attached = 0;
            self.backend.pause();
            self.backend.detach();
            self.status.detaches += 1;
        }
    }
    fn execute(&mut self, command: Command) -> Result<u64, i32> {
        match command {
            Command::Attach {
                window,
                width,
                height,
            } => {
                if window == 0 || width == 0 || height == 0 {
                    return Err(INVALID_ARGUMENT);
                }
                let generation = self
                    .status
                    .surface_generation
                    .checked_add(1)
                    .ok_or(EXHAUSTED)?;
                self.detach();
                // 即使 attach 失败也消耗 generation，迟到回调不得成为后来窗口的有效回调。
                self.status.surface_generation = generation;
                self.backend.attach(window, width, height)?;
                self.status.attached = 1;
                self.status.width = width;
                self.status.height = height;
                self.status.attaches += 1;
                self.status.last_error = OK;
                if self.status.foreground == 0 {
                    self.backend.pause();
                }
                Ok(generation)
            }
            Command::Resize {
                generation,
                width,
                height,
            } => {
                self.current(generation)?;
                if self.status.attached == 0 {
                    return Err(NOT_ATTACHED);
                }
                // 0 extent 是系统短暂不可见，暂停但保留同 generation 及窗口引用。
                self.backend.resize(width, height)?;
                self.status.width = width;
                self.status.height = height;
                self.status.resizes += 1;
                if self.status.foreground == 0 {
                    self.backend.pause();
                }
                Ok(generation)
            }
            Command::Detach(generation) => {
                self.current(generation)?;
                self.detach();
                Ok(generation)
            }
            Command::Foreground(active) => {
                self.status.foreground = u32::from(active);
                if !active {
                    self.backend.pause();
                } else if self.status.attached != 0 {
                    self.backend.resize(self.status.width, self.status.height)?;
                }
                Ok(0)
            }
            Command::Tick(generation) => {
                self.current(generation)?;
                if self.status.attached != 0
                    && self.status.foreground != 0
                    && self.status.width > 0
                    && self.status.height > 0
                    && self.backend.render_poc()?
                {
                    self.status.presented_frames += 1;
                }
                Ok(generation)
            }
            Command::WindowSurfaceId(generation) => {
                self.current(generation)?;
                if self.status.attached == 0 {
                    return Err(NOT_ATTACHED);
                }
                let id = self.backend.window_surface_id()?;
                if id == 0 {
                    return Err(GPU_FAILED);
                }
                Ok(id)
            }
            Command::Shutdown => {
                self.detach();
                Ok(0)
            }
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.detach();
    }
}
