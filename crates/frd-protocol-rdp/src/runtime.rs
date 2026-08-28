use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use frd_protocol_api::{ProtocolError, ProtocolExit, ProtocolRuntime, SessionCommand};
use ironrdp::pdu::input::fast_path::FastPathInputEvent;

use crate::active_session::run_active_session;
use crate::config::RdpConnectionConfig;
use crate::connector::connect_and_activate;
use crate::error::{rdp_error, RDP_ACTIVATION_FAILED, RDP_CANCELLED};
use crate::input::{RdpInputError, RdpInputState};

const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const NETWORK_STAGE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) enum ActiveCommandBatch {
    Continue(Vec<FastPathInputEvent>),
    Disconnect(Vec<FastPathInputEvent>),
    Terminal(Vec<FastPathInputEvent>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReactivationCommand {
    Continue,
    Disconnect,
    Terminal,
}

pub(crate) fn drain_active_commands(
    runtime: &mut ProtocolRuntime,
    input: &mut RdpInputState,
) -> Result<ActiveCommandBatch, RdpInputError> {
    if runtime.requires_shutdown() {
        return Ok(ActiveCommandBatch::Terminal(input.stop()));
    }

    let mut events = Vec::new();
    while let Some(command) = runtime.try_next_command() {
        match command {
            SessionCommand::Input(session_input) => {
                events.extend(input.translate(session_input.event)?);
            }
            SessionCommand::Disconnect => {
                events.extend(input.stop());
                return Ok(ActiveCommandBatch::Disconnect(events));
            }
            SessionCommand::ViewportChanged { .. }
            | SessionCommand::ResolveServerIdentity { .. }
            | SessionCommand::ClipboardWrite(_) => {}
        }
    }
    Ok(ActiveCommandBatch::Continue(events))
}

pub(crate) fn drain_reactivation_commands(runtime: &mut ProtocolRuntime) -> ReactivationCommand {
    if runtime.requires_shutdown() {
        return ReactivationCommand::Terminal;
    }
    while let Some(command) = runtime.try_next_command() {
        if matches!(command, SessionCommand::Disconnect) {
            return ReactivationCommand::Disconnect;
        }
    }
    if runtime.requires_shutdown() {
        ReactivationCommand::Terminal
    } else {
        ReactivationCommand::Continue
    }
}

struct TrackedWorker {
    cancellation: StageCancellation,
    handle: thread::JoinHandle<()>,
}

#[derive(Clone)]
pub(crate) struct StageCancellation {
    cancelled: Arc<AtomicBool>,
}

impl StageCancellation {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

pub(crate) struct CancellationCheckedIo<S> {
    inner: S,
    cancellation: StageCancellation,
}

impl<S> CancellationCheckedIo<S> {
    pub(crate) fn new(inner: S, cancellation: StageCancellation) -> Self {
        Self {
            inner,
            cancellation,
        }
    }

    pub(crate) fn into_inner(self) -> S {
        self.inner
    }

    fn check_cancelled(&self) -> io::Result<()> {
        if self.cancellation.is_cancelled() {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "RDP stage cancelled",
            ))
        } else {
            Ok(())
        }
    }
}

impl<S: Read> Read for CancellationCheckedIo<S> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check_cancelled()?;
        let result = self.inner.read(buffer);
        self.check_cancelled()?;
        result
    }
}

impl<S: Write> Write for CancellationCheckedIo<S> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.check_cancelled()?;
        let result = self.inner.write(buffer);
        self.check_cancelled()?;
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check_cancelled()?;
        let result = self.inner.flush();
        self.check_cancelled()?;
        result
    }
}

static BLOCKING_STAGE_WORKER: OnceLock<Mutex<Option<TrackedWorker>>> = OnceLock::new();
static NETWORK_STAGE_WORKER: OnceLock<Mutex<Option<TrackedWorker>>> = OnceLock::new();

pub(crate) fn run_protocol_session(
    mut config: RdpConnectionConfig,
    mut runtime: ProtocolRuntime,
) -> ProtocolExit {
    let session_id = config.request.session_id;
    let executor = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(executor) => executor,
        Err(_) => return ProtocolExit::Failed(rdp_error(RDP_ACTIVATION_FAILED)),
    };

    let result = match executor.block_on(connect_and_activate(&mut config, &mut runtime)) {
        Ok(session) => run_active_session(session, session_id, &mut runtime),
        Err(error) if error.code() == RDP_CANCELLED => ProtocolExit::Closed,
        Err(error) => ProtocolExit::Failed(error),
    };
    drop(config);
    drop(executor);
    result
}

pub(crate) async fn wait_for_network_future<T, E, F>(
    runtime: &mut ProtocolRuntime,
    future: F,
    failure_code: &'static str,
) -> Result<T, ProtocolError>
where
    T: Send + 'static,
    E: Send + 'static,
    F: Future<Output = Result<T, E>> + Send + 'static,
{
    if disconnect_requested(runtime) {
        return Err(cancelled());
    }

    let result_rx =
        start_tracked_worker(network_stage_worker(), "frd-rdp-network-stage", move |_| {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|executor| executor.block_on(future));
            result
        })
        .map_err(|()| rdp_error(failure_code))?;
    let deadline = Instant::now() + NETWORK_STAGE_TIMEOUT;

    loop {
        if disconnect_requested(runtime) {
            cancel_tracked_worker(network_stage_worker());
            return Err(cancelled());
        }
        match result_rx.try_recv() {
            Ok(Ok(Ok(value))) => {
                finish_tracked_worker(network_stage_worker());
                return Ok(value);
            }
            Ok(Ok(Err(_))) | Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                finish_tracked_worker(network_stage_worker());
                return Err(rdp_error(failure_code));
            }
            Err(TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            cancel_tracked_worker(network_stage_worker());
            return Err(rdp_error(failure_code));
        }
        let wait = COMMAND_POLL_INTERVAL.min(deadline.duration_since(now));
        tokio::time::sleep(wait).await;
    }
}

pub(crate) async fn wait_for_blocking<T>(
    runtime: &mut ProtocolRuntime,
    shutdown: TcpStream,
    failure_code: &'static str,
    operation: impl FnOnce(StageCancellation) -> T + Send + 'static,
) -> Result<T, ProtocolError>
where
    T: Send + 'static,
{
    wait_for_blocking_with_timeout(
        runtime,
        shutdown,
        failure_code,
        NETWORK_STAGE_TIMEOUT,
        operation,
    )
    .await
}

async fn wait_for_blocking_with_timeout<T>(
    runtime: &mut ProtocolRuntime,
    shutdown: TcpStream,
    failure_code: &'static str,
    timeout: Duration,
    operation: impl FnOnce(StageCancellation) -> T + Send + 'static,
) -> Result<T, ProtocolError>
where
    T: Send + 'static,
{
    if disconnect_requested(runtime) {
        let _ = shutdown.shutdown(Shutdown::Both);
        return Err(cancelled());
    }

    let result_rx =
        start_tracked_worker(blocking_stage_worker(), "frd-rdp-blocking-stage", operation)
            .map_err(|()| rdp_error(failure_code))?;
    let deadline = Instant::now() + timeout;
    loop {
        if disconnect_requested(runtime) {
            cancel_tracked_worker(blocking_stage_worker());
            let _ = shutdown.shutdown(Shutdown::Both);
            return Err(cancelled());
        }
        match result_rx.try_recv() {
            Ok(value) => {
                finish_tracked_worker(blocking_stage_worker());
                return Ok(value);
            }
            Err(TryRecvError::Disconnected) => {
                finish_tracked_worker(blocking_stage_worker());
                return Err(rdp_error(failure_code));
            }
            Err(TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            cancel_tracked_worker(blocking_stage_worker());
            let _ = shutdown.shutdown(Shutdown::Both);
            return Err(rdp_error(failure_code));
        }
        let wait = COMMAND_POLL_INTERVAL.min(deadline.duration_since(now));
        tokio::time::sleep(wait).await;
    }
}

fn blocking_stage_worker() -> &'static Mutex<Option<TrackedWorker>> {
    BLOCKING_STAGE_WORKER.get_or_init(|| Mutex::new(None))
}

fn network_stage_worker() -> &'static Mutex<Option<TrackedWorker>> {
    NETWORK_STAGE_WORKER.get_or_init(|| Mutex::new(None))
}

fn start_tracked_worker<T>(
    registry: &'static Mutex<Option<TrackedWorker>>,
    name: &'static str,
    operation: impl FnOnce(StageCancellation) -> T + Send + 'static,
) -> Result<Receiver<T>, ()>
where
    T: Send + 'static,
{
    let mut worker = registry.lock().map_err(|_| ())?;
    if worker
        .as_ref()
        .is_some_and(|tracked| !tracked.handle.is_finished())
    {
        return Err(());
    }
    if let Some(finished) = worker.take() {
        let _ = finished.handle.join();
    }

    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let cancellation = StageCancellation::new();
    let worker_cancellation = cancellation.clone();
    let handle = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _ = result_tx.send(operation(worker_cancellation));
        })
        .map_err(|_| ())?;
    *worker = Some(TrackedWorker {
        cancellation,
        handle,
    });
    Ok(result_rx)
}

fn cancel_tracked_worker(registry: &'static Mutex<Option<TrackedWorker>>) {
    let Ok(worker) = registry.lock() else {
        return;
    };
    if let Some(tracked) = worker.as_ref() {
        tracked.cancellation.cancel();
    }
}

fn finish_tracked_worker(registry: &'static Mutex<Option<TrackedWorker>>) {
    let Ok(mut worker) = registry.lock() else {
        return;
    };
    if let Some(finished) = worker.take() {
        let _ = finished.handle.join();
    }
}

fn disconnect_requested(runtime: &mut ProtocolRuntime) -> bool {
    while let Some(command) = runtime.try_next_command() {
        if matches!(command, SessionCommand::Disconnect) {
            return true;
        }
    }
    false
}

fn cancelled() -> ProtocolError {
    rdp_error(RDP_CANCELLED)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::thread;
    use std::time::{Duration, Instant};

    use frd_core::{
        InputEvent, KeyState, Modifiers, PhysicalKeyCode, SecretBuffer, SessionId, SessionInput,
    };
    use frd_protocol_api::{
        ConnectRequest, ConnectionStage, Credentials, Endpoint, ProtocolError, ProtocolExit,
        ProtocolId, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCommand, SessionEvent,
        SurfacePublisher,
    };

    use crate::config::RdpConnectionConfig;
    use crate::error::{RDP_DNS_FAILED, RDP_TLS_FAILED};
    use crate::input::RdpInputState;

    use super::{
        drain_active_commands, drain_reactivation_commands, run_protocol_session,
        wait_for_blocking, wait_for_blocking_with_timeout, wait_for_network_future,
        ActiveCommandBatch, CancellationCheckedIo,
    };

    static WORKER_TEST_SERIAL: Mutex<()> = Mutex::new(());

    fn worker_test_guard() -> MutexGuard<'static, ()> {
        WORKER_TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn input_active_command_batch_drops_stale_generation_before_translation() {
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events, _event_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(AcceptingFrames),
            None,
            Box::new(NoopWake),
        );
        runtime
            .begin_generation(
                session_id,
                1,
                frd_core::PixelSize::new(2, 2).expect("valid size"),
                frd_frame::PixelFormat::Bgrx8UnormSrgb,
            )
            .expect("generation begins");
        commands
            .send(SessionCommand::Input(SessionInput {
                session_id,
                generation: 2,
                event: key_event(0x1e, KeyState::Pressed),
            }))
            .expect("runtime command receiver remains open");
        commands
            .send(SessionCommand::Input(SessionInput {
                session_id,
                generation: 1,
                event: key_event(0x30, KeyState::Pressed),
            }))
            .expect("runtime command receiver remains open");
        let mut input = RdpInputState::new();

        let ActiveCommandBatch::Continue(events) =
            drain_active_commands(&mut runtime, &mut input).expect("current input translates")
        else {
            panic!("no shutdown command was queued");
        };

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            ironrdp::pdu::input::fast_path::FastPathInputEvent::KeyboardEvent(flags, 0x30)
                if flags.is_empty()
        ));
    }

    #[test]
    fn lifecycle_disconnect_batch_releases_held_input_and_stops_acceptance() {
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events, _event_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(AcceptingFrames),
            None,
            Box::new(NoopWake),
        );
        runtime
            .begin_generation(
                session_id,
                1,
                frd_core::PixelSize::new(2, 2).expect("valid size"),
                frd_frame::PixelFormat::Bgrx8UnormSrgb,
            )
            .expect("generation begins");
        commands
            .send(SessionCommand::Input(SessionInput {
                session_id,
                generation: 1,
                event: key_event(0xe01d, KeyState::Pressed),
            }))
            .expect("runtime command receiver remains open");
        let mut input = RdpInputState::new();
        assert!(matches!(
            drain_active_commands(&mut runtime, &mut input),
            Ok(ActiveCommandBatch::Continue(events)) if events.len() == 1
        ));

        commands
            .send(SessionCommand::Disconnect)
            .expect("runtime command receiver remains open");
        let ActiveCommandBatch::Disconnect(releases) =
            drain_active_commands(&mut runtime, &mut input).expect("disconnect cannot fail")
        else {
            panic!("disconnect must stop the active input batch");
        };

        assert_eq!(
            releases,
            vec![
                ironrdp::pdu::input::fast_path::FastPathInputEvent::KeyboardEvent(
                    ironrdp::pdu::input::fast_path::KeyboardFlags::RELEASE
                        | ironrdp::pdu::input::fast_path::KeyboardFlags::EXTENDED,
                    0x1d,
                )
            ]
        );
        assert_eq!(
            input.translate(InputEvent::ReleaseAll),
            Err(crate::input::RdpInputError::Stopped)
        );
    }

    #[test]
    fn lifecycle_disconnect_preserves_queued_input_before_release_all() {
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events, _event_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(AcceptingFrames),
            None,
            Box::new(NoopWake),
        );
        runtime
            .begin_generation(
                session_id,
                1,
                frd_core::PixelSize::new(2, 2).expect("valid size"),
                frd_frame::PixelFormat::Bgrx8UnormSrgb,
            )
            .expect("generation begins");
        commands
            .send(SessionCommand::Input(SessionInput {
                session_id,
                generation: 1,
                event: key_event(0x1e, KeyState::Pressed),
            }))
            .expect("input sends");
        commands
            .send(SessionCommand::Disconnect)
            .expect("disconnect sends");
        let mut input = RdpInputState::new();

        let ActiveCommandBatch::Disconnect(events) =
            drain_active_commands(&mut runtime, &mut input).expect("batch translates")
        else {
            panic!("disconnect must finish the batch");
        };
        assert!(matches!(
            events.as_slice(),
            [
                ironrdp::pdu::input::fast_path::FastPathInputEvent::KeyboardEvent(press, 0x1e),
                ironrdp::pdu::input::fast_path::FastPathInputEvent::KeyboardEvent(release, 0x1e),
            ] if press.is_empty()
                && *release == ironrdp::pdu::input::fast_path::KeyboardFlags::RELEASE
        ));
    }

    #[test]
    fn lifecycle_reactivation_drops_input_until_the_new_generation_is_ready() {
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events, _event_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(AcceptingFrames),
            None,
            Box::new(NoopWake),
        );
        runtime
            .begin_generation(
                session_id,
                1,
                frd_core::PixelSize {
                    width: 1280,
                    height: 720,
                },
                frd_frame::PixelFormat::Bgrx8UnormSrgb,
            )
            .expect("generation begins");
        commands
            .send(SessionCommand::Input(SessionInput {
                session_id,
                generation: 1,
                event: InputEvent::PointerMove {
                    remote: frd_core::PixelPoint { x: 10, y: 20 },
                },
            }))
            .expect("input command sends");

        assert_eq!(
            drain_reactivation_commands(&mut runtime),
            super::ReactivationCommand::Continue
        );
        assert!(runtime.try_next_command().is_none());
    }

    fn key_event(code: u32, state: KeyState) -> InputEvent {
        InputEvent::PhysicalKey {
            code: PhysicalKeyCode(code),
            state,
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn lifecycle_cancelled_credential_transport_worker_is_tracked_single_flight() {
        let _serial = worker_test_guard();
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events, _) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RejectingFrames),
            None,
            Box::new(NoopWake),
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
        let transport = TcpStream::connect(listener.local_addr().expect("test listener address"))
            .expect("connect test stream");
        let (_server, _) = listener.accept().expect("accept test stream");
        let shutdown = transport.try_clone().expect("clone test stream");
        let starts = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let owner = CredentialTransportOwner {
            _credential: "credential-canary".to_owned(),
            _transport: transport,
            dropped: dropped.clone(),
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let controller = thread::spawn(move || {
            started_rx.recv().expect("credentialed worker must start");
            commands
                .send(SessionCommand::Disconnect)
                .expect("runtime command receiver remains open");
        });
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let first_starts = starts.clone();

        let first = executor.block_on(wait_for_blocking(
            &mut runtime,
            shutdown,
            RDP_TLS_FAILED,
            move |_| {
                first_starts.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).expect("test controller remains open");
                release_rx.recv().expect("test release remains open");
                drop(owner);
            },
        ));
        controller.join().expect("test controller exits");
        assert!(!dropped.load(Ordering::SeqCst));

        let mut repeated = Vec::new();
        for _ in 0..3 {
            let listener =
                TcpListener::bind(("127.0.0.1", 0)).expect("bind repeated test listener");
            let repeated_stream = TcpStream::connect(
                listener
                    .local_addr()
                    .expect("repeated test listener address"),
            )
            .expect("connect repeated test stream");
            let (_repeated_server, _) = listener.accept().expect("accept repeated test stream");
            let repeated_starts = starts.clone();
            repeated.push(executor.block_on(wait_for_blocking_with_timeout(
                &mut runtime,
                repeated_stream,
                RDP_TLS_FAILED,
                Duration::from_millis(50),
                move |_| {
                    repeated_starts.fetch_add(1, Ordering::SeqCst);
                },
            )));
        }
        release_tx
            .send(())
            .expect("tracked credentialed worker remains releasable");
        let cleanup_deadline = Instant::now() + Duration::from_secs(1);
        while !dropped.load(Ordering::SeqCst) && Instant::now() < cleanup_deadline {
            thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(
            first
                .expect_err("Disconnect must cancel the first worker")
                .code(),
            "rdp_cancelled"
        );
        assert!(repeated.into_iter().all(|attempt| {
            attempt
                .expect_err("a tracked in-flight worker must reject every repeated start")
                .code()
                == "rdp_tls_failed"
        }));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(dropped.load(Ordering::SeqCst));
    }

    struct CredentialTransportOwner {
        _credential: String,
        _transport: TcpStream,
        dropped: Arc<AtomicBool>,
    }

    impl Drop for CredentialTransportOwner {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn lifecycle_cancelled_credential_stage_rejects_post_cancel_protocol_io() {
        let _serial = worker_test_guard();
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events, _) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RejectingFrames),
            None,
            Box::new(NoopWake),
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
        let shutdown = TcpStream::connect(listener.local_addr().expect("test listener address"))
            .expect("connect test stream");
        let (_server, _) = listener.accept().expect("accept test stream");
        let writes = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let controller = thread::spawn(move || {
            started_rx.recv().expect("credentialed worker must start");
            commands
                .send(SessionCommand::Disconnect)
                .expect("runtime command receiver remains open");
        });
        let worker_writes = writes.clone();
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let result = executor.block_on(wait_for_blocking(
            &mut runtime,
            shutdown,
            RDP_TLS_FAILED,
            move |cancellation| {
                let mut io = CancellationCheckedIo::new(
                    CountingWriter {
                        writes: worker_writes,
                    },
                    cancellation,
                );
                started_tx.send(()).expect("test controller remains open");
                release_rx.recv().expect("test release remains open");
                let write_result = io.write_all(b"post-cancel protocol progress");
                finished_tx
                    .send(write_result)
                    .expect("test observer remains open");
            },
        ));
        controller.join().expect("test controller exits");
        release_tx
            .send(())
            .expect("tracked credentialed worker remains releasable");
        let write_error = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("tracked credentialed worker eventually observes cancellation")
            .expect_err("post-cancel protocol I/O must fail closed");

        assert_eq!(
            result.expect_err("Disconnect must cancel the stage").code(),
            "rdp_cancelled"
        );
        assert_eq!(write_error.kind(), std::io::ErrorKind::ConnectionAborted);
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }

    struct CountingWriter {
        writes: Arc<AtomicUsize>,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn lifecycle_cancelled_dns_worker_is_tracked_single_flight() {
        let _serial = worker_test_guard();
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events, _) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RejectingFrames),
            None,
            Box::new(NoopWake),
        );
        let starts = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let controller = thread::spawn(move || {
            started_rx.recv().expect("blocking lookup must start");
            commands
                .send(SessionCommand::Disconnect)
                .expect("runtime command receiver remains open");
        });
        let first_starts = starts.clone();
        let lookup = async move {
            tokio::task::spawn_blocking(move || {
                first_starts.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).expect("test controller remains open");
                release_rx.recv().expect("test release remains open");
                finished_tx.send(()).expect("test observer remains open");
            })
            .await
            .map_err(|_| ())
        };
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let first = executor.block_on(wait_for_network_future(
            &mut runtime,
            lookup,
            RDP_DNS_FAILED,
        ));
        controller.join().expect("test controller exits");
        let mut repeated = Vec::new();
        for _ in 0..3 {
            let repeated_starts = starts.clone();
            repeated.push(executor.block_on(wait_for_network_future(
                &mut runtime,
                async move {
                    repeated_starts.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(())
                },
                RDP_DNS_FAILED,
            )));
        }
        release_tx
            .send(())
            .expect("tracked lookup remains releasable");
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("tracked lookup eventually finishes");

        assert_eq!(
            first.expect_err("Disconnect must cancel DNS").code(),
            "rdp_cancelled"
        );
        assert!(repeated.into_iter().all(|attempt| {
            attempt
                .expect_err("a tracked in-flight lookup must reject every repeated start")
                .code()
                == "rdp_dns_failed"
        }));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn lifecycle_in_flight_dns_cancellation_does_not_wait_for_lookup_cleanup() {
        let _serial = worker_test_guard();
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events, _) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RejectingFrames),
            None,
            Box::new(NoopWake),
        );
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let controller = thread::spawn(move || {
            started_rx
                .recv()
                .expect("blocking lookup must enter its in-flight state");
            commands
                .send(SessionCommand::Disconnect)
                .expect("runtime command receiver remains open");
            thread::sleep(Duration::from_millis(600));
            release_tx
                .send(())
                .expect("blocking lookup remains alive until released");
        });
        let lookup = async move {
            tokio::task::spawn_blocking(move || {
                started_tx.send(()).expect("test controller remains open");
                release_rx.recv().expect("test release remains open");
                finished_tx.send(()).expect("test observer remains open");
            })
            .await
            .map_err(|_| ())
        };
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let started_at = Instant::now();
        let result = executor.block_on(wait_for_network_future(
            &mut runtime,
            lookup,
            RDP_DNS_FAILED,
        ));
        drop(executor);
        let return_bound = started_at.elapsed();
        controller.join().expect("test controller exits");
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("detached lookup cleanup eventually completes");

        let error = result.expect_err("Disconnect must cancel in-flight DNS");
        assert_eq!(error.code(), "rdp_cancelled");
        assert!(
            return_bound < Duration::from_millis(300),
            "cancellation waited for blocking DNS cleanup: {return_bound:?}"
        );
    }

    #[test]
    fn lifecycle_in_flight_blocking_stage_cancellation_does_not_wait_for_cleanup() {
        let _serial = worker_test_guard();
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events, _) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RejectingFrames),
            None,
            Box::new(NoopWake),
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
        let client = TcpStream::connect(listener.local_addr().expect("test listener address"))
            .expect("connect test stream");
        let (_server, _) = listener.accept().expect("accept test stream");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let controller = thread::spawn(move || {
            started_rx
                .recv()
                .expect("blocking stage must enter its in-flight state");
            commands
                .send(SessionCommand::Disconnect)
                .expect("runtime command receiver remains open");
            thread::sleep(Duration::from_millis(600));
            release_tx
                .send(())
                .expect("blocking stage remains alive until released");
        });
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let started_at = Instant::now();
        let result = executor.block_on(wait_for_blocking(
            &mut runtime,
            client,
            RDP_TLS_FAILED,
            move |_| {
                started_tx.send(()).expect("test controller remains open");
                release_rx.recv().expect("test release remains open");
                finished_tx.send(()).expect("test observer remains open");
            },
        ));
        drop(executor);
        let return_bound = started_at.elapsed();
        controller.join().expect("test controller exits");
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("detached blocking-stage cleanup eventually completes");

        let error = result.expect_err("Disconnect must cancel the in-flight blocking stage");
        assert_eq!(error.code(), "rdp_cancelled");
        assert!(
            return_bound < Duration::from_millis(300),
            "cancellation waited for blocking-stage cleanup: {return_bound:?}"
        );
    }

    #[test]
    fn lifecycle_in_flight_blocking_stage_timeout_does_not_wait_for_cleanup() {
        let _serial = worker_test_guard();
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let (events, _) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RejectingFrames),
            None,
            Box::new(NoopWake),
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
        let client = TcpStream::connect(listener.local_addr().expect("test listener address"))
            .expect("connect test stream");
        let (_server, _) = listener.accept().expect("accept test stream");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let controller = thread::spawn(move || {
            started_rx
                .recv()
                .expect("blocking stage must enter its in-flight state");
            thread::sleep(Duration::from_millis(600));
            release_tx
                .send(())
                .expect("blocking stage remains alive until released");
        });
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let started_at = Instant::now();
        let result = executor.block_on(wait_for_blocking_with_timeout(
            &mut runtime,
            client,
            RDP_TLS_FAILED,
            Duration::from_millis(50),
            move |_| {
                started_tx.send(()).expect("test controller remains open");
                release_rx.recv().expect("test release remains open");
                finished_tx.send(()).expect("test observer remains open");
            },
        ));
        drop(executor);
        let return_bound = started_at.elapsed();
        controller.join().expect("test controller exits");
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("detached blocking-stage cleanup eventually completes");

        let error = result.expect_err("stage deadline must fail the in-flight blocking stage");
        assert_eq!(error.code(), "rdp_tls_failed");
        assert!(
            return_bound < Duration::from_millis(300),
            "timeout waited for blocking-stage cleanup: {return_bound:?}"
        );
    }

    #[test]
    fn lifecycle_disconnect_before_network_returns_closed() {
        let session_id = SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        commands
            .send(SessionCommand::Disconnect)
            .expect("runtime command receiver remains open");
        let (events, event_rx) = mpsc::channel();
        let runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RejectingFrames),
            None,
            Box::new(NoopWake),
        );
        let config = RdpConnectionConfig::try_from(ConnectRequest {
            session_id,
            endpoint: Endpoint::new("no-network.invalid", 3389).expect("valid endpoint"),
            protocol_id: ProtocolId::rdp(),
            credentials: Some(Credentials {
                username: "alice".to_owned(),
                password: SecretBuffer::new(vec![0x01]).take(),
            }),
            saved_server_pin: None,
        })
        .expect("valid RDP config");

        assert_eq!(run_protocol_session(config, runtime), ProtocolExit::Closed);
        assert_eq!(
            event_rx.try_iter().collect::<Vec<_>>(),
            vec![SessionEvent::StageChanged(ConnectionStage::Connecting)]
        );
    }

    struct RecordingEvents(mpsc::Sender<SessionEvent>);

    impl RuntimeEventSink for RecordingEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            self.0
                .send(event)
                .map_err(|_| ProtocolError::EventPortClosed)
        }
    }

    struct RejectingFrames;

    impl SurfacePublisher for RejectingFrames {
        fn publish(&self, _: frd_frame::SurfaceUpdate) -> Result<(), ProtocolError> {
            Err(ProtocolError::FramePortRejected)
        }
    }

    struct AcceptingFrames;

    impl SurfacePublisher for AcceptingFrames {
        fn publish(&self, _: frd_frame::SurfaceUpdate) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct NoopWake;

    impl RuntimeWake for NoopWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }
}
