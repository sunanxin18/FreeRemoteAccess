use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use crate::app::connection::ProtocolKind;
use crate::protocols::ProtocolAdapter;
use crate::session::{ProtocolContext, SessionCommand, SessionError, SessionEventSink};

const STOCK_RDP_PORT: u16 = 3389;
const STOCK_RFB_PORT: u16 = 5900;
const RFB_BANNER_BYTES: usize = 12;
const MIN_RDP_RESPONSE_BYTES: usize = 19;
const PRODUCTION_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(750);
const PRODUCTION_TOTAL_TIMEOUT: Duration = Duration::from_millis(1_750);
const PRODUCTION_MAX_RESPONSE_BYTES: usize = 64;

// TPKT + X.224 Connection Request + RDP Negotiation Request. The advertised
// protocols are TLS, CredSSP-capable Hybrid, and Hybrid Extended. This is only
// capability negotiation: the detector sends no CredSSP token or credentials.
const RDP_NEGOTIATION_REQUEST: [u8; 19] = [
    0x03, 0x00, 0x00, 0x13, 0x0e, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x08, 0x00, 0x0b,
    0x00, 0x00, 0x00,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutoProbeLimits {
    attempt_timeout: Duration,
    total_timeout: Duration,
    max_response_bytes: usize,
}

impl AutoProbeLimits {
    fn new(
        attempt_timeout: Duration,
        total_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, SessionError> {
        if attempt_timeout.is_zero()
            || total_timeout.is_zero()
            || attempt_timeout > total_timeout
            || max_response_bytes < MIN_RDP_RESPONSE_BYTES
            || max_response_bytes > 1024
        {
            return Err(SessionError::new("auto_probe_limits_invalid"));
        }
        Ok(Self {
            attempt_timeout,
            total_timeout,
            max_response_bytes,
        })
    }

    fn production() -> Self {
        Self::new(
            PRODUCTION_ATTEMPT_TIMEOUT,
            PRODUCTION_TOTAL_TIMEOUT,
            PRODUCTION_MAX_RESPONSE_BYTES,
        )
        .expect("production auto-probe limits are valid")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProbePlan {
    rdp_port: u16,
    rfb_port: u16,
}

impl ProbePlan {
    const fn from_explicit_port(port: Option<u16>) -> Self {
        match port {
            Some(port) => Self {
                rdp_port: port,
                rfb_port: port,
            },
            None => Self {
                rdp_port: STOCK_RDP_PORT,
                rfb_port: STOCK_RFB_PORT,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectedProtocolFamily {
    Rdp,
    Rfb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DetectedEndpoint {
    family: DetectedProtocolFamily,
    port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RfbBanner {
    minor: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RdpNegotiationEvidence {
    Selected(u32),
    Failure(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeOutcome {
    Positive,
    Timeout,
    Malformed,
    Unreachable,
}

pub struct AutoAdapter;

impl AutoAdapter {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for AutoAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolAdapter for AutoAdapter {
    fn run(
        self: Box<Self>,
        context: ProtocolContext,
        commands: Receiver<SessionCommand>,
        events: SessionEventSink,
    ) -> Result<(), SessionError> {
        let connection = context.into_connection();
        if connection.protocol != ProtocolKind::Auto {
            return Err(SessionError::new("auto_protocol_mismatch"));
        }
        let selected = detect_host(
            connection.endpoint.host(),
            connection.endpoint.explicit_port(),
            AutoProbeLimits::production(),
        )?;
        let protocol = match selected.family {
            DetectedProtocolFamily::Rdp => ProtocolKind::Rdp,
            // RFB evidence identifies only the protocol family. Auto applies the
            // product preference for the Mac native username/password path;
            // standard VNC remains the explicit Linux/VNC entry.
            DetectedProtocolFamily::Rfb => ProtocolKind::AppleRfb,
        };
        let connection = connection
            .select_auto_protocol(protocol, selected.port)
            .ok_or_else(|| SessionError::new("auto_protocol_selection_invalid"))?;
        let adapter = super::adapter_for(protocol)?;
        adapter.run(ProtocolContext::new(connection), commands, events)
    }
}

fn detect_host(
    host: &str,
    explicit_port: Option<u16>,
    limits: AutoProbeLimits,
) -> Result<DetectedEndpoint, SessionError> {
    let total_deadline = Instant::now() + limits.total_timeout;
    let ips = resolve_host_bounded(host, total_deadline)?;
    detect_on_ips_until(
        &ips,
        ProbePlan::from_explicit_port(explicit_port),
        limits,
        total_deadline,
    )
}

fn resolve_host_bounded(host: &str, deadline: Instant) -> Result<Vec<IpAddr>, SessionError> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let host = host.to_owned();
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("freeremote-auto-resolve".to_owned())
        .spawn(move || {
            let result = (host.as_str(), 0).to_socket_addrs().map(|addresses| {
                addresses
                    .map(|address| address.ip())
                    .collect::<BTreeSet<_>>()
            });
            let _ = sender.send(result);
        })
        .map_err(|_| SessionError::new("auto_probe_resolution_failed"))?;
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| SessionError::new("auto_probe_timeout"))?;
    let resolved = receiver
        .recv_timeout(remaining)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => SessionError::new("auto_probe_timeout"),
            mpsc::RecvTimeoutError::Disconnected => {
                SessionError::new("auto_probe_resolution_failed")
            }
        })?
        .map_err(|_| SessionError::new("auto_probe_resolution_failed"))?;
    if resolved.is_empty() {
        return Err(SessionError::new("auto_probe_resolution_failed"));
    }
    Ok(resolved.into_iter().collect())
}

#[cfg(test)]
fn detect_on_ips(
    ips: &[IpAddr],
    plan: ProbePlan,
    limits: AutoProbeLimits,
) -> Result<DetectedEndpoint, SessionError> {
    let total_deadline = Instant::now() + limits.total_timeout;
    detect_on_ips_until(ips, plan, limits, total_deadline)
}

fn detect_on_ips_until(
    ips: &[IpAddr],
    plan: ProbePlan,
    limits: AutoProbeLimits,
    total_deadline: Instant,
) -> Result<DetectedEndpoint, SessionError> {
    if ips.is_empty() {
        return Err(SessionError::new("auto_protocol_not_detected"));
    }
    let rdp = probe_rdp(ips, plan.rdp_port, limits, total_deadline);
    let rfb = probe_rfb(ips, plan.rfb_port, limits, total_deadline);
    match (rdp, rfb) {
        (ProbeOutcome::Positive, ProbeOutcome::Positive) => {
            Err(SessionError::new("auto_protocol_ambiguous"))
        }
        (ProbeOutcome::Positive, _) => Ok(DetectedEndpoint {
            family: DetectedProtocolFamily::Rdp,
            port: plan.rdp_port,
        }),
        (_, ProbeOutcome::Positive) => Ok(DetectedEndpoint {
            family: DetectedProtocolFamily::Rfb,
            port: plan.rfb_port,
        }),
        (ProbeOutcome::Malformed, _) | (_, ProbeOutcome::Malformed) => {
            Err(SessionError::new("auto_probe_malformed"))
        }
        (ProbeOutcome::Timeout, _) | (_, ProbeOutcome::Timeout) => {
            Err(SessionError::new("auto_probe_timeout"))
        }
        _ => Err(SessionError::new("auto_protocol_not_detected")),
    }
}

fn probe_rdp(
    ips: &[IpAddr],
    port: u16,
    limits: AutoProbeLimits,
    total_deadline: Instant,
) -> ProbeOutcome {
    let attempt_deadline = bounded_attempt_deadline(limits.attempt_timeout, total_deadline);
    let mut stream = match connect_bounded(ips, port, attempt_deadline) {
        Ok(stream) => stream,
        Err(outcome) => return outcome,
    };
    if set_io_timeout(&stream, attempt_deadline).is_err() {
        return ProbeOutcome::Timeout;
    }
    if let Err(error) = stream.write_all(&RDP_NEGOTIATION_REQUEST) {
        return classify_io_error(&error);
    }
    let mut header = [0u8; 4];
    if let Err(error) = stream.read_exact(&mut header) {
        return classify_read_error(&error);
    }
    if header[0] != 3 || header[1] != 0 {
        return ProbeOutcome::Malformed;
    }
    let declared = usize::from(u16::from_be_bytes([header[2], header[3]]));
    if declared < MIN_RDP_RESPONSE_BYTES || declared > limits.max_response_bytes {
        return ProbeOutcome::Malformed;
    }
    let mut response = vec![0u8; declared];
    response[..4].copy_from_slice(&header);
    if let Err(error) = stream.read_exact(&mut response[4..]) {
        return classify_read_error(&error);
    }
    match parse_rdp_negotiation_response(&response, limits.max_response_bytes) {
        Ok(_) => ProbeOutcome::Positive,
        Err(_) => ProbeOutcome::Malformed,
    }
}

fn probe_rfb(
    ips: &[IpAddr],
    port: u16,
    limits: AutoProbeLimits,
    total_deadline: Instant,
) -> ProbeOutcome {
    let attempt_deadline = bounded_attempt_deadline(limits.attempt_timeout, total_deadline);
    let mut stream = match connect_bounded(ips, port, attempt_deadline) {
        Ok(stream) => stream,
        Err(outcome) => return outcome,
    };
    if set_io_timeout(&stream, attempt_deadline).is_err() {
        return ProbeOutcome::Timeout;
    }
    let mut banner = [0u8; RFB_BANNER_BYTES];
    if let Err(error) = stream.read_exact(&mut banner) {
        return classify_read_error(&error);
    }
    match parse_rfb_banner(&banner) {
        Ok(_) => ProbeOutcome::Positive,
        Err(_) => ProbeOutcome::Malformed,
    }
}

fn bounded_attempt_deadline(attempt_timeout: Duration, total_deadline: Instant) -> Instant {
    let attempt_deadline = Instant::now() + attempt_timeout;
    attempt_deadline.min(total_deadline)
}

fn connect_bounded(
    ips: &[IpAddr],
    port: u16,
    deadline: Instant,
) -> Result<TcpStream, ProbeOutcome> {
    for ip in ips {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(ProbeOutcome::Timeout);
        };
        match TcpStream::connect_timeout(&SocketAddr::new(*ip, port), remaining) {
            Ok(stream) => return Ok(stream),
            Err(_) => {}
        }
    }
    // Connect errno/timing differs across desktop platforms and firewalls.
    // Classify every bounded connection failure uniformly as unreachable;
    // `Timeout` is reserved for a peer that accepted but did not complete its
    // pre-authentication handshake within the fixed deadline.
    Err(ProbeOutcome::Unreachable)
}

fn set_io_timeout(stream: &TcpStream, deadline: Instant) -> io::Result<()> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "probe deadline elapsed"))?;
    stream.set_read_timeout(Some(remaining))?;
    stream.set_write_timeout(Some(remaining))
}

fn classify_read_error(error: &io::Error) -> ProbeOutcome {
    if is_timeout(error) {
        ProbeOutcome::Timeout
    } else if error.kind() == io::ErrorKind::UnexpectedEof {
        ProbeOutcome::Malformed
    } else {
        ProbeOutcome::Unreachable
    }
}

fn classify_io_error(error: &io::Error) -> ProbeOutcome {
    if is_timeout(error) {
        ProbeOutcome::Timeout
    } else {
        ProbeOutcome::Unreachable
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

fn parse_rfb_banner(bytes: &[u8]) -> Result<RfbBanner, SessionError> {
    if bytes.len() != RFB_BANNER_BYTES
        || &bytes[..8] != b"RFB 003."
        || bytes[11] != b'\n'
        || !bytes[8..11].iter().all(u8::is_ascii_digit)
    {
        return Err(SessionError::new("auto_probe_malformed"));
    }
    let minor = u16::from(bytes[8] - b'0') * 100
        + u16::from(bytes[9] - b'0') * 10
        + u16::from(bytes[10] - b'0');
    Ok(RfbBanner { minor })
}

fn parse_rdp_negotiation_response(
    bytes: &[u8],
    max_response_bytes: usize,
) -> Result<RdpNegotiationEvidence, SessionError> {
    if bytes.len() < MIN_RDP_RESPONSE_BYTES
        || bytes.len() > max_response_bytes
        || bytes[0] != 3
        || bytes[1] != 0
        || usize::from(u16::from_be_bytes([bytes[2], bytes[3]])) != bytes.len()
        || usize::from(bytes[4]) + 5 != bytes.len()
        || bytes[5] != 0xd0
        || bytes[10] != 0
        || bytes.len() != MIN_RDP_RESPONSE_BYTES
        || u16::from_le_bytes([bytes[13], bytes[14]]) != 8
    {
        return Err(SessionError::new("auto_probe_malformed"));
    }
    let value = u32::from_le_bytes([bytes[15], bytes[16], bytes[17], bytes[18]]);
    match bytes[11] {
        0x02 if bytes[12] & !0x1b == 0 && matches!(value, 0 | 1 | 2 | 8) => {
            Ok(RdpNegotiationEvidence::Selected(value))
        }
        0x03 if bytes[12] == 0 && (1..=6).contains(&value) => {
            Ok(RdpNegotiationEvidence::Failure(value))
        }
        _ => Err(SessionError::new("auto_probe_malformed")),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    use super::*;

    const CREDENTIAL_CANARY: &[u8] = b"canary";
    const RDP_NEGOTIATION_RESPONSE: [u8; 19] = [
        0x03, 0x00, 0x00, 0x13, 0x0e, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x08, 0x00,
        0x02, 0x00, 0x00, 0x00,
    ];
    const RDP_NEGOTIATION_FAILURE: [u8; 19] = [
        0x03, 0x00, 0x00, 0x13, 0x0e, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x08, 0x00,
        0x05, 0x00, 0x00, 0x00,
    ];

    fn limits() -> AutoProbeLimits {
        AutoProbeLimits::new(Duration::from_millis(150), Duration::from_millis(350), 64).unwrap()
    }

    fn spawn_server(
        connections: usize,
        handler: impl Fn(TcpStream, usize) + Send + Sync + 'static,
    ) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = std::sync::Arc::new(handler);
        let worker = thread::spawn(move || {
            for index in 0..connections {
                let (stream, _) = listener.accept().unwrap();
                handler(stream, index);
            }
        });
        (port, worker)
    }

    fn assert_fixed_rdp_probe_without_credentials(mut stream: TcpStream) {
        let mut request = [0u8; 19];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(request, RDP_NEGOTIATION_REQUEST);
        assert!(!request
            .windows(CREDENTIAL_CANARY.len())
            .any(|window| window == CREDENTIAL_CANARY));
    }

    #[test]
    fn literal_rfb_banner_parser_rejects_partial_overlong_and_non_literal_inputs() {
        assert_eq!(
            parse_rfb_banner(b"RFB 003.008\n").unwrap(),
            RfbBanner { minor: 8 }
        );
        for malformed in [
            b"RFB 003.008".as_slice(),
            b"RFB 003.008\nX".as_slice(),
            b"rfb 003.008\n".as_slice(),
            b"RFB 004.008\n".as_slice(),
            b"RFB 003.0x8\n".as_slice(),
        ] {
            assert_eq!(
                parse_rfb_banner(malformed).unwrap_err().code(),
                "auto_probe_malformed"
            );
        }
    }

    #[test]
    fn x224_parser_accepts_only_bounded_negotiation_response_or_failure_pdus() {
        assert_eq!(
            parse_rdp_negotiation_response(&RDP_NEGOTIATION_RESPONSE, 64).unwrap(),
            RdpNegotiationEvidence::Selected(2)
        );
        assert_eq!(
            parse_rdp_negotiation_response(&RDP_NEGOTIATION_FAILURE, 64).unwrap(),
            RdpNegotiationEvidence::Failure(5)
        );

        for malformed in [
            b"RFB 003.008\n".as_slice(),
            &RDP_NEGOTIATION_RESPONSE[..18],
            &[
                0x03, 0x00, 0x01, 0x00, 0x0e, 0xd0, 0, 0, 0, 0, 0, 0x02, 0, 8, 0, 2, 0, 0, 0,
            ],
        ] {
            assert_eq!(
                parse_rdp_negotiation_response(malformed, 64)
                    .unwrap_err()
                    .code(),
                "auto_probe_malformed"
            );
        }

        let mut response_with_unknown_flags = RDP_NEGOTIATION_RESPONSE;
        response_with_unknown_flags[12] = 0x80;
        assert_eq!(
            parse_rdp_negotiation_response(&response_with_unknown_flags, 64)
                .unwrap_err()
                .code(),
            "auto_probe_malformed"
        );
        let mut failure_with_flags = RDP_NEGOTIATION_FAILURE;
        failure_with_flags[12] = 1;
        assert_eq!(
            parse_rdp_negotiation_response(&failure_with_flags, 64)
                .unwrap_err()
                .code(),
            "auto_probe_malformed"
        );
    }

    #[test]
    fn no_port_plan_uses_only_stock_rdp_and_rfb_ports() {
        let plan = ProbePlan::from_explicit_port(None);
        assert_eq!(plan.rdp_port, 3389);
        assert_eq!(plan.rfb_port, 5900);
    }

    #[test]
    fn exactly_one_rfb_banner_selects_rfb_without_sending_credentials() {
        let (rdp_port, rdp_worker) = spawn_server(1, |mut stream, _| {
            assert_fixed_rdp_probe_without_credentials(stream.try_clone().unwrap());
            stream.write_all(b"not-rdp").unwrap();
        });
        let (rfb_port, rfb_worker) = spawn_server(1, |mut stream, _| {
            stream.write_all(b"RFB 003.008\n").unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let mut bytes = [0u8; 64];
            assert_eq!(stream.read(&mut bytes).unwrap_or(0), 0);
        });

        let selected = detect_on_ips(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan { rdp_port, rfb_port },
            limits(),
        )
        .unwrap();

        assert_eq!(selected.family, DetectedProtocolFamily::Rfb);
        assert_eq!(selected.port, rfb_port);
        rdp_worker.join().unwrap();
        rfb_worker.join().unwrap();
    }

    #[test]
    fn valid_rdp_negotiation_failure_still_identifies_rdp_without_credssp() {
        let (rdp_port, rdp_worker) = spawn_server(1, |mut stream, _| {
            assert_fixed_rdp_probe_without_credentials(stream.try_clone().unwrap());
            stream.write_all(&RDP_NEGOTIATION_FAILURE).unwrap();
        });
        let (rfb_port, rfb_worker) = spawn_server(1, |mut stream, _| {
            stream.write_all(b"not an rfb!").unwrap();
        });

        let selected = detect_on_ips(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan { rdp_port, rfb_port },
            limits(),
        )
        .unwrap();

        assert_eq!(selected.family, DetectedProtocolFamily::Rdp);
        assert_eq!(selected.port, rdp_port);
        rdp_worker.join().unwrap();
        rfb_worker.join().unwrap();
    }

    #[test]
    fn two_positive_protocol_families_fail_closed_as_ambiguous() {
        let (rdp_port, rdp_worker) = spawn_server(1, |mut stream, _| {
            assert_fixed_rdp_probe_without_credentials(stream.try_clone().unwrap());
            stream.write_all(&RDP_NEGOTIATION_RESPONSE).unwrap();
        });
        let (rfb_port, rfb_worker) = spawn_server(1, |mut stream, _| {
            stream.write_all(b"RFB 003.008\n").unwrap();
        });

        let error = detect_on_ips(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan { rdp_port, rfb_port },
            limits(),
        )
        .unwrap_err();

        assert_eq!(error.code(), "auto_protocol_ambiguous");
        rdp_worker.join().unwrap();
        rfb_worker.join().unwrap();
    }

    #[test]
    fn malformed_pre_authentication_evidence_has_a_stable_fail_closed_code() {
        let (rdp_port, rdp_worker) = spawn_server(1, |mut stream, _| {
            assert_fixed_rdp_probe_without_credentials(stream.try_clone().unwrap());
            stream.write_all(b"not-rdp").unwrap();
        });
        let (rfb_port, rfb_worker) = spawn_server(1, |mut stream, _| {
            stream.write_all(b"NOT 003.008\n").unwrap();
        });

        let error = detect_on_ips(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan { rdp_port, rfb_port },
            limits(),
        )
        .unwrap_err();

        assert_eq!(error.code(), "auto_probe_malformed");
        rdp_worker.join().unwrap();
        rfb_worker.join().unwrap();
    }

    #[test]
    fn unreachable_and_timeout_targets_have_distinct_stable_fail_closed_codes() {
        let rdp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let rdp_port = rdp.local_addr().unwrap().port();
        let rfb = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let rfb_port = rfb.local_addr().unwrap().port();
        drop((rdp, rfb));
        assert_eq!(
            detect_on_ips(
                &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
                ProbePlan { rdp_port, rfb_port },
                limits(),
            )
            .unwrap_err()
            .code(),
            "auto_protocol_not_detected"
        );

        let (rdp_port, rdp_worker) = spawn_server(1, |_, _| {
            thread::sleep(Duration::from_millis(250));
        });
        let (rfb_port, rfb_worker) = spawn_server(1, |_, _| {
            thread::sleep(Duration::from_millis(250));
        });
        let timeout_limits =
            AutoProbeLimits::new(Duration::from_millis(50), Duration::from_millis(90), 64).unwrap();
        assert_eq!(
            detect_on_ips(
                &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
                ProbePlan { rdp_port, rfb_port },
                timeout_limits,
            )
            .unwrap_err()
            .code(),
            "auto_probe_timeout"
        );
        rdp_worker.join().unwrap();
        rfb_worker.join().unwrap();
    }

    #[test]
    fn explicit_auto_port_is_probed_for_both_families_without_port_inference() {
        let (port, worker) = spawn_server(2, |mut stream, index| {
            if index == 0 {
                assert_fixed_rdp_probe_without_credentials(stream.try_clone().unwrap());
            }
            stream.write_all(b"RFB 003.008\n").unwrap();
        });

        let selected = detect_on_ips(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan::from_explicit_port(Some(port)),
            limits(),
        )
        .unwrap();

        assert_eq!(selected.family, DetectedProtocolFamily::Rfb);
        assert_eq!(selected.port, port);
        worker.join().unwrap();
    }
}
