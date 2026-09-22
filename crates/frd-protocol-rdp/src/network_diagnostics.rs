//! 网络失败仅记录固定分类与系统错误数字，绝不格式化底层错误或连接身份。
use std::io;

use frd_protocol_api::ProtocolError;

use crate::error::{rdp_error, RDP_TCP_FAILED};
use crate::runtime::NetworkFailure;

#[derive(Clone, Copy, Debug)]
pub(crate) enum NetworkStage {
    Resolve,
    Preflight,
    Authentication,
    Credssp,
    Activation,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum NetworkOperation {
    Lookup,
    Connect,
    IntoStd,
    SetBlocking,
    LocalAddress,
    CloneSocket,
}

impl NetworkStage {
    pub(crate) fn report(self, operation: NetworkOperation, failure: NetworkFailure<io::Error>) {
        // stderr 的写入失败不得使连接错误处理本身 panic。
        use std::io::Write;
        let _ = writeln!(
            io::stderr().lock(),
            "{}",
            diagnostic_line(self, operation, failure)
        );
    }

    pub(crate) fn io_error(self, operation: NetworkOperation, error: io::Error) -> ProtocolError {
        self.report(operation, NetworkFailure::Io(error));
        rdp_error(RDP_TCP_FAILED)
    }
}

fn diagnostic_line(
    stage: NetworkStage,
    operation: NetworkOperation,
    failure: NetworkFailure<io::Error>,
) -> String {
    let (reason, error) = match failure {
        NetworkFailure::Io(error) => ("io", Some(error)),
        NetworkFailure::RuntimeCreation(error) => ("runtime_creation", Some(error)),
        NetworkFailure::WorkerUnavailable => ("worker_unavailable", None),
        NetworkFailure::WorkerDisconnected => ("worker_disconnected", None),
        NetworkFailure::Timeout => ("timeout", None),
    };
    let kind = error.as_ref().map(io::Error::kind);
    let errno = error.as_ref().and_then(io::Error::raw_os_error);
    format!("RDP network failure stage={stage:?} operation={operation:?} reason={reason} io_kind={kind:?} os_error={errno:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_keeps_kind_and_errno_but_never_formats_source_message() {
        let line = diagnostic_line(
            NetworkStage::Preflight,
            NetworkOperation::Connect,
            NetworkFailure::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "secret-host secret-user secret-password",
            )),
        );
        assert_eq!(line, "RDP network failure stage=Preflight operation=Connect reason=io io_kind=Some(PermissionDenied) os_error=None");
        let error = io::Error::from_raw_os_error(13);
        let line = diagnostic_line(
            NetworkStage::Authentication,
            NetworkOperation::Connect,
            NetworkFailure::Io(error),
        );
        assert!(line.contains("os_error=Some(13)"));
        assert!(line.len() < 256);
    }

    #[test]
    fn infrastructure_failures_remain_distinct_from_socket_failure() {
        for (failure, expected) in [
            (NetworkFailure::WorkerUnavailable, "worker_unavailable"),
            (NetworkFailure::WorkerDisconnected, "worker_disconnected"),
            (NetworkFailure::Timeout, "timeout"),
            (
                NetworkFailure::RuntimeCreation(io::Error::new(io::ErrorKind::Other, "sensitive")),
                "runtime_creation",
            ),
        ] {
            let line = diagnostic_line(NetworkStage::Resolve, NetworkOperation::Lookup, failure);
            assert!(line.contains(&format!("reason={expected} ")));
            assert!(!line.contains("sensitive"));
            assert!(line.len() < 256);
        }
    }
}
