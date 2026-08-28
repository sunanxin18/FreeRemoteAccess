use std::future::Future;
use std::net::{Shutdown, TcpStream};
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

pub(crate) async fn wait_for_network_future<T, E>(
    runtime: &mut ProtocolRuntime,
    future: impl Future<Output = Result<T, E>>,
    failure_code: &'static str,
) -> Result<T, ProtocolError> {
    let mut future = Box::pin(future);
    let deadline = Instant::now() + NETWORK_STAGE_TIMEOUT;

    loop {
        if disconnect_requested(runtime) {
            return Err(cancelled());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(rdp_error(failure_code));
        }
        let wait = COMMAND_POLL_INTERVAL.min(deadline.duration_since(now));
        match tokio::time::timeout(wait, future.as_mut()).await {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(_)) => return Err(rdp_error(failure_code)),
            Err(_) => {}
        }
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
    if disconnect_requested(runtime) {
        let _ = shutdown.shutdown(Shutdown::Both);
        return Err(cancelled());
    }

    let mut task = tokio::task::spawn_blocking(operation);
    let deadline = Instant::now() + NETWORK_STAGE_TIMEOUT;
    loop {
        let now = Instant::now();
        if now >= deadline {
            let _ = shutdown.shutdown(Shutdown::Both);
            let _ = task.await;
            return Err(rdp_error(failure_code));
        }
        let wait = COMMAND_POLL_INTERVAL.min(deadline.duration_since(now));
        match tokio::time::timeout(wait, &mut task).await {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(_)) => return Err(rdp_error(failure_code)),
            Err(_) if disconnect_requested(runtime) => {
                let _ = shutdown.shutdown(Shutdown::Both);
                let _ = task.await;
                return Err(cancelled());
            }
            Err(_) => {}
        }
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
    use std::sync::mpsc;

    use frd_core::{SecretBuffer, SessionId};
    use frd_protocol_api::{
        ConnectRequest, ConnectionStage, Credentials, Endpoint, ProtocolError, ProtocolExit,
        ProtocolId, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCommand, SessionEvent,
        SurfacePublisher,
    };

    use crate::config::RdpConnectionConfig;

    use super::run_protocol_session;

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
