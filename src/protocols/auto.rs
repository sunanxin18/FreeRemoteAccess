use std::collections::BTreeSet;
use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use hickory_resolver::lookup::Lookup;
use hickory_resolver::proto::rr::{RData, RecordType};
use hickory_resolver::TokioResolver;

use crate::app::connection::ProtocolKind;
use crate::protocols::rfb_security::{self, SecurityListFraming};
use crate::protocols::ProtocolAdapter;
use crate::session::{ProtocolContext, SessionCommand, SessionError, SessionEventSink};

const STOCK_RDP_PORT: u16 = 3389;
const STOCK_RFB_PORT: u16 = 5900;
const RFB_BANNER_BYTES: usize = 12;
const MIN_RDP_RESPONSE_BYTES: usize = 19;
const PRODUCTION_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(750);
const PRODUCTION_TOTAL_TIMEOUT: Duration = Duration::from_millis(1_750);
const PRODUCTION_MAX_RESPONSE_BYTES: usize = 64;
const MAX_RESOLVED_ADDRESSES: usize = 8;
const MAX_RESOLVER_SCAN_ADDRESSES: usize = 32;
const MAX_RESOLVER_SCAN_PER_FAMILY: usize = MAX_RESOLVER_SCAN_ADDRESSES / 2;
const MAX_CONNECT_ATTEMPTS: usize = 4;
const MAX_RFB_SECURITY_TYPES: usize = 64;

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
struct DetectedEndpoint {
    protocol: ProtocolKind,
    address: SocketAddr,
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
    Positive(DetectedEndpoint),
    CrossProtocolRfbBanner,
    PassiveRfbWait,
    Negative,
    Timeout,
    Malformed,
    Unreachable,
}

struct CancellableResolver {
    runtime: tokio::runtime::Runtime,
    resolver: TokioResolver,
}

impl CancellableResolver {
    fn production() -> Result<Self, SessionError> {
        let runtime = build_resolver_runtime()?;
        let resolver = {
            let _runtime_guard = runtime.enter();
            TokioResolver::builder_tokio()
                .map_err(|_| SessionError::new("auto_probe_resolution_failed"))?
                .build()
                .map_err(|_| SessionError::new("auto_probe_resolution_failed"))?
        };
        Ok(Self { runtime, resolver })
    }

    fn resolve(&self, host: &str, deadline: Instant) -> Result<Vec<IpAddr>, SessionError> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![ip]);
        }
        let host = host.to_owned();
        let resolver = &self.resolver;
        let (ipv4, ipv6) = self.runtime.block_on(resolve_future_until(
            async move {
                Ok::<_, ()>(tokio::join!(
                    resolver.lookup(host.clone(), RecordType::A),
                    resolver.lookup(host, RecordType::AAAA),
                ))
            },
            deadline,
        ))?;
        if ipv4.is_err() && ipv6.is_err() {
            return Err(SessionError::new("auto_probe_resolution_failed"));
        }
        let ipv4 = ipv4
            .as_ref()
            .map(collect_lookup_ipv4_bounded)
            .unwrap_or_default();
        let ipv6 = ipv6
            .as_ref()
            .map(collect_lookup_ipv6_bounded)
            .unwrap_or_default();
        let addresses = merge_resolved_families_bounded(ipv4, ipv6);
        if addresses.is_empty() {
            return Err(SessionError::new("auto_probe_resolution_failed"));
        }
        Ok(addresses)
    }
}

fn build_resolver_runtime() -> Result<tokio::runtime::Runtime, SessionError> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .thread_name("freeremote-auto-dns")
        .build()
        .map_err(|_| SessionError::new("auto_probe_resolution_failed"))
}

async fn resolve_future_until<F, T, E>(future: F, deadline: Instant) -> Result<T, SessionError>
where
    F: Future<Output = Result<T, E>>,
{
    let remaining =
        remaining_until(deadline).map_err(|_| SessionError::new("auto_probe_timeout"))?;
    tokio::time::timeout(remaining, future)
        .await
        .map_err(|_| SessionError::new("auto_probe_timeout"))?
        .map_err(|_| SessionError::new("auto_probe_resolution_failed"))
}

fn global_resolver() -> Result<&'static CancellableResolver, SessionError> {
    static RESOLVER: OnceLock<Result<CancellableResolver, SessionError>> = OnceLock::new();
    match RESOLVER.get_or_init(CancellableResolver::production) {
        Ok(resolver) => Ok(resolver),
        Err(error) => Err(*error),
    }
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
        let connection = context.connection();
        if connection.protocol != ProtocolKind::Auto {
            return Err(SessionError::new("auto_protocol_mismatch"));
        }
        let selected = detect_host(
            connection.endpoint.host(),
            connection.endpoint.explicit_port(),
            AutoProbeLimits::production(),
        )?;
        let adapter = super::adapter_for(selected.protocol)?;
        adapter.run(
            prepare_selected_context(context, selected)?,
            commands,
            events,
        )
    }
}

fn prepare_selected_context(
    context: ProtocolContext,
    selected: DetectedEndpoint,
) -> Result<ProtocolContext, SessionError> {
    let (connection, platform_services) = context.into_parts();
    let connection = connection
        .select_auto_protocol(selected.protocol, selected.address)
        .ok_or_else(|| SessionError::new("auto_protocol_selection_invalid"))?;
    Ok(ProtocolContext::with_platform_services(
        connection,
        platform_services,
    ))
}

fn detect_host(
    host: &str,
    explicit_port: Option<u16>,
    limits: AutoProbeLimits,
) -> Result<DetectedEndpoint, SessionError> {
    let total_deadline = Instant::now() + limits.total_timeout;
    let ips = global_resolver()?.resolve(host, total_deadline)?;
    detect_on_ips_until(
        &ips,
        ProbePlan::from_explicit_port(explicit_port),
        limits,
        total_deadline,
    )
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
    if Instant::now() >= total_deadline {
        return Err(SessionError::new("auto_probe_timeout"));
    }
    let (rdp, rfb) = thread::scope(|scope| -> Result<_, SessionError> {
        let rdp = thread::Builder::new()
            .name("freeremote-auto-rdp".to_owned())
            .spawn_scoped(scope, || {
                probe_rdp(ips, plan.rdp_port, limits, total_deadline)
            })
            .map_err(|_| SessionError::new("auto_probe_worker_spawn_failed"))?;
        let rfb = thread::Builder::new()
            .name("freeremote-auto-rfb".to_owned())
            .spawn_scoped(scope, || {
                probe_rfb(ips, plan.rfb_port, limits, total_deadline)
            })
            .map_err(|_| SessionError::new("auto_probe_worker_spawn_failed"))?;
        Ok((
            rdp.join().unwrap_or(ProbeOutcome::Malformed),
            rfb.join().unwrap_or(ProbeOutcome::Malformed),
        ))
    })?;
    merge_probe_outcomes(rdp, rfb, plan.rdp_port == plan.rfb_port)
}

fn merge_probe_outcomes(
    rdp: ProbeOutcome,
    rfb: ProbeOutcome,
    shared_explicit_port: bool,
) -> Result<DetectedEndpoint, SessionError> {
    if !shared_explicit_port && matches!(rdp, ProbeOutcome::CrossProtocolRfbBanner) {
        return Err(SessionError::new("auto_probe_malformed"));
    }
    match (rdp, rfb) {
        (ProbeOutcome::Positive(_), ProbeOutcome::Positive(_)) => {
            Err(SessionError::new("auto_protocol_ambiguous"))
        }
        (ProbeOutcome::Positive(endpoint), ProbeOutcome::PassiveRfbWait)
            if shared_explicit_port && endpoint.protocol == ProtocolKind::Rdp =>
        {
            Ok(endpoint)
        }
        (ProbeOutcome::CrossProtocolRfbBanner, ProbeOutcome::Positive(endpoint))
            if shared_explicit_port
                && matches!(
                    endpoint.protocol,
                    ProtocolKind::AppleRfb | ProtocolKind::StandardRfb
                ) =>
        {
            Ok(endpoint)
        }
        (ProbeOutcome::Malformed, _) | (_, ProbeOutcome::Malformed) => {
            Err(SessionError::new("auto_probe_malformed"))
        }
        (ProbeOutcome::Timeout, _) | (_, ProbeOutcome::Timeout) => {
            Err(SessionError::new("auto_probe_timeout"))
        }
        (ProbeOutcome::PassiveRfbWait, _) | (_, ProbeOutcome::PassiveRfbWait) => {
            Err(SessionError::new("auto_probe_timeout"))
        }
        (ProbeOutcome::CrossProtocolRfbBanner, _) => Err(SessionError::new("auto_probe_malformed")),
        (ProbeOutcome::Positive(endpoint), ProbeOutcome::Unreachable | ProbeOutcome::Negative)
        | (ProbeOutcome::Unreachable | ProbeOutcome::Negative, ProbeOutcome::Positive(endpoint)) => {
            Ok(endpoint)
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
    let (mut stream, address) = match connect_bounded(ips, port, attempt_deadline) {
        Ok(connected) => connected,
        Err(outcome) => return outcome,
    };
    if let Err(error) = write_all_until(&mut stream, &RDP_NEGOTIATION_REQUEST, attempt_deadline) {
        return classify_io_error(&error);
    }
    let mut header = [0u8; 4];
    if let Err((error, received)) = read_exact_until(&mut stream, &mut header, attempt_deadline) {
        return classify_read_progress_error(&error, received);
    }
    if &header == b"RFB " {
        let mut banner = [0u8; RFB_BANNER_BYTES];
        banner[..header.len()].copy_from_slice(&header);
        return match read_exact_until(&mut stream, &mut banner[header.len()..], attempt_deadline) {
            Ok(()) if parse_rfb_banner(&banner).is_ok() => ProbeOutcome::CrossProtocolRfbBanner,
            Ok(()) => ProbeOutcome::Malformed,
            Err((error, received)) => {
                classify_read_progress_error(&error, received.saturating_add(header.len()))
            }
        };
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
    if let Err((error, received)) =
        read_exact_until(&mut stream, &mut response[4..], attempt_deadline)
    {
        return if received == 0 && error.kind() == io::ErrorKind::UnexpectedEof {
            ProbeOutcome::Malformed
        } else {
            classify_read_progress_error(&error, received.saturating_add(4))
        };
    }
    match parse_rdp_negotiation_response(&response, limits.max_response_bytes) {
        Ok(_) => ProbeOutcome::Positive(DetectedEndpoint {
            protocol: ProtocolKind::Rdp,
            address,
        }),
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
    let (mut stream, address) = match connect_bounded(ips, port, attempt_deadline) {
        Ok(connected) => connected,
        Err(outcome) => return outcome,
    };
    let mut banner = [0u8; RFB_BANNER_BYTES];
    if let Err((error, received)) = read_exact_until(&mut stream, &mut banner, attempt_deadline) {
        return if is_timeout(&error) && received == 0 {
            ProbeOutcome::PassiveRfbWait
        } else if is_timeout(&error) {
            ProbeOutcome::Malformed
        } else {
            classify_read_progress_error(&error, received)
        };
    }
    let parsed = match parse_rfb_banner(&banner) {
        Ok(parsed) => parsed,
        Err(_) => return ProbeOutcome::Malformed,
    };
    let (version_minor, reply) = match parsed.minor {
        minor if minor >= 8 => (8, banner),
        3 => (3, *b"RFB 003.003\n"),
        7 => (7, *b"RFB 003.007\n"),
        _ => (8, *b"RFB 003.008\n"),
    };
    if let Err(error) = write_all_until(&mut stream, &reply, attempt_deadline) {
        return classify_io_error(&error);
    }
    let security_types = match read_rfb_security_types(
        &mut stream,
        version_minor,
        attempt_deadline,
        limits.max_response_bytes,
    ) {
        Ok(types) => types,
        Err(outcome) => return outcome,
    };
    match classify_rfb_security_types(&security_types) {
        Ok(protocol) => ProbeOutcome::Positive(DetectedEndpoint { protocol, address }),
        Err(error) if error.code() == "auto_protocol_not_supported" => ProbeOutcome::Negative,
        Err(_) => ProbeOutcome::Malformed,
    }
}

fn read_rfb_security_types(
    stream: &mut TcpStream,
    version_minor: u16,
    deadline: Instant,
    max_response_bytes: usize,
) -> Result<Vec<u8>, ProbeOutcome> {
    let count = match rfb_security::security_list_framing(version_minor) {
        SecurityListFraming::ServerSelectedU32 => {
            let mut bytes = [0u8; 4];
            read_exact_until(stream, &mut bytes, deadline)
                .map_err(|(error, received)| classify_read_progress_error(&error, received))?;
            let security = u32::from_be_bytes(bytes);
            if security == 0 {
                return Err(ProbeOutcome::Negative);
            }
            let security = u8::try_from(security).map_err(|_| ProbeOutcome::Malformed)?;
            return Ok(vec![security]);
        }
        SecurityListFraming::CountedU8 => {
            let mut byte = [0u8; 1];
            read_exact_until(stream, &mut byte, deadline)
                .map_err(|(error, received)| classify_read_progress_error(&error, received))?;
            usize::from(byte[0])
        }
    };
    if count == 0 {
        return Err(ProbeOutcome::Negative);
    }
    if count > MAX_RFB_SECURITY_TYPES || count > max_response_bytes {
        return Err(ProbeOutcome::Malformed);
    }
    let mut types = vec![0u8; count];
    read_exact_until(stream, &mut types, deadline)
        .map_err(|(error, received)| classify_read_progress_error(&error, received))?;
    Ok(types)
}

fn classify_rfb_security_types(types: &[u8]) -> Result<ProtocolKind, SessionError> {
    let apple = types
        .iter()
        .copied()
        .any(rfb_security::is_supported_apple_native);
    if apple {
        return Ok(ProtocolKind::AppleRfb);
    }
    if types
        .iter()
        .copied()
        .any(rfb_security::is_supported_standard)
    {
        return Ok(ProtocolKind::StandardRfb);
    }
    Err(SessionError::new("auto_protocol_not_supported"))
}

fn bounded_attempt_deadline(attempt_timeout: Duration, total_deadline: Instant) -> Instant {
    let attempt_deadline = Instant::now() + attempt_timeout;
    attempt_deadline.min(total_deadline)
}

fn connect_bounded(
    ips: &[IpAddr],
    port: u16,
    deadline: Instant,
) -> Result<(TcpStream, SocketAddr), ProbeOutcome> {
    let candidates = connection_candidates(ips);
    let mut saw_timeout = false;
    for (index, ip) in candidates.iter().enumerate() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(ProbeOutcome::Timeout);
        };
        let attempts_left = u32::try_from(candidates.len() - index).unwrap_or(1);
        let per_address = remaining / attempts_left;
        if per_address.is_zero() {
            return Err(ProbeOutcome::Timeout);
        }
        let address = SocketAddr::new(*ip, port);
        match TcpStream::connect_timeout(&address, per_address) {
            Ok(stream) => return Ok((stream, address)),
            Err(error) => match classify_connect_error(&error, Instant::now() >= deadline) {
                ProbeOutcome::Timeout => saw_timeout = true,
                ProbeOutcome::Unreachable => {}
                _ => unreachable!("connect errors have only timeout or unreachable outcomes"),
            },
        }
    }
    if saw_timeout || Instant::now() >= deadline {
        Err(ProbeOutcome::Timeout)
    } else {
        Err(ProbeOutcome::Unreachable)
    }
}

fn remaining_until(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "probe deadline elapsed"))
}

fn read_exact_until(
    stream: &mut TcpStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), (io::Error, usize)> {
    let mut received = 0usize;
    while !bytes.is_empty() {
        let remaining = remaining_until(deadline).map_err(|error| (error, received))?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|error| (error, received))?;
        match stream.read(bytes) {
            Ok(0) => {
                return Err((
                    io::Error::new(io::ErrorKind::UnexpectedEof, "probe peer closed"),
                    received,
                ));
            }
            Ok(count) => {
                received += count;
                bytes = &mut bytes[count..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err((error, received)),
        }
    }
    Ok(())
}

fn write_all_until(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let remaining = remaining_until(deadline)?;
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(bytes) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "probe write zero")),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn classify_read_progress_error(error: &io::Error, received: usize) -> ProbeOutcome {
    if is_timeout(error) {
        ProbeOutcome::Timeout
    } else if error.kind() == io::ErrorKind::UnexpectedEof && received == 0 {
        ProbeOutcome::Negative
    } else if error.kind() == io::ErrorKind::UnexpectedEof {
        ProbeOutcome::Malformed
    } else {
        ProbeOutcome::Unreachable
    }
}

fn bound_resolved_addresses(addresses: Vec<IpAddr>) -> Vec<IpAddr> {
    let unique = addresses
        .into_iter()
        .take(MAX_RESOLVER_SCAN_ADDRESSES)
        .collect::<BTreeSet<_>>();
    let mut v4 = unique.iter().filter_map(|ip| match ip {
        IpAddr::V4(value) => Some(IpAddr::V4(*value)),
        IpAddr::V6(_) => None,
    });
    let mut v6 = unique.iter().filter_map(|ip| match ip {
        IpAddr::V6(value) => Some(IpAddr::V6(*value)),
        IpAddr::V4(_) => None,
    });
    let mut bounded = Vec::with_capacity(MAX_RESOLVED_ADDRESSES);
    while bounded.len() < MAX_RESOLVED_ADDRESSES {
        let before = bounded.len();
        if let Some(ip) = v4.next() {
            bounded.push(ip);
        }
        if bounded.len() < MAX_RESOLVED_ADDRESSES {
            if let Some(ip) = v6.next() {
                bounded.push(ip);
            }
        }
        if bounded.len() == before {
            break;
        }
    }
    bounded
}

fn collect_lookup_ipv4_bounded(lookup: &Lookup) -> Vec<IpAddr> {
    lookup
        .answers()
        .iter()
        .take(MAX_RESOLVER_SCAN_PER_FAMILY)
        .filter_map(|record| match &record.data {
            RData::A(address) => Some(IpAddr::V4(address.0)),
            _ => None,
        })
        .collect()
}

fn collect_lookup_ipv6_bounded(lookup: &Lookup) -> Vec<IpAddr> {
    lookup
        .answers()
        .iter()
        .take(MAX_RESOLVER_SCAN_PER_FAMILY)
        .filter_map(|record| match &record.data {
            RData::AAAA(address) => Some(IpAddr::V6(address.0)),
            _ => None,
        })
        .collect()
}

fn merge_resolved_families_bounded(
    ipv4: impl IntoIterator<Item = IpAddr>,
    ipv6: impl IntoIterator<Item = IpAddr>,
) -> Vec<IpAddr> {
    let addresses = ipv4
        .into_iter()
        .take(MAX_RESOLVER_SCAN_PER_FAMILY)
        .filter(IpAddr::is_ipv4)
        .chain(
            ipv6.into_iter()
                .take(MAX_RESOLVER_SCAN_PER_FAMILY)
                .filter(IpAddr::is_ipv6),
        )
        .collect();
    bound_resolved_addresses(addresses)
}

fn connection_candidates(addresses: &[IpAddr]) -> &[IpAddr] {
    &addresses[..addresses.len().min(MAX_CONNECT_ATTEMPTS)]
}

fn classify_io_error(error: &io::Error) -> ProbeOutcome {
    if is_timeout(error) {
        ProbeOutcome::Timeout
    } else {
        ProbeOutcome::Unreachable
    }
}

fn classify_connect_error(error: &io::Error, deadline_elapsed: bool) -> ProbeOutcome {
    if deadline_elapsed || is_timeout(error) {
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
        0x02 if bytes[12] & !0x1f == 0 && matches!(value, 0 | 1 | 2 | 8) => {
            Ok(RdpNegotiationEvidence::Selected(value))
        }
        0x03 if bytes[12] == 0 && (1..=7).contains(&value) => {
            Ok(RdpNegotiationEvidence::Failure(value))
        }
        _ => Err(SessionError::new("auto_probe_malformed")),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::app::connection::{validate_connection, ConnectionRequest, ServiceKind};
    use crate::platform::{AudioOutputSink, AudioOutputSpec, PlatformError, PlatformServices};
    use secrecy::SecretString;

    struct TestPlatformServices;

    impl PlatformServices for TestPlatformServices {
        fn create_audio_output(
            &self,
            _spec: AudioOutputSpec,
        ) -> Result<AudioOutputSink, PlatformError> {
            Err(PlatformError::new("test_audio_unavailable"))
        }

        fn set_clipboard_text(&self, _text: &str) -> Result<(), PlatformError> {
            Ok(())
        }

        fn open_external_url(&self, _url: &str) -> Result<(), PlatformError> {
            Ok(())
        }
    }

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

    #[test]
    fn auto_selection_preserves_the_exact_injected_platform_service_object() {
        let connection = validate_connection(ConnectionRequest {
            service: ServiceKind::Auto,
            host: "example.invalid".to_owned(),
            port: None,
            username: "local-user".to_owned(),
            password: SecretString::from("secret".to_owned()),
            domain: None,
        })
        .unwrap();
        let services: Arc<dyn PlatformServices> = Arc::new(TestPlatformServices);
        let context = ProtocolContext::with_platform_services(connection, Arc::clone(&services));

        let selected = prepare_selected_context(
            context,
            DetectedEndpoint {
                protocol: ProtocolKind::AppleRfb,
                address: "192.0.2.41:5900".parse().unwrap(),
            },
        )
        .unwrap();

        assert!(Arc::ptr_eq(selected.platform_services(), &services));
        assert_eq!(selected.connection().protocol, ProtocolKind::AppleRfb);
        assert_eq!(
            selected.connection().endpoint.pinned_addr(),
            Some("192.0.2.41:5900".parse().unwrap())
        );
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

    fn spawn_parallel_server(
        connections: usize,
        handler: impl Fn(TcpStream, usize) + Send + Sync + 'static,
    ) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = Arc::new(handler);
        let worker = thread::spawn(move || {
            let mut handlers = Vec::with_capacity(connections);
            for index in 0..connections {
                let (stream, _) = listener.accept().unwrap();
                let handler = Arc::clone(&handler);
                handlers.push(thread::spawn(move || handler(stream, index)));
            }
            for handler in handlers {
                handler.join().unwrap();
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

        for flags in [0x01, 0x02, 0x04, 0x08, 0x10, 0x1f] {
            let mut response_with_defined_flags = RDP_NEGOTIATION_RESPONSE;
            response_with_defined_flags[12] = flags;
            assert_eq!(
                parse_rdp_negotiation_response(&response_with_defined_flags, 64).unwrap(),
                RdpNegotiationEvidence::Selected(2)
            );
        }
        for failure_code in 1..=7u32 {
            let mut failure = RDP_NEGOTIATION_FAILURE;
            failure[15..19].copy_from_slice(&failure_code.to_le_bytes());
            assert_eq!(
                parse_rdp_negotiation_response(&failure, 64).unwrap(),
                RdpNegotiationEvidence::Failure(failure_code)
            );
        }
        for invalid_failure_code in [0u32, 8] {
            let mut failure = RDP_NEGOTIATION_FAILURE;
            failure[15..19].copy_from_slice(&invalid_failure_code.to_le_bytes());
            assert_eq!(
                parse_rdp_negotiation_response(&failure, 64)
                    .unwrap_err()
                    .code(),
                "auto_probe_malformed"
            );
        }
    }

    #[test]
    fn no_port_plan_uses_only_stock_rdp_and_rfb_ports() {
        let plan = ProbePlan::from_explicit_port(None);
        assert_eq!(plan.rdp_port, 3389);
        assert_eq!(plan.rfb_port, 5900);
    }

    #[test]
    fn exactly_one_rfb_banner_selects_rfb_without_sending_credentials() {
        let (rdp_port, rdp_worker) = spawn_server(1, |stream, _| {
            assert_fixed_rdp_probe_without_credentials(stream.try_clone().unwrap());
        });
        let (rfb_port, rfb_worker) = spawn_server(1, |mut stream, _| {
            stream.write_all(b"RFB 003.008\n").unwrap();
            let mut reply = [0u8; 12];
            stream.read_exact(&mut reply).unwrap();
            assert_eq!(&reply, b"RFB 003.008\n");
            stream.write_all(&[1, 2]).unwrap();
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

        assert_eq!(selected.protocol, ProtocolKind::StandardRfb);
        assert_eq!(selected.address.port(), rfb_port);
        rdp_worker.join().unwrap();
        rfb_worker.join().unwrap();
    }

    #[test]
    fn valid_rdp_negotiation_failure_still_identifies_rdp_without_credssp() {
        let (rdp_port, rdp_worker) = spawn_server(1, |mut stream, _| {
            assert_fixed_rdp_probe_without_credentials(stream.try_clone().unwrap());
            stream.write_all(&RDP_NEGOTIATION_FAILURE).unwrap();
        });
        let (rfb_port, rfb_worker) = spawn_server(1, |_, _| {});

        let selected = detect_on_ips(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan { rdp_port, rfb_port },
            limits(),
        )
        .unwrap();

        assert_eq!(selected.protocol, ProtocolKind::Rdp);
        assert_eq!(selected.address.port(), rdp_port);
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
            let mut reply = [0u8; 12];
            stream.read_exact(&mut reply).unwrap();
            stream.write_all(&[1, 2]).unwrap();
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
        assert_eq!(
            classify_connect_error(
                &io::Error::new(io::ErrorKind::ConnectionRefused, "fixture"),
                false,
            ),
            ProbeOutcome::Unreachable
        );
        assert_eq!(
            classify_connect_error(&io::Error::new(io::ErrorKind::TimedOut, "fixture"), false,),
            ProbeOutcome::Timeout
        );
        assert_eq!(
            classify_connect_error(
                &io::Error::new(io::ErrorKind::ConnectionRefused, "fixture"),
                true,
            ),
            ProbeOutcome::Timeout
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
        let plan = ProbePlan::from_explicit_port(Some(15900));
        assert_eq!(plan.rdp_port, 15900);
        assert_eq!(plan.rfb_port, 15900);
    }

    #[test]
    fn explicit_auto_port_accepts_zero_byte_passive_rfb_wait_for_real_rdp_listener() {
        let (port, server) = spawn_parallel_server(2, |mut stream, _| {
            stream
                .set_read_timeout(Some(Duration::from_millis(40)))
                .unwrap();
            let mut request = [0u8; RDP_NEGOTIATION_REQUEST.len()];
            match stream.read_exact(&mut request) {
                Ok(()) => {
                    assert_eq!(request, RDP_NEGOTIATION_REQUEST);
                    stream.write_all(&RDP_NEGOTIATION_RESPONSE).unwrap();
                }
                Err(error) if is_timeout(&error) => {
                    thread::sleep(Duration::from_millis(140));
                }
                Err(error) => panic!("unexpected RDP listener read error: {error}"),
            }
        });
        let explicit_limits =
            AutoProbeLimits::new(Duration::from_millis(90), Duration::from_millis(180), 64)
                .unwrap();

        let selected = detect_on_ips(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan::from_explicit_port(Some(port)),
            explicit_limits,
        )
        .unwrap();

        assert_eq!(selected.protocol, ProtocolKind::Rdp);
        assert_eq!(selected.address.port(), port);
        server.join().unwrap();
    }

    #[test]
    fn explicit_auto_port_rejects_partial_rfb_garbage_timeout_beside_positive_rdp() {
        let (port, server) = spawn_parallel_server(2, |mut stream, _| {
            stream
                .set_read_timeout(Some(Duration::from_millis(30)))
                .unwrap();
            let mut request = [0u8; RDP_NEGOTIATION_REQUEST.len()];
            match stream.read_exact(&mut request) {
                Ok(()) => {
                    assert_eq!(request, RDP_NEGOTIATION_REQUEST);
                    stream.write_all(&RDP_NEGOTIATION_RESPONSE).unwrap();
                }
                Err(error) if is_timeout(&error) => {
                    stream.write_all(b"RFB 003.").unwrap();
                    thread::sleep(Duration::from_millis(140));
                }
                Err(error) => panic!("unexpected RDP listener read error: {error}"),
            }
        });
        let explicit_limits =
            AutoProbeLimits::new(Duration::from_millis(90), Duration::from_millis(180), 64)
                .unwrap();

        let error = detect_on_ips(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan::from_explicit_port(Some(port)),
            explicit_limits,
        )
        .unwrap_err();

        assert_eq!(error.code(), "auto_probe_malformed");
        server.join().unwrap();
    }

    #[test]
    fn explicit_auto_port_selects_a_real_server_first_rfb_listener() {
        let (port, server) = spawn_parallel_server(2, |mut stream, _| {
            stream.write_all(b"RFB 003.008\n").unwrap();
            let mut client_first = [0u8; 12];
            stream.read_exact(&mut client_first).unwrap();
            if &client_first == b"RFB 003.008\n" {
                stream.write_all(&[1, 2]).unwrap();
            } else {
                assert_eq!(&client_first, &RDP_NEGOTIATION_REQUEST[..12]);
            }
        });

        let selected = detect_on_ips(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan::from_explicit_port(Some(port)),
            limits(),
        )
        .unwrap();

        assert_eq!(selected.protocol, ProtocolKind::StandardRfb);
        assert_eq!(selected.address.port(), port);
        server.join().unwrap();
    }

    #[test]
    fn absolute_read_deadline_rejects_a_slowloris_that_drips_bytes() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for byte in b"RFB 003.008\n" {
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        let mut stream = TcpStream::connect(address).unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_millis(80);
        let mut banner = [0u8; 12];

        let (error, _) = read_exact_until(&mut stream, &mut banner, deadline).unwrap_err();

        assert!(is_timeout(&error));
        assert!(started.elapsed() < Duration::from_millis(160));
        server.join().unwrap();
    }

    #[test]
    fn rdp_partial_tpkt_body_cannot_restart_the_attempt_deadline() {
        let (port, server) = spawn_server(1, |mut stream, _| {
            assert_fixed_rdp_probe_without_credentials(stream.try_clone().unwrap());
            for byte in RDP_NEGOTIATION_RESPONSE {
                if stream.write_all(&[byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        let limits =
            AutoProbeLimits::new(Duration::from_millis(75), Duration::from_millis(100), 64)
                .unwrap();
        let started = Instant::now();

        let outcome = probe_rdp(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port,
            limits,
            started + limits.total_timeout,
        );

        assert!(matches!(outcome, ProbeOutcome::Timeout));
        assert!(started.elapsed() < Duration::from_millis(150));
        server.join().unwrap();
    }

    #[test]
    fn rfb_partial_security_list_is_malformed_and_a_stalled_list_times_out() {
        let (partial_port, partial_server) = spawn_server(1, |mut stream, _| {
            stream.write_all(b"RFB 003.008\n").unwrap();
            let mut reply = [0u8; 12];
            stream.read_exact(&mut reply).unwrap();
            stream.write_all(&[2, 2]).unwrap();
        });
        let partial = probe_rfb(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            partial_port,
            limits(),
            Instant::now() + Duration::from_millis(350),
        );
        assert_eq!(partial, ProbeOutcome::Malformed);
        partial_server.join().unwrap();

        let (stalled_port, stalled_server) = spawn_server(1, |mut stream, _| {
            stream.write_all(b"RFB 003.008\n").unwrap();
            let mut reply = [0u8; 12];
            stream.read_exact(&mut reply).unwrap();
            stream.write_all(&[2, 2]).unwrap();
            thread::sleep(Duration::from_millis(180));
        });
        let short_limits =
            AutoProbeLimits::new(Duration::from_millis(50), Duration::from_millis(80), 64).unwrap();
        let stalled = probe_rfb(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            stalled_port,
            short_limits,
            Instant::now() + short_limits.total_timeout,
        );
        assert_eq!(stalled, ProbeOutcome::Timeout);
        stalled_server.join().unwrap();
    }

    #[test]
    fn rfb_security_enumeration_selects_apple_standard_and_mixed_deterministically() {
        assert_eq!(
            classify_rfb_security_types(&[36, 33, 30]).unwrap(),
            ProtocolKind::AppleRfb
        );
        assert_eq!(
            classify_rfb_security_types(&[2, 1]).unwrap(),
            ProtocolKind::StandardRfb
        );
        for mixed in [vec![2, 36], vec![36, 2], vec![1, 30]] {
            assert_eq!(
                classify_rfb_security_types(&mixed).unwrap(),
                ProtocolKind::AppleRfb
            );
        }
        assert_eq!(
            classify_rfb_security_types(&[16, 19]).unwrap_err().code(),
            "auto_protocol_not_supported"
        );
        assert_eq!(
            classify_rfb_security_types(&[35]).unwrap_err().code(),
            "auto_protocol_not_supported"
        );
        assert_eq!(
            classify_rfb_security_types(&[35, 2]).unwrap(),
            ProtocolKind::StandardRfb
        );
    }

    #[test]
    fn rfb_37_probe_reads_the_standard_u8_security_count() {
        let (port, server) = spawn_server(1, |mut stream, _| {
            stream.write_all(b"RFB 003.007\n").unwrap();
            let mut reply = [0u8; 12];
            stream.read_exact(&mut reply).unwrap();
            assert_eq!(&reply, b"RFB 003.007\n");
            stream.write_all(&[1, 2]).unwrap();
        });

        let outcome = probe_rfb(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port,
            limits(),
            Instant::now() + Duration::from_millis(350),
        );

        assert!(matches!(
            outcome,
            ProbeOutcome::Positive(DetectedEndpoint {
                protocol: ProtocolKind::StandardRfb,
                ..
            })
        ));
        server.join().unwrap();
    }

    #[test]
    fn rfb_38_probe_enumerates_security_without_selecting_or_sending_credentials() {
        let (port, server) = spawn_server(1, |mut stream, _| {
            stream.write_all(b"RFB 003.008\n").unwrap();
            let mut reply = [0u8; 12];
            stream.read_exact(&mut reply).unwrap();
            assert_eq!(&reply, b"RFB 003.008\n");
            stream.write_all(&[2, 2, 36]).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let mut unexpected = [0u8; 64];
            assert_eq!(stream.read(&mut unexpected).unwrap_or(0), 0);
        });
        let outcome = probe_rfb(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port,
            limits(),
            Instant::now() + Duration::from_millis(350),
        );

        assert!(matches!(
            outcome,
            ProbeOutcome::Positive(DetectedEndpoint {
                protocol: ProtocolKind::AppleRfb,
                ..
            })
        ));
        server.join().unwrap();
    }

    #[test]
    fn rfb_33_and_apple_889_use_the_existing_exact_version_rules() {
        for (banner, security_bytes, expected_reply, expected_protocol) in [
            (
                *b"RFB 003.003\n",
                vec![0, 0, 0, 2],
                *b"RFB 003.003\n",
                ProtocolKind::StandardRfb,
            ),
            (
                *b"RFB 003.889\n",
                vec![1, 36],
                *b"RFB 003.889\n",
                ProtocolKind::AppleRfb,
            ),
        ] {
            let (port, server) = spawn_server(1, move |mut stream, _| {
                stream.write_all(&banner).unwrap();
                let mut reply = [0u8; 12];
                stream.read_exact(&mut reply).unwrap();
                assert_eq!(reply, expected_reply);
                stream.write_all(&security_bytes).unwrap();
            });
            let outcome = probe_rfb(
                &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
                port,
                limits(),
                Instant::now() + Duration::from_millis(350),
            );
            assert!(matches!(
                outcome,
                ProbeOutcome::Positive(endpoint) if endpoint.protocol == expected_protocol
            ));
            server.join().unwrap();
        }
    }

    #[test]
    fn strict_merge_rejects_positive_with_timeout_or_malformed() {
        let endpoint = DetectedEndpoint {
            protocol: ProtocolKind::Rdp,
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 3389)),
        };
        for (other, code) in [
            (ProbeOutcome::Timeout, "auto_probe_timeout"),
            (ProbeOutcome::Malformed, "auto_probe_malformed"),
        ] {
            assert_eq!(
                merge_probe_outcomes(ProbeOutcome::Positive(endpoint), other, false)
                    .unwrap_err()
                    .code(),
                code
            );
        }
        assert_eq!(
            merge_probe_outcomes(
                ProbeOutcome::Positive(endpoint),
                ProbeOutcome::Unreachable,
                false,
            )
            .unwrap(),
            endpoint
        );
    }

    #[test]
    fn shared_port_connection_timeout_is_not_a_passive_rfb_wait() {
        let endpoint = DetectedEndpoint {
            protocol: ProtocolKind::Rdp,
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 3389)),
        };

        assert_eq!(
            merge_probe_outcomes(
                ProbeOutcome::Positive(endpoint),
                ProbeOutcome::Timeout,
                true,
            )
            .unwrap_err()
            .code(),
            "auto_probe_timeout"
        );
    }

    #[test]
    fn exhausted_total_budget_fails_before_starting_probe_workers() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let error = detect_on_ips_until(
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ProbePlan {
                rdp_port: port,
                rfb_port: port,
            },
            limits(),
            Instant::now() - Duration::from_millis(1),
        )
        .unwrap_err();

        assert_eq!(error.code(), "auto_probe_timeout");
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn rdp_and_rfb_probes_start_in_parallel() {
        let barrier = Arc::new(Barrier::new(3));
        let (rdp_port, rdp_server) = spawn_server(1, {
            let barrier = Arc::clone(&barrier);
            move |_, _| {
                barrier.wait();
                thread::sleep(Duration::from_millis(200));
            }
        });
        let (rfb_port, rfb_server) = spawn_server(1, {
            let barrier = Arc::clone(&barrier);
            move |_, _| {
                barrier.wait();
                thread::sleep(Duration::from_millis(200));
            }
        });
        let detector = thread::spawn(move || {
            detect_on_ips(
                &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
                ProbePlan { rdp_port, rfb_port },
                AutoProbeLimits::new(Duration::from_millis(100), Duration::from_millis(130), 64)
                    .unwrap(),
            )
        });

        barrier.wait();
        assert_eq!(
            detector.join().unwrap().unwrap_err().code(),
            "auto_probe_timeout"
        );
        rdp_server.join().unwrap();
        rfb_server.join().unwrap();
    }

    #[test]
    fn resolved_addresses_are_deduplicated_bounded_and_family_fair() {
        let addresses = bound_resolved_addresses(vec![
            "127.0.0.3".parse().unwrap(),
            "::3".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
            "::1".parse().unwrap(),
            "127.0.0.2".parse().unwrap(),
            "::2".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
            "127.0.0.4".parse().unwrap(),
            "::4".parse().unwrap(),
            "127.0.0.5".parse().unwrap(),
        ]);
        assert_eq!(addresses.len(), MAX_RESOLVED_ADDRESSES);
        assert_eq!(addresses[0], "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(addresses[1], "::1".parse::<IpAddr>().unwrap());
        assert_eq!(addresses[2], "127.0.0.2".parse::<IpAddr>().unwrap());
        assert_eq!(addresses[3], "::2".parse::<IpAddr>().unwrap());
        assert_eq!(
            connection_candidates(&addresses).len(),
            MAX_CONNECT_ATTEMPTS
        );
    }

    #[test]
    fn resolver_acquisition_stops_scanning_an_excessive_address_iterator() {
        let scanned = Arc::new(AtomicUsize::new(0));
        let iterator = (0u16..=u16::MAX).map({
            let scanned = Arc::clone(&scanned);
            move |value| {
                scanned.fetch_add(1, Ordering::SeqCst);
                IpAddr::V4(Ipv4Addr::new(10, (value >> 8) as u8, value as u8, 1))
            }
        });

        let addresses = merge_resolved_families_bounded(iterator, std::iter::empty());

        assert_eq!(addresses.len(), MAX_RESOLVED_ADDRESSES);
        assert_eq!(scanned.load(Ordering::SeqCst), MAX_RESOLVER_SCAN_PER_FAMILY);
    }

    #[test]
    fn family_batched_dns_results_keep_reachable_ipv4_in_connect_candidates() {
        let scanned_v4 = Arc::new(AtomicUsize::new(0));
        let scanned_v6 = Arc::new(AtomicUsize::new(0));
        let ipv4 = std::iter::once(IpAddr::V4(Ipv4Addr::LOCALHOST)).inspect({
            let scanned_v4 = Arc::clone(&scanned_v4);
            move |_| {
                scanned_v4.fetch_add(1, Ordering::SeqCst);
            }
        });
        let ipv6 = (1u16..=u16::MAX).map({
            let scanned_v6 = Arc::clone(&scanned_v6);
            move |suffix| {
                scanned_v6.fetch_add(1, Ordering::SeqCst);
                IpAddr::V6(std::net::Ipv6Addr::new(
                    0x2001, 0xdb8, 0, 0, 0, 0, 0, suffix,
                ))
            }
        });

        let addresses = merge_resolved_families_bounded(ipv4, ipv6);

        assert!(connection_candidates(&addresses).contains(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert_eq!(scanned_v4.load(Ordering::SeqCst), 1);
        assert!(scanned_v6.load(Ordering::SeqCst) <= MAX_RESOLVER_SCAN_PER_FAMILY);
    }

    #[test]
    fn multi_address_connect_pins_the_exact_successful_candidate() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let expected = listener.local_addr().unwrap();
        let server = thread::spawn(move || listener.accept().unwrap().1);
        let candidates = [
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        ];

        let (stream, pinned) = connect_bounded(
            &candidates,
            expected.port(),
            Instant::now() + Duration::from_millis(800),
        )
        .unwrap();

        assert_eq!(pinned, expected);
        assert_eq!(stream.peer_addr().unwrap(), expected);
        drop(stream);
        server.join().unwrap();
    }

    #[test]
    fn cancellable_resolver_timeout_does_not_poison_the_next_lookup() {
        let runtime = build_resolver_runtime().unwrap();
        let timed_out = runtime.block_on(resolve_future_until(
            std::future::pending::<Result<Vec<IpAddr>, SessionError>>(),
            Instant::now() + Duration::from_millis(20),
        ));
        assert_eq!(timed_out.unwrap_err().code(), "auto_probe_timeout");

        let recovered = runtime.block_on(resolve_future_until(
            std::future::ready(Ok::<_, SessionError>(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])),
            Instant::now() + Duration::from_millis(100),
        ));
        assert_eq!(recovered.unwrap(), vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    }

    #[test]
    fn expired_resolver_deadline_does_not_poll_the_backend_future() {
        let runtime = build_resolver_runtime().unwrap();
        let polls = Arc::new(AtomicUsize::new(0));
        let result = runtime.block_on(resolve_future_until(
            {
                let polls = Arc::clone(&polls);
                async move {
                    polls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, SessionError>(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
                }
            },
            Instant::now() - Duration::from_millis(1),
        ));

        assert_eq!(result.unwrap_err().code(), "auto_probe_timeout");
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }

    #[cfg(feature = "cli")]
    #[test]
    fn full_auto_adapter_probe_never_emits_the_validated_connection_secret() {
        use secrecy::SecretString;

        use crate::app::connection::{validate_connection, ConnectionRequest, ServiceKind};
        use crate::session::{ProtocolContext, SessionEngine, UiWakeHandle};

        struct NoopWake;
        impl UiWakeHandle for NoopWake {
            fn wake(&self) -> Result<(), SessionError> {
                Ok(())
            }
        }

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let probe_captures = Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
        let rfb_connections = Arc::new(AtomicUsize::new(0));
        let server = {
            let probe_captures = Arc::clone(&probe_captures);
            let rfb_connections = Arc::clone(&rfb_connections);
            thread::spawn(move || {
                let mut handlers = Vec::new();
                for _ in 0..3 {
                    let (mut stream, _) = listener.accept().unwrap();
                    let probe_captures = Arc::clone(&probe_captures);
                    let rfb_connections = Arc::clone(&rfb_connections);
                    handlers.push(thread::spawn(move || {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(100)))
                            .unwrap();
                        let mut first = [0u8; 64];
                        match stream.read(&mut first) {
                            Ok(count) if count > 0 => {
                                probe_captures.lock().unwrap().push(first[..count].to_vec());
                            }
                            _ => {
                                stream.write_all(b"RFB 003.008\n").unwrap();
                                let mut client_banner = [0u8; 12];
                                stream.read_exact(&mut client_banner).unwrap();
                                let index = rfb_connections.fetch_add(1, Ordering::SeqCst);
                                if index == 0 {
                                    probe_captures.lock().unwrap().push(client_banner.to_vec());
                                }
                                stream.write_all(&[1, 2]).unwrap();
                                let mut selection = [0u8; 1];
                                let _ = stream.read(&mut selection);
                            }
                        }
                    }));
                }
                for handler in handlers {
                    handler.join().unwrap();
                }
            })
        };
        let canary = "credential-canary-full-auto";
        let connection = validate_connection(ConnectionRequest {
            service: ServiceKind::Auto,
            host: Ipv4Addr::LOCALHOST.to_string(),
            port: Some(address.port()),
            username: "probe-user-canary".to_owned(),
            password: SecretString::from(canary.to_owned()),
            domain: None,
        })
        .unwrap();
        assert!(!format!("{connection:?}").contains(canary));

        let engine = SessionEngine::spawn(
            Box::new(AutoAdapter::new()),
            ProtocolContext::new(connection),
            Arc::new(NoopWake),
        )
        .unwrap();
        engine.join().unwrap();
        server.join().unwrap();

        let captures = probe_captures.lock().unwrap();
        assert_eq!(captures.len(), 2);
        for capture in captures.iter() {
            assert!(!capture
                .windows(canary.len())
                .any(|bytes| bytes == canary.as_bytes()));
            assert!(!capture
                .windows(b"probe-user-canary".len())
                .any(|bytes| bytes == b"probe-user-canary"));
        }
        assert!(captures
            .iter()
            .any(|capture| capture == &RDP_NEGOTIATION_REQUEST));
        assert!(captures.iter().any(|capture| capture == b"RFB 003.008\n"));
    }
}
