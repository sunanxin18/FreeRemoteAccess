use std::future::Future;
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use frd_protocol_api::{ProtocolError, ProtocolExit, ProtocolRuntime, SessionCommand};

use crate::config::RdpConnectionConfig;
use crate::connector::{connect_and_activate, ActivatedRdpSession};
use crate::error::{rdp_error, RDP_ACTIVATION_FAILED, RDP_CANCELLED};

const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const NETWORK_STAGE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn run_protocol_session(
    mut config: RdpConnectionConfig,
    mut runtime: ProtocolRuntime,
) -> ProtocolExit {
    let executor = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(executor) => executor,
        Err(_) => return ProtocolExit::Failed(rdp_error(RDP_ACTIVATION_FAILED)),
    };

    match executor.block_on(connect_and_activate(&mut config, &mut runtime)) {
        Ok(session) => executor.block_on(wait_for_disconnect(session, &mut runtime)),
        Err(error) if error.code() == RDP_CANCELLED => ProtocolExit::Closed,
        Err(error) => ProtocolExit::Failed(error),
    }
}

async fn wait_for_disconnect(
    session: ActivatedRdpSession,
    runtime: &mut ProtocolRuntime,
) -> ProtocolExit {
    loop {
        if disconnect_requested(runtime) {
            session.transport.shutdown();
            return ProtocolExit::Closed;
        }
        if runtime.requires_shutdown() {
            session.transport.shutdown();
            return ProtocolExit::Failed(ProtocolError::Terminal);
        }
        tokio::time::sleep(COMMAND_POLL_INTERVAL).await;
    }
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

    let (result_tx, result_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("frd-rdp-network-stage".to_owned())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|executor| executor.block_on(future));
            let _ = result_tx.send(result);
        })
        .map_err(|_| rdp_error(failure_code))?;
    let deadline = Instant::now() + NETWORK_STAGE_TIMEOUT;

    loop {
        if disconnect_requested(runtime) {
            return Err(cancelled());
        }
        match result_rx.try_recv() {
            Ok(Ok(Ok(value))) => return Ok(value),
            Ok(Ok(Err(_))) | Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                return Err(rdp_error(failure_code));
            }
            Err(TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        if now >= deadline {
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
    operation: impl FnOnce() -> T + Send + 'static,
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
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ProtocolError>
where
    T: Send + 'static,
{
    if disconnect_requested(runtime) {
        let _ = shutdown.shutdown(Shutdown::Both);
        return Err(cancelled());
    }

    let (result_tx, result_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("frd-rdp-blocking-stage".to_owned())
        .spawn(move || {
            let _ = result_tx.send(operation());
        })
        .map_err(|_| rdp_error(failure_code))?;
    let deadline = Instant::now() + timeout;
    loop {
        if disconnect_requested(runtime) {
            let _ = shutdown.shutdown(Shutdown::Both);
            return Err(cancelled());
        }
        match result_rx.try_recv() {
            Ok(value) => return Ok(value),
            Err(TryRecvError::Disconnected) => return Err(rdp_error(failure_code)),
            Err(TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = shutdown.shutdown(Shutdown::Both);
            return Err(rdp_error(failure_code));
        }
        let wait = COMMAND_POLL_INTERVAL.min(deadline.duration_since(now));
        tokio::time::sleep(wait).await;
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
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use frd_core::{SecretBuffer, SessionId};
    use frd_protocol_api::{
        ConnectRequest, ConnectionStage, Credentials, Endpoint, ProtocolError, ProtocolExit,
        ProtocolId, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCommand, SessionEvent,
        SurfacePublisher,
    };

    use crate::config::RdpConnectionConfig;
    use crate::error::{RDP_DNS_FAILED, RDP_TLS_FAILED};

    use super::{
        run_protocol_session, wait_for_blocking, wait_for_blocking_with_timeout,
        wait_for_network_future,
    };

    #[test]
    fn lifecycle_in_flight_dns_cancellation_does_not_wait_for_lookup_cleanup() {
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
            move || {
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
            move || {
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

    struct NoopWake;

    impl RuntimeWake for NoopWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }
}
