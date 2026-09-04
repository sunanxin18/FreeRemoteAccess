//! Apple 连接、接收缓冲与单一加密 writer 的所有权边界。

use std::cell::Cell;
use std::error;
use std::fmt;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::session::{
    take_wire_ciphertext_frame, InboundSessionCrypto, OutboundSessionCrypto, SessionCrypto,
};

const PRODUCTION_WRITER_IO_TIMEOUT: Duration = Duration::from_secs(2);
const HP_INPUT_DIAGNOSTICS_ENV: &str = "FRD_APPLE_HP_INPUT_DIAGNOSTICS";
const HP_INPUT_DIAGNOSTICS_QUEUE_LIMIT: usize = 32;
const HP_INPUT_DIAGNOSTICS_REPORT_POLL: Duration = Duration::from_millis(25);
const NO_WRITER_COMPLETION: u64 = u64::MAX;

#[derive(Clone)]
pub(crate) struct HighPerformanceInputDiagnostics {
    inner: Option<Arc<HighPerformanceInputDiagnosticState>>,
}

struct HighPerformanceInputDiagnosticState {
    started_at: Instant,
    runtime_consumed: AtomicU64,
    writer_completed: AtomicU64,
    last_writer_completed_ms: AtomicU64,
    terminal_emitted: AtomicBool,
    stage_lines: SyncSender<HighPerformanceInputLine>,
    terminal_lines: SyncSender<HighPerformanceInputLine>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HighPerformanceInputTerminal {
    LocalDisconnect,
    CleanClose,
    PeerClosed,
    AdapterError,
}

impl HighPerformanceInputTerminal {
    const fn code(self) -> &'static str {
        match self {
            Self::LocalDisconnect => "local_disconnect",
            Self::CleanClose => "clean_close",
            Self::PeerClosed => "peer_close",
            Self::AdapterError => "adapter_error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HighPerformanceInputLine {
    Stage {
        stage: &'static str,
        count: u64,
        elapsed_ms: u64,
    },
    Terminal {
        kind: &'static str,
        runtime_consumed: u64,
        writer_completed: u64,
        since_last_writer_ms: Option<u64>,
        elapsed_ms: u64,
    },
}

impl fmt::Display for HighPerformanceInputLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stage {
                stage,
                count,
                elapsed_ms,
            } => write!(
                formatter,
                "[apple-hp-input] stage={stage} count={count} elapsed_ms={elapsed_ms}"
            ),
            Self::Terminal {
                kind,
                runtime_consumed,
                writer_completed,
                since_last_writer_ms,
                elapsed_ms,
            } => write!(
                formatter,
                "[apple-hp-input] stage=terminal kind={kind} runtime_consumed={runtime_consumed} writer_completed={writer_completed} since_last_writer_ms={} elapsed_ms={elapsed_ms}",
                since_last_writer_ms
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_owned())
            ),
        }
    }
}

impl HighPerformanceInputDiagnostics {
    pub(crate) fn for_protocol(protocol_id: &frd_core::ProtocolId) -> Self {
        if protocol_id.as_str() != "apple-high-performance"
            || !std::env::var_os(HP_INPUT_DIAGNOSTICS_ENV).is_some_and(|value| value == "1")
        {
            return Self { inner: None };
        }
        let (stage_lines, stage_receiver) = mpsc::sync_channel(HP_INPUT_DIAGNOSTICS_QUEUE_LIMIT);
        let (terminal_lines, terminal_receiver) = mpsc::sync_channel(1);
        let reporter = thread::Builder::new()
            .name("frd-hp-input-protocol-diagnostic".to_owned())
            .spawn(move || report_hp_input_lines(stage_receiver, terminal_receiver));
        if reporter.is_err() {
            return Self { inner: None };
        }
        Self::with_senders(Instant::now(), stage_lines, terminal_lines)
    }

    fn with_senders(
        started_at: Instant,
        stage_lines: SyncSender<HighPerformanceInputLine>,
        terminal_lines: SyncSender<HighPerformanceInputLine>,
    ) -> Self {
        Self {
            inner: Some(Arc::new(HighPerformanceInputDiagnosticState {
                started_at,
                runtime_consumed: AtomicU64::new(0),
                writer_completed: AtomicU64::new(0),
                last_writer_completed_ms: AtomicU64::new(NO_WRITER_COMPLETION),
                terminal_emitted: AtomicBool::new(false),
                stage_lines,
                terminal_lines,
            })),
        }
    }

    #[cfg(test)]
    fn enabled_for_test(
        started_at: Instant,
        capacity: usize,
    ) -> (
        Self,
        Receiver<HighPerformanceInputLine>,
        Receiver<HighPerformanceInputLine>,
    ) {
        let (stage_lines, stage_receiver) = mpsc::sync_channel(capacity);
        let (terminal_lines, terminal_receiver) = mpsc::sync_channel(1);
        (
            Self::with_senders(started_at, stage_lines, terminal_lines),
            stage_receiver,
            terminal_receiver,
        )
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub(crate) fn observe_runtime_consumed(&self, now: Instant) {
        let Some(state) = &self.inner else {
            return;
        };
        let count = state
            .runtime_consumed
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        enqueue_hp_input_stage(state, "runtime_consumed", count, now);
    }

    fn observe_writer_completed(&self, now: Instant) {
        let Some(state) = &self.inner else {
            return;
        };
        state
            .last_writer_completed_ms
            .store(elapsed_millis(state.started_at, now), Ordering::Release);
        let count = state
            .writer_completed
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        enqueue_hp_input_stage(state, "writer_completed", count, now);
    }

    pub(crate) fn observe_terminal(
        &self,
        terminal: HighPerformanceInputTerminal,
        now: Instant,
    ) -> bool {
        let Some(state) = &self.inner else {
            return false;
        };
        if state
            .terminal_emitted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let elapsed_ms = elapsed_millis(state.started_at, now);
        let last_writer_completed_ms = state.last_writer_completed_ms.load(Ordering::Acquire);
        let line = HighPerformanceInputLine::Terminal {
            kind: terminal.code(),
            runtime_consumed: state.runtime_consumed.load(Ordering::Relaxed),
            writer_completed: state.writer_completed.load(Ordering::Relaxed),
            since_last_writer_ms: (last_writer_completed_ms != NO_WRITER_COMPLETION)
                .then(|| elapsed_ms.saturating_sub(last_writer_completed_ms)),
            elapsed_ms,
        };
        let _ = state.terminal_lines.try_send(line);
        true
    }
}

fn elapsed_millis(started_at: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(started_at)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX - 1)
}

fn enqueue_hp_input_stage(
    state: &HighPerformanceInputDiagnosticState,
    stage: &'static str,
    count: u64,
    now: Instant,
) {
    if count.is_power_of_two() {
        let _ = state.stage_lines.try_send(HighPerformanceInputLine::Stage {
            stage,
            count,
            elapsed_ms: elapsed_millis(state.started_at, now),
        });
    }
}

fn report_hp_input_lines(
    stage_lines: Receiver<HighPerformanceInputLine>,
    terminal_lines: Receiver<HighPerformanceInputLine>,
) {
    loop {
        match terminal_lines.try_recv() {
            Ok(terminal) => {
                for line in stage_lines.try_iter() {
                    eprintln!("{line}");
                }
                eprintln!("{terminal}");
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                for line in stage_lines.try_iter() {
                    eprintln!("{line}");
                }
                return;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match stage_lines.recv_timeout(HP_INPUT_DIAGNOSTICS_REPORT_POLL) {
            Ok(line) => eprintln!("{line}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Ok(terminal) = terminal_lines.recv() {
                    eprintln!("{terminal}");
                }
                return;
            }
        }
    }
}

#[derive(Debug)]
struct PeerClosed;

impl fmt::Display for PeerClosed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("连接已被服务器关闭（EOF）")
    }
}

impl error::Error for PeerClosed {}

#[derive(Debug)]
struct ColdDeadlineError;

impl fmt::Display for ColdDeadlineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("cold deadline")
    }
}

impl error::Error for ColdDeadlineError {}

fn cold_deadline_error() -> anyhow::Error {
    ColdDeadlineError.into()
}

pub fn is_cold_deadline_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<ColdDeadlineError>())
}

pub fn is_peer_closed(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<PeerClosed>())
}

pub fn is_timeout(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io_error| {
            matches!(
                io_error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            )
        })
}

enum WriterCommand {
    Message {
        plaintext: Vec<u8>,
        result: SyncSender<Result<()>>,
    },
    HighPerformanceKeyEvent {
        down: bool,
        keysym: u32,
        result: SyncSender<Result<()>>,
    },
    Shutdown,
}

struct WriterControl {
    commands: Sender<WriterCommand>,
    interrupt: TcpStream,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WriterIoTestEvent {
    Started { wire_bytes: usize },
    Finished { succeeded: bool },
    ResultDelivered,
    InputDiagnosticQueued,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionRuntimeTestEvent {
    LoopEntered,
    DisconnectDequeued,
    MediaClosed,
    ConnectionShutdown,
    WriterShutdown,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplicationFrameReadTestEvent {
    ReadPollSet(Duration),
    ApplicationFrameRead(Duration),
}

#[derive(Default)]
struct WriterHooks {
    #[cfg(test)]
    io_events: Option<Sender<WriterIoTestEvent>>,
    input_diagnostics: Option<HighPerformanceInputDiagnostics>,
}

impl WriterHooks {
    fn notify_write_started(&self, _wire_bytes: usize) {
        #[cfg(test)]
        if let Some(io_events) = &self.io_events {
            let _ = io_events.send(WriterIoTestEvent::Started {
                wire_bytes: _wire_bytes,
            });
        }
    }

    fn notify_write_finished(&self, _succeeded: bool) {
        #[cfg(test)]
        if let Some(io_events) = &self.io_events {
            let _ = io_events.send(WriterIoTestEvent::Finished {
                succeeded: _succeeded,
            });
        }
    }

    fn notify_result_delivered(&self) {
        #[cfg(test)]
        if let Some(io_events) = &self.io_events {
            let _ = io_events.send(WriterIoTestEvent::ResultDelivered);
        }
    }

    fn notify_input_diagnostic_queued(&self) {
        #[cfg(test)]
        if let Some(io_events) = &self.io_events {
            let _ = io_events.send(WriterIoTestEvent::InputDiagnosticQueued);
        }
    }
}

#[derive(Clone)]
pub struct AppleWriterHandle {
    control: Arc<WriterControl>,
}

impl AppleWriterHandle {
    pub fn send_private_message(&self, plaintext: &[u8]) -> Result<()> {
        Self::receive_message_result(self.enqueue_private_message(plaintext.to_vec())?)
    }

    fn enqueue_private_message(&self, plaintext: Vec<u8>) -> Result<Receiver<Result<()>>> {
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        self.control
            .commands
            .send(WriterCommand::Message {
                plaintext,
                result: result_tx,
            })
            .map_err(|_| anyhow!("Apple writer 已关闭"))?;
        Ok(result_rx)
    }

    pub(crate) fn send_high_performance_key_event(&self, down: bool, keysym: u32) -> Result<()> {
        (|| {
            let (result_tx, result_rx) = mpsc::sync_channel(1);
            self.control
                .commands
                .send(WriterCommand::HighPerformanceKeyEvent {
                    down,
                    keysym,
                    result: result_tx,
                })
                .map_err(|_| anyhow!("Apple writer 已关闭"))?;
            Self::receive_message_result(result_rx)
        })()
        .context("发送 Apple High Performance 加密按键失败")
    }

    fn receive_message_result(result_rx: Receiver<Result<()>>) -> Result<()> {
        result_rx
            .recv()
            .map_err(|_| anyhow!("Apple writer 未返回发送结果"))?
    }

    pub fn shutdown(&self) -> Result<()> {
        let worker = self
            .control
            .worker
            .lock()
            .map_err(|_| anyhow!("Apple writer join 状态已损坏"))?
            .take();
        if let Some(worker) = worker {
            let _ = self.control.interrupt.shutdown(Shutdown::Both);
            let _ = self.control.commands.send(WriterCommand::Shutdown);
            worker
                .join()
                .map_err(|_| anyhow!("Apple writer 线程异常退出"))?;
        }
        Ok(())
    }
}

fn spawn_writer_with_hooks(
    mut stream: TcpStream,
    interrupt: TcpStream,
    absolute_deadline: Option<Instant>,
    mut crypto: Option<OutboundSessionCrypto>,
    hooks: WriterHooks,
) -> AppleWriterHandle {
    let (commands_tx, commands_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        writer_loop(
            &mut stream,
            absolute_deadline,
            &mut crypto,
            commands_rx,
            hooks,
        )
    });
    AppleWriterHandle {
        control: Arc::new(WriterControl {
            commands: commands_tx,
            interrupt,
            worker: Mutex::new(Some(worker)),
        }),
    }
}

fn writer_loop(
    stream: &mut TcpStream,
    absolute_deadline: Option<Instant>,
    crypto: &mut Option<OutboundSessionCrypto>,
    commands: Receiver<WriterCommand>,
    hooks: WriterHooks,
) {
    while let Ok(command) = commands.recv() {
        let diagnostic_key_event = hooks.input_diagnostics.is_some()
            && matches!(&command, WriterCommand::HighPerformanceKeyEvent { .. });
        let (wire_result, result) = match command {
            WriterCommand::Message { plaintext, result } => {
                let wire_result = match crypto {
                    Some(crypto) => crypto.seal(&plaintext),
                    None => Ok(plaintext),
                };
                (wire_result, result)
            }
            WriterCommand::HighPerformanceKeyEvent {
                down,
                keysym,
                result,
            } => {
                let wire_result = crypto
                    .as_mut()
                    .context("Apple High Performance 加密按键要求已建立的 Apple 加密会话")
                    .and_then(|crypto| crypto.seal(&crate::protocol::msg_key_event(down, keysym)));
                (wire_result, result)
            }
            WriterCommand::Shutdown => {
                let _ = stream.shutdown(Shutdown::Both);
                break;
            }
        };
        let send_result = (|| {
            let write_timeout = if let Some(deadline) = absolute_deadline {
                deadline
                    .checked_duration_since(Instant::now())
                    .filter(|duration| !duration.is_zero())
                    .ok_or_else(cold_deadline_error)?
                    .min(PRODUCTION_WRITER_IO_TIMEOUT)
            } else {
                PRODUCTION_WRITER_IO_TIMEOUT
            };
            stream
                .set_write_timeout(Some(write_timeout))
                .context("设置 Apple writer 写超时失败")?;
            let wire = wire_result?;
            hooks.notify_write_started(wire.len());
            let write_result = stream.write_all(&wire).context("写入失败（连接中断？）");
            hooks.notify_write_finished(write_result.is_ok());
            write_result
        })();
        let failed = send_result.is_err();
        let _ = result.send(send_result);
        hooks.notify_result_delivered();
        if diagnostic_key_event && !failed {
            if let Some(diagnostics) = &hooks.input_diagnostics {
                diagnostics.observe_writer_completed(Instant::now());
                hooks.notify_input_diagnostic_queued();
            }
        }
        if failed {
            let _ = stream.shutdown(Shutdown::Both);
            break;
        }
    }
}

pub struct AppleConnection {
    stream: TcpStream,
    absolute_deadline: Option<Instant>,
    read_timeout_cap: Cell<Option<Duration>>,
    buffer: Vec<u8>,
    position: usize,
    end: usize,
    inbound_crypto: Option<InboundSessionCrypto>,
    outbound_crypto: Option<OutboundSessionCrypto>,
    wire_pending: Vec<u8>,
    writer: Option<AppleWriterHandle>,
    writer_input_diagnostics: Option<HighPerformanceInputDiagnostics>,
    #[cfg(test)]
    session_runtime_test_events: Option<Sender<SessionRuntimeTestEvent>>,
    #[cfg(test)]
    application_frame_read_test_events: Option<Sender<ApplicationFrameReadTestEvent>>,
}

impl AppleConnection {
    pub fn new(stream: TcpStream) -> Self {
        Self::new_with_optional_deadline(stream, None)
    }

    pub fn new_with_deadline(stream: TcpStream, deadline: Instant) -> Self {
        Self::new_with_optional_deadline(stream, Some(deadline))
    }

    fn new_with_optional_deadline(stream: TcpStream, absolute_deadline: Option<Instant>) -> Self {
        Self {
            stream,
            absolute_deadline,
            read_timeout_cap: Cell::new(None),
            buffer: vec![0; 16384],
            position: 0,
            end: 0,
            inbound_crypto: None,
            outbound_crypto: None,
            wire_pending: Vec::new(),
            writer: None,
            writer_input_diagnostics: None,
            #[cfg(test)]
            session_runtime_test_events: None,
            #[cfg(test)]
            application_frame_read_test_events: None,
        }
    }

    fn apply_deadline_timeouts(&self) -> Result<()> {
        let Some(deadline) = self.absolute_deadline else {
            return Ok(());
        };
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(cold_deadline_error)?;
        let read_timeout = self
            .read_timeout_cap
            .get()
            .map(|current| current.min(remaining))
            .unwrap_or(remaining);
        self.stream
            .set_read_timeout(Some(read_timeout))
            .context("设置 deadline 读超时失败")?;
        self.stream
            .set_write_timeout(Some(remaining))
            .context("设置 deadline 写超时失败")?;
        Ok(())
    }

    pub fn set_read_timeout(&self, duration: Option<Duration>) -> Result<()> {
        self.stream
            .set_read_timeout(duration)
            .context("设置读超时失败")?;
        self.read_timeout_cap.set(duration);
        #[cfg(test)]
        if let Some(duration) = duration {
            self.notify_application_frame_read_test_event(
                ApplicationFrameReadTestEvent::ReadPollSet(duration),
            );
        }
        Ok(())
    }

    pub fn read_timeout(&self) -> std::io::Result<Option<Duration>> {
        self.stream.read_timeout()
    }

    pub fn write_timeout(&self) -> std::io::Result<Option<Duration>> {
        self.stream.write_timeout()
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        self.stream.peer_addr().context("读取远端 socket 地址失败")
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.stream.local_addr().context("读取本地 socket 地址失败")
    }

    pub fn set_crypto(&mut self, crypto: SessionCrypto) -> Result<()> {
        if self.writer.is_some() || self.inbound_crypto.is_some() || self.outbound_crypto.is_some()
        {
            bail!("Apple 加密状态已经安装");
        }
        self.wire_pending = self.buffer[self.position..self.end].to_vec();
        self.position = 0;
        self.end = 0;
        let (inbound, outbound) = crypto.split();
        self.inbound_crypto = Some(inbound);
        self.outbound_crypto = Some(outbound);
        Ok(())
    }

    pub fn is_encrypted(&self) -> bool {
        self.inbound_crypto.is_some()
    }

    pub fn writer_handle(&mut self) -> Result<AppleWriterHandle> {
        self.writer_handle_with_hooks(WriterHooks::default())
    }

    pub(crate) fn install_input_diagnostics(
        &mut self,
        diagnostics: HighPerformanceInputDiagnostics,
    ) {
        if self.writer.is_none() && diagnostics.is_enabled() {
            self.writer_input_diagnostics = Some(diagnostics);
        }
    }

    #[cfg(test)]
    pub(crate) fn writer_handle_with_io_events(
        &mut self,
        io_events: Sender<WriterIoTestEvent>,
    ) -> Result<AppleWriterHandle> {
        self.writer_handle_with_hooks(WriterHooks {
            io_events: Some(io_events),
            ..WriterHooks::default()
        })
    }

    #[cfg(test)]
    pub(crate) fn set_session_runtime_test_events(
        &mut self,
        events: Sender<SessionRuntimeTestEvent>,
    ) {
        self.session_runtime_test_events = Some(events);
    }

    #[cfg(test)]
    pub(crate) fn set_application_frame_read_test_events(
        &mut self,
        events: Sender<ApplicationFrameReadTestEvent>,
    ) {
        self.application_frame_read_test_events = Some(events);
    }

    #[cfg(test)]
    pub(crate) fn notify_session_runtime_test_event(&self, event: SessionRuntimeTestEvent) {
        if let Some(events) = &self.session_runtime_test_events {
            let _ = events.send(event);
        }
    }

    #[cfg(test)]
    fn notify_application_frame_read_test_event(&self, event: ApplicationFrameReadTestEvent) {
        if let Some(events) = &self.application_frame_read_test_events {
            let _ = events.send(event);
        }
    }

    fn writer_handle_with_hooks(&mut self, mut hooks: WriterHooks) -> Result<AppleWriterHandle> {
        if let Some(writer) = &self.writer {
            return Ok(writer.clone());
        }
        if hooks.input_diagnostics.is_none() {
            hooks.input_diagnostics = self.writer_input_diagnostics.take();
        }
        let stream = self
            .stream
            .try_clone()
            .context("无法复制 Apple writer socket")?;
        let interrupt = self
            .stream
            .try_clone()
            .context("无法复制 Apple writer 中断 socket")?;
        let writer = spawn_writer_with_hooks(
            stream,
            interrupt,
            self.absolute_deadline,
            self.outbound_crypto.take(),
            hooks,
        );
        self.writer = Some(writer.clone());
        Ok(writer)
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        if self.inbound_crypto.is_some() || self.writer.is_some() {
            return self.writer_handle()?.send_private_message(bytes);
        }
        self.apply_deadline_timeouts()?;
        self.stream
            .write_all(bytes)
            .context("写入失败（连接中断？）")
    }

    pub fn read_app_frame(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(data) = self.read_app_frame_step()? {
                return Ok(data);
            }
        }
    }

    pub fn read_app_frame_step(&mut self) -> Result<Option<Vec<u8>>> {
        if self.inbound_crypto.is_none() {
            bail!("连接未挂载加密");
        }
        if self.position < self.end {
            bail!("读缓冲有未消费数据，帧模式与流模式混用");
        }
        if let Some(ciphertext) = take_wire_ciphertext_frame(&mut self.wire_pending)? {
            return self
                .inbound_crypto
                .as_mut()
                .expect("加密状态已检查")
                .open(&ciphertext)
                .map(Some);
        }
        let mut temporary = [0; 16384];
        #[cfg(test)]
        if let Some(duration) = self.read_timeout_cap.get() {
            self.notify_application_frame_read_test_event(
                ApplicationFrameReadTestEvent::ApplicationFrameRead(duration),
            );
        }
        self.apply_deadline_timeouts()?;
        let received = self
            .stream
            .read(&mut temporary)
            .context("读取失败（连接可能已被服务器关闭）")?;
        if received == 0 {
            return Err(PeerClosed.into());
        }
        self.wire_pending.extend_from_slice(&temporary[..received]);
        let Some(ciphertext) = take_wire_ciphertext_frame(&mut self.wire_pending)? else {
            return Ok(None);
        };
        self.inbound_crypto
            .as_mut()
            .expect("加密状态已检查")
            .open(&ciphertext)
            .map(Some)
    }

    fn inject(&mut self, data: &[u8]) {
        let remaining = self.buffer[self.position..self.end].to_vec();
        let mut joined = data.to_vec();
        joined.extend_from_slice(&remaining);
        if joined.len() > self.buffer.len() {
            self.buffer.resize(joined.len(), 0);
        }
        self.buffer[..joined.len()].copy_from_slice(&joined);
        self.position = 0;
        self.end = joined.len();
    }

    pub fn read_exact_bytes(&mut self, output: &mut [u8]) -> Result<()> {
        let requested = output.len();
        let mut filled = 0;
        while filled < requested {
            if self.position < self.end {
                let take = (self.end - self.position).min(requested - filled);
                output[filled..filled + take]
                    .copy_from_slice(&self.buffer[self.position..self.position + take]);
                self.position += take;
                filled += take;
            } else if self.inbound_crypto.is_some() {
                loop {
                    if let Some(ciphertext) = take_wire_ciphertext_frame(&mut self.wire_pending)? {
                        let data = self
                            .inbound_crypto
                            .as_mut()
                            .expect("加密状态已检查")
                            .open(&ciphertext)?;
                        self.inject(&data);
                        break;
                    }
                    let mut temporary = [0; 16384];
                    self.apply_deadline_timeouts()?;
                    let received = self
                        .stream
                        .read(&mut temporary)
                        .context("读取失败（连接可能已被服务器关闭）")?;
                    if received == 0 {
                        return Err(PeerClosed.into());
                    }
                    self.wire_pending.extend_from_slice(&temporary[..received]);
                }
            } else if requested - filled >= self.buffer.len() {
                self.apply_deadline_timeouts()?;
                let received = self
                    .stream
                    .read(&mut output[filled..])
                    .context("读取失败（连接可能已被服务器关闭）")?;
                if received == 0 {
                    return Err(PeerClosed.into());
                }
                filled += received;
            } else {
                self.position = 0;
                self.end = 0;
                self.apply_deadline_timeouts()?;
                let received = self
                    .stream
                    .read(&mut self.buffer)
                    .context("读取失败（连接可能已被服务器关闭）")?;
                if received == 0 {
                    return Err(PeerClosed.into());
                }
                self.end = received;
            }
        }
        Ok(())
    }

    pub fn read_vec(&mut self, length: usize) -> Result<Vec<u8>> {
        let mut bytes = vec![0; length];
        self.read_exact_bytes(&mut bytes)?;
        Ok(bytes)
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        let mut bytes = [0; 1];
        self.read_exact_bytes(&mut bytes)?;
        Ok(bytes[0])
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        let mut bytes = [0; 2];
        self.read_exact_bytes(&mut bytes)?;
        Ok(u16::from_be_bytes(bytes))
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        let mut bytes = [0; 4];
        self.read_exact_bytes(&mut bytes)?;
        Ok(u32::from_be_bytes(bytes))
    }

    pub fn shutdown(&self) -> Result<()> {
        if let Some(writer) = &self.writer {
            writer.shutdown()?;
        }
        self.stream
            .shutdown(Shutdown::Both)
            .context("关闭 Apple 连接失败")
    }
}

impl Drop for AppleConnection {
    fn drop(&mut self) {
        if let Some(writer) = &self.writer {
            let _ = writer.shutdown();
        }
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn hp_keyboard_protocol_diagnostics_queue_counts_and_each_terminal_exactly_once() {
        let started_at = Instant::now();
        for (terminal, expected_code) in [
            (
                HighPerformanceInputTerminal::LocalDisconnect,
                "local_disconnect",
            ),
            (HighPerformanceInputTerminal::CleanClose, "clean_close"),
            (HighPerformanceInputTerminal::PeerClosed, "peer_close"),
            (HighPerformanceInputTerminal::AdapterError, "adapter_error"),
        ] {
            let (diagnostics, stage_lines, terminal_lines) =
                HighPerformanceInputDiagnostics::enabled_for_test(started_at, 8);
            diagnostics.observe_runtime_consumed(started_at + Duration::from_millis(5));
            diagnostics.observe_writer_completed(started_at + Duration::from_millis(8));
            assert!(diagnostics.observe_terminal(terminal, started_at + Duration::from_millis(21)));
            assert!(!diagnostics.observe_terminal(
                HighPerformanceInputTerminal::AdapterError,
                started_at + Duration::from_millis(34)
            ));

            let mut lines = stage_lines.try_iter().collect::<Vec<_>>();
            lines.extend(terminal_lines.try_iter());
            assert_eq!(lines.len(), 3);
            assert_eq!(
                lines[0],
                HighPerformanceInputLine::Stage {
                    stage: "runtime_consumed",
                    count: 1,
                    elapsed_ms: 5,
                }
            );
            assert_eq!(
                lines[1],
                HighPerformanceInputLine::Stage {
                    stage: "writer_completed",
                    count: 1,
                    elapsed_ms: 8,
                }
            );
            assert_eq!(
                lines[2],
                HighPerformanceInputLine::Terminal {
                    kind: expected_code,
                    runtime_consumed: 1,
                    writer_completed: 1,
                    since_last_writer_ms: Some(13),
                    elapsed_ms: 21,
                }
            );
            let rendered = lines[2].to_string();
            for forbidden in ["keysym", "text", "username", "password", "address"] {
                assert!(!rendered.contains(forbidden));
            }
        }
    }

    #[test]
    fn installing_hp_input_diagnostics_does_not_create_the_writer() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = thread::spawn(move || listener.accept().unwrap().0);
        let stream = TcpStream::connect(address).unwrap();
        let mut connection = AppleConnection::new(stream);
        let (diagnostics, _stage_lines, _terminal_lines) =
            HighPerformanceInputDiagnostics::enabled_for_test(Instant::now(), 1);

        connection.install_input_diagnostics(diagnostics);

        assert!(connection.writer.is_none());
        drop(connection);
        drop(peer.join().unwrap());
    }

    #[test]
    fn writer_delivers_key_result_before_queueing_diagnostics() {
        let outer_key = [0x51; 16];
        let outer_iv = [0x61; 16];
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let mut prefix = [0u8; 2];
            peer.read_exact(&mut prefix).unwrap();
            let mut ciphertext = vec![0u8; usize::from(u16::from_be_bytes(prefix))];
            peer.read_exact(&mut ciphertext).unwrap();
        });
        let stream = TcpStream::connect(address).unwrap();
        let mut connection = AppleConnection::new(stream);
        connection
            .set_crypto(SessionCrypto::from_key_iv(outer_key, outer_iv))
            .unwrap();
        let (diagnostics, _stage_lines, _terminal_lines) =
            HighPerformanceInputDiagnostics::enabled_for_test(Instant::now(), 4);
        connection.install_input_diagnostics(diagnostics);
        let (events_tx, events_rx) = mpsc::channel();
        let writer = connection.writer_handle_with_io_events(events_tx).unwrap();

        writer.send_high_performance_key_event(true, 0x61).unwrap();

        assert!(matches!(
            events_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            WriterIoTestEvent::Started { .. }
        ));
        assert_eq!(
            events_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            WriterIoTestEvent::Finished { succeeded: true }
        );
        assert_eq!(
            events_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            WriterIoTestEvent::ResultDelivered
        );
        assert_eq!(
            events_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            WriterIoTestEvent::InputDiagnosticQueued
        );
        writer.shutdown().unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn high_performance_key_commands_are_standard_rfb_inside_outer_session_crypto() {
        let outer_key = [0x31; 16];
        let outer_iv = [0x42; 16];
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let mut outer_crypto = SessionCrypto::from_key_iv(outer_key, outer_iv);
            let mut messages = Vec::new();
            for _ in 0..2 {
                let mut prefix = [0u8; 2];
                peer.read_exact(&mut prefix).unwrap();
                let mut ciphertext = vec![0u8; usize::from(u16::from_be_bytes(prefix))];
                peer.read_exact(&mut ciphertext).unwrap();
                messages.push(outer_crypto.open(&ciphertext).unwrap());
            }
            messages
        });

        let stream = TcpStream::connect(address).unwrap();
        let mut connection = AppleConnection::new(stream);
        connection
            .set_crypto(SessionCrypto::from_key_iv(outer_key, outer_iv))
            .unwrap();
        let writer = connection.writer_handle().unwrap();
        writer.send_high_performance_key_event(true, 0x61).unwrap();
        writer.send_high_performance_key_event(false, 0x61).unwrap();
        writer.shutdown().unwrap();

        let messages = server.join().unwrap();
        assert_eq!(messages[0], crate::protocol::msg_key_event(true, 0x61));
        assert_eq!(messages[1], crate::protocol::msg_key_event(false, 0x61));
    }

    #[test]
    fn high_performance_key_without_session_crypto_has_safe_operation_context() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });
        let stream = TcpStream::connect(address).unwrap();
        let mut connection = AppleConnection::new(stream);
        let writer = connection.writer_handle().unwrap();

        let error = writer
            .send_high_performance_key_event(true, 0x61)
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "发送 Apple High Performance 加密按键失败"
        );
        let chain = format!("{error:#}");
        assert!(chain.contains("要求已建立的 Apple 加密会话"));
        assert!(!chain.contains("0x61"));
        writer.shutdown().unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn encrypted_frame_prefix_read_app_frame_and_stream_refills_decrypt_same_literal_wire_frame() {
        const WIRE_FRAME: [u8; 34] = [
            0x00, 0x20, 0xd4, 0x0f, 0x3e, 0x05, 0x98, 0x0f, 0x86, 0xde, 0x8c, 0x7d, 0x28, 0x28,
            0xdf, 0xf9, 0xf6, 0x02, 0x67, 0x0e, 0xf5, 0xaa, 0xb5, 0x0a, 0x17, 0x45, 0x98, 0xca,
            0x07, 0xc7, 0xe8, 0x22, 0x9a, 0x43,
        ];

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&WIRE_FRAME[1..]).unwrap();
        });
        let stream = TcpStream::connect(address).unwrap();
        let mut app_connection = AppleConnection::new(stream);
        app_connection
            .set_crypto(SessionCrypto::from_key_iv([0x11; 16], [0x22; 16]))
            .unwrap();
        app_connection
            .wire_pending
            .extend_from_slice(&WIRE_FRAME[..1]);
        assert_eq!(app_connection.read_app_frame().unwrap(), b"same");
        server.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&WIRE_FRAME[7..]).unwrap();
        });
        let stream = TcpStream::connect(address).unwrap();
        let mut stream_connection = AppleConnection::new(stream);
        stream_connection
            .set_crypto(SessionCrypto::from_key_iv([0x11; 16], [0x22; 16]))
            .unwrap();
        stream_connection
            .wire_pending
            .extend_from_slice(&WIRE_FRAME[..7]);
        let mut plaintext = [0u8; 4];
        stream_connection.read_exact_bytes(&mut plaintext).unwrap();
        assert_eq!(&plaintext, b"same");
        server.join().unwrap();
    }

    #[test]
    fn encrypted_app_frame_step_yields_after_one_incomplete_socket_read() {
        let key = [0x31; 16];
        let iv = [0x42; 16];
        let mut sender_crypto = SessionCrypto::from_key_iv(key, iv);
        let wire = sender_crypto.seal(b"incremental").unwrap();
        let (first_byte_sent, first_byte_ready) = std::sync::mpsc::channel();
        let (remainder_ready, send_remainder) = std::sync::mpsc::channel();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&wire[..1]).unwrap();
            first_byte_sent.send(()).unwrap();
            send_remainder.recv().unwrap();
            stream.write_all(&wire[1..]).unwrap();
        });

        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut connection = AppleConnection::new(stream);
        connection
            .set_crypto(SessionCrypto::from_key_iv(key, iv))
            .unwrap();
        first_byte_ready.recv().unwrap();

        assert_eq!(connection.read_app_frame_step().unwrap(), None);
        remainder_ready.send(()).unwrap();
        let plaintext = loop {
            if let Some(plaintext) = connection.read_app_frame_step().unwrap() {
                break plaintext;
            }
        };
        assert_eq!(plaintext, b"incremental");
        server.join().unwrap();
    }

    #[test]
    fn sequential_deadline_reads_and_writes_refresh_both_socket_timeouts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for expected in [0x11_u8, 0x22] {
                let mut byte = [0_u8; 1];
                stream.read_exact(&mut byte).unwrap();
                assert_eq!(byte[0], expected);
                stream.write_all(&[expected.wrapping_add(1)]).unwrap();
            }
        });
        let stream = TcpStream::connect(address).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut connection = AppleConnection::new_with_deadline(stream, deadline);

        connection.write_all(&[0x11]).unwrap();
        assert_eq!(connection.read_u8().unwrap(), 0x12);
        connection
            .stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        connection
            .stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        connection.write_all(&[0x22]).unwrap();
        assert_eq!(connection.read_u8().unwrap(), 0x23);

        let read_timeout = connection.read_timeout().unwrap().unwrap();
        let write_timeout = connection.write_timeout().unwrap().unwrap();
        assert!(read_timeout > Duration::from_secs(10));
        assert!(write_timeout > Duration::from_secs(10));
        assert!(read_timeout <= Duration::from_secs(30));
        assert!(write_timeout <= Duration::from_secs(30));
        server.join().unwrap();
    }

    #[test]
    fn shutdown_interrupts_in_flight_write_and_disconnects_queued_callers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (peer_ready_tx, peer_ready_rx) = mpsc::channel();
        let (release_peer_tx, release_peer_rx) = mpsc::channel();
        let peer = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            peer_ready_tx.send(()).unwrap();
            let _ = release_peer_rx.recv();
        });

        let stream = TcpStream::connect(address).unwrap();
        peer_ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut connection = AppleConnection::new(stream);
        let (writer_io_tx, writer_io_rx) = mpsc::channel();
        let writer = connection
            .writer_handle_with_io_events(writer_io_tx)
            .unwrap();

        let active_writer = writer.clone();
        let active_send = thread::spawn(move || {
            active_writer.send_private_message(&vec![0x5a; 64 * 1024 * 1024])
        });
        let event = writer_io_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer must enter write_all before shutdown");
        assert!(matches!(event, WriterIoTestEvent::Started { .. }));

        let queued_one = writer.enqueue_private_message(vec![0x11; 8]).unwrap();
        let queued_two = writer.enqueue_private_message(vec![0x22; 8]).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let shutdown_threads: Vec<_> = [writer.clone(), writer.clone()]
            .into_iter()
            .map(|writer| {
                let barrier = barrier.clone();
                let shutdown_tx = shutdown_tx.clone();
                thread::spawn(move || {
                    barrier.wait();
                    shutdown_tx.send(writer.shutdown()).unwrap();
                })
            })
            .collect();
        barrier.wait();
        drop(shutdown_tx);

        let shutdown_results = (0..2)
            .map(|_| {
                shutdown_rx
                    .recv_timeout(Duration::from_secs(3))
                    .expect("shutdown must be bounded")
            })
            .collect::<Vec<_>>();
        let active_result = active_send.join().unwrap();
        let queued_results =
            [queued_one, queued_two].map(AppleWriterHandle::receive_message_result);
        for thread in shutdown_threads {
            thread.join().unwrap();
        }
        let repeated_writer_shutdown = writer.shutdown();
        let connection_shutdown = connection.shutdown();

        let _ = release_peer_tx.send(());
        peer.join().unwrap();

        assert!(shutdown_results.into_iter().all(|result| result.is_ok()));
        let active_error = active_result.unwrap_err();
        assert_eq!(active_error.to_string(), "写入失败（连接中断？）");
        for queued_result in queued_results {
            let queued_error = queued_result.unwrap_err();
            assert_eq!(queued_error.to_string(), "Apple writer 未返回发送结果");
        }
        assert!(repeated_writer_shutdown.is_ok());
        if let Err(error) = connection_shutdown {
            assert!(
                error
                    .chain()
                    .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
                    .any(|io_error| io_error.kind() == std::io::ErrorKind::NotConnected),
                "重复关闭 Apple 连接只能返回 NotConnected，实际为 {error:#}"
            );
        }
    }
}
