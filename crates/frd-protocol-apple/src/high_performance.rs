use std::fmt;
use std::time::{Duration, Instant};

use crate::dynamic_resolution::DisplaySize;
use crate::hpss::ServerStateGeometry;

pub(crate) const APPLE_HIGH_PERFORMANCE_UNAVAILABLE: &str = "apple_high_performance_unavailable";
const STARTUP_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StartupRequestMessage {
    DisplayQuery,
    FramebufferUpdate,
}

impl fmt::Display for StartupRequestMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DisplayQuery => "09",
            Self::FramebufferUpdate => "03",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HighPerformanceDiagnostic {
    SinkReady,
    FactoryCreate,
    TcpConnected,
    RfbBannerAccepted,
    SecurityOfferReceived,
    NamedSrpNotOffered,
    EncryptionInvariant,
    SetDisplayWriteClosed,
    StartupRequestWriteClosed {
        message: StartupRequestMessage,
    },
    ConfirmationTimeout {
        elapsed_ms: u64,
    },
    ConfirmationPeerClosed,
    ConfirmationMalformed,
    ConfirmationCommitWriteClosed,
    PendingControlBudget,
    NamedSrpSelected,
    SrpStep1Written,
    SrpChallengeAccepted,
    SrpProofComputed,
    SrpStep2Written,
    SrpResponseAccepted,
    SrpAuthenticated,
    ClientInitWritten,
    ServerInitAccepted,
    EncryptionRequestWritten,
    EncryptionInfoAccepted,
    EncryptionActivated,
    RuntimeHandoff,
    AuthenticationFailed,
    SetDisplayWritten {
        server_init_width: u16,
        server_init_height: u16,
        startup_width: u16,
        startup_height: u16,
    },
    ServerStateAccepted {
        accepted_width: u16,
        accepted_height: u16,
        elapsed_ms: u64,
    },
}

impl HighPerformanceDiagnostic {
    pub(crate) const fn stage_code(self) -> &'static str {
        match self {
            Self::SinkReady => "hp00_sink_ready",
            Self::FactoryCreate => "hp00_factory_create",
            Self::TcpConnected => "hp00_tcp_connected",
            Self::RfbBannerAccepted => "hp00_rfb_banner_accepted",
            Self::SecurityOfferReceived => "hp00_security_offer_received",
            Self::NamedSrpNotOffered => "hp01_named_srp_not_offered",
            Self::EncryptionInvariant => "hp02_encryption_invariant",
            Self::SetDisplayWriteClosed => "hp03_set_display_write_closed",
            Self::StartupRequestWriteClosed { .. } => "hp04_startup_request_write_closed",
            Self::ConfirmationTimeout { .. } => "hp05_confirmation_timeout",
            Self::ConfirmationPeerClosed => "hp06_confirmation_peer_closed",
            Self::ConfirmationMalformed => "hp07_confirmation_malformed",
            Self::ConfirmationCommitWriteClosed => "hp08_confirmation_commit_write_closed",
            Self::PendingControlBudget => "hp09_pending_control_budget",
            Self::NamedSrpSelected => "hp00_named_srp_selected",
            Self::SrpStep1Written => "hp00_srp_step1_written",
            Self::SrpChallengeAccepted => "hp00_srp_challenge_accepted",
            Self::SrpProofComputed => "hp00_srp_proof_computed",
            Self::SrpStep2Written => "hp00_srp_step2_written",
            Self::SrpResponseAccepted => "hp00_srp_response_accepted",
            Self::SrpAuthenticated => "hp00_srp_authenticated",
            Self::ClientInitWritten => "hp00_client_init_written",
            Self::ServerInitAccepted => "hp00_server_init_accepted",
            Self::EncryptionRequestWritten => "hp00_encryption_request_written",
            Self::EncryptionInfoAccepted => "hp00_encryption_info_accepted",
            Self::EncryptionActivated => "hp00_encryption_activated",
            Self::RuntimeHandoff => "hp00_runtime_handoff",
            Self::AuthenticationFailed => "hp10_authentication_failed",
            Self::SetDisplayWritten { .. } => "hp00_set_display_written",
            Self::ServerStateAccepted { .. } => "hp00_server_state_accepted",
        }
    }

    pub(crate) fn emit(self) {
        #[cfg(debug_assertions)]
        eprintln!("{self}");
    }
}

impl fmt::Display for HighPerformanceDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "[apple-hp-stage] stage={}", self.stage_code())?;
        match self {
            Self::StartupRequestWriteClosed { message } => {
                write!(formatter, " message={message}")
            }
            Self::ConfirmationTimeout { elapsed_ms } => {
                write!(formatter, " elapsed_ms={elapsed_ms}")
            }
            Self::SetDisplayWritten {
                server_init_width,
                server_init_height,
                startup_width,
                startup_height,
            } => write!(
                formatter,
                " server_init_width={server_init_width} server_init_height={server_init_height} startup_width={startup_width} startup_height={startup_height}"
            ),
            Self::ServerStateAccepted {
                accepted_width,
                accepted_height,
                elapsed_ms,
            } => write!(
                formatter,
                " accepted_width={accepted_width} accepted_height={accepted_height} elapsed_ms={elapsed_ms}"
            ),
            Self::SinkReady
            | Self::FactoryCreate
            | Self::TcpConnected
            | Self::RfbBannerAccepted
            | Self::SecurityOfferReceived
            | Self::NamedSrpNotOffered
            | Self::EncryptionInvariant
            | Self::SetDisplayWriteClosed
            | Self::ConfirmationPeerClosed
            | Self::ConfirmationMalformed
            | Self::ConfirmationCommitWriteClosed
            | Self::PendingControlBudget
            | Self::NamedSrpSelected
            | Self::SrpStep1Written
            | Self::SrpChallengeAccepted
            | Self::SrpProofComputed
            | Self::SrpStep2Written
            | Self::SrpResponseAccepted
            | Self::SrpAuthenticated
            | Self::ClientInitWritten
            | Self::ServerInitAccepted
            | Self::EncryptionRequestWritten
            | Self::EncryptionInfoAccepted
            | Self::EncryptionActivated
            | Self::RuntimeHandoff
            | Self::AuthenticationFailed => Ok(()),
        }
    }
}

pub(crate) struct HighPerformanceStageObserver<'a> {
    sink: Option<&'a mut dyn FnMut(HighPerformanceDiagnostic)>,
}

impl<'a> HighPerformanceStageObserver<'a> {
    pub(crate) fn for_protocol(
        protocol_id: &frd_core::ProtocolId,
        sink: &'a mut dyn FnMut(HighPerformanceDiagnostic),
    ) -> Self {
        Self {
            sink: (protocol_id == &frd_core::ProtocolId::apple_high_performance()).then_some(sink),
        }
    }

    pub(crate) const fn disabled() -> Self {
        Self { sink: None }
    }

    pub(crate) fn observe(&mut self, diagnostic: HighPerformanceDiagnostic) {
        if let Some(sink) = self.sink.as_mut() {
            sink(diagnostic);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HighPerformanceUnavailable {
    diagnostic: HighPerformanceDiagnostic,
}

impl HighPerformanceUnavailable {
    pub(crate) fn diagnosed(diagnostic: HighPerformanceDiagnostic) -> Self {
        diagnostic.emit();
        Self { diagnostic }
    }

    pub(crate) const fn code(self) -> &'static str {
        APPLE_HIGH_PERFORMANCE_UNAVAILABLE
    }

    #[cfg(test)]
    pub(crate) const fn stage_code(self) -> &'static str {
        self.diagnostic.stage_code()
    }
}

impl fmt::Display for HighPerformanceUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for HighPerformanceUnavailable {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HighPerformanceConfirmation {
    pub(crate) size: DisplaySize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HighPerformanceObservation {
    Confirmed(HighPerformanceConfirmation),
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HighPerformanceStartupState {
    Awaiting,
    Confirmed(HighPerformanceConfirmation),
    Failed(HighPerformanceUnavailable),
}

#[derive(Clone)]
pub(crate) struct HighPerformanceStartupGate {
    requested_at: Instant,
    state: HighPerformanceStartupState,
}

impl HighPerformanceStartupGate {
    pub(crate) fn new(requested_at: Instant) -> Self {
        Self {
            requested_at,
            state: HighPerformanceStartupState::Awaiting,
        }
    }

    pub(crate) fn is_awaiting(&self) -> bool {
        matches!(self.state, HighPerformanceStartupState::Awaiting)
    }

    pub(crate) fn is_confirmed(&self) -> bool {
        matches!(self.state, HighPerformanceStartupState::Confirmed(_))
    }

    pub(crate) fn confirmation(&self) -> Option<HighPerformanceConfirmation> {
        match self.state {
            HighPerformanceStartupState::Confirmed(confirmation) => Some(confirmation),
            HighPerformanceStartupState::Awaiting | HighPerformanceStartupState::Failed(_) => None,
        }
    }

    pub(crate) fn elapsed_ms_at(&self, now: Instant) -> u64 {
        elapsed_ms(self.requested_at, now)
    }

    pub(crate) fn ensure_not_timed_out(
        &mut self,
        now: Instant,
    ) -> Result<(), HighPerformanceUnavailable> {
        match self.state {
            HighPerformanceStartupState::Failed(error) => Err(error),
            HighPerformanceStartupState::Confirmed(_) => Ok(()),
            HighPerformanceStartupState::Awaiting if self.is_before_deadline(now) => Ok(()),
            HighPerformanceStartupState::Awaiting => {
                self.fail(HighPerformanceDiagnostic::ConfirmationTimeout {
                    elapsed_ms: elapsed_ms(self.requested_at, now),
                })
            }
        }
    }

    pub(crate) fn observe_server_state_at(
        &mut self,
        geometry: ServerStateGeometry,
        observed_at: Instant,
    ) -> Result<HighPerformanceObservation, HighPerformanceUnavailable> {
        if let HighPerformanceStartupState::Failed(error) = self.state {
            return Err(error);
        }

        let Some(size) = DisplaySize::new(geometry.width, geometry.height) else {
            return self.fail(HighPerformanceDiagnostic::ConfirmationMalformed);
        };
        let confirmation = HighPerformanceConfirmation { size };

        match self.state {
            HighPerformanceStartupState::Failed(error) => Err(error),
            HighPerformanceStartupState::Confirmed(existing) if existing == confirmation => {
                Ok(HighPerformanceObservation::Duplicate)
            }
            HighPerformanceStartupState::Confirmed(_) => {
                self.fail(HighPerformanceDiagnostic::ConfirmationMalformed)
            }
            HighPerformanceStartupState::Awaiting if self.is_before_deadline(observed_at) => {
                self.state = HighPerformanceStartupState::Confirmed(confirmation);
                Ok(HighPerformanceObservation::Confirmed(confirmation))
            }
            HighPerformanceStartupState::Awaiting => {
                self.fail(HighPerformanceDiagnostic::ConfirmationTimeout {
                    elapsed_ms: elapsed_ms(self.requested_at, observed_at),
                })
            }
        }
    }

    fn fail<T>(
        &mut self,
        diagnostic: HighPerformanceDiagnostic,
    ) -> Result<T, HighPerformanceUnavailable> {
        let error = HighPerformanceUnavailable::diagnosed(diagnostic);
        self.state = HighPerformanceStartupState::Failed(error);
        Err(error)
    }

    fn is_before_deadline(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.requested_at) < STARTUP_CONFIRMATION_TIMEOUT
    }
}

fn elapsed_ms(started_at: Instant, now: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(started_at).as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use anyhow::Error;

    use super::{
        HighPerformanceDiagnostic, HighPerformanceObservation, HighPerformanceStartupGate,
        HighPerformanceUnavailable, StartupRequestMessage, APPLE_HIGH_PERFORMANCE_UNAVAILABLE,
    };
    use crate::hpss::{
        parse_server_state_geometry, ServerStateGeometry,
        CAPTURED_SERVER_STATE_WITH_ACTIVE_FRAMEBUFFER,
    };

    const GEOMETRY: ServerStateGeometry = ServerStateGeometry {
        message_version: 5,
        display_count: 1,
        width: 1440,
        height: 2560,
    };

    fn latest_addable_instant() -> Instant {
        let base = Instant::now();
        let mut maximum_seconds = 0_u64;
        for bit in (0..u64::BITS).rev() {
            let candidate = maximum_seconds | (1_u64 << bit);
            if base.checked_add(Duration::from_secs(candidate)).is_some() {
                maximum_seconds = candidate;
            }
        }
        base.checked_add(Duration::from_secs(maximum_seconds))
            .unwrap()
    }

    #[test]
    fn strict_startup_pending_accepts_before_deadline() {
        let requested_at = Instant::now();
        let mut gate = HighPerformanceStartupGate::new(requested_at);

        gate.ensure_not_timed_out(requested_at + Duration::from_millis(4_999))
            .unwrap();

        assert!(gate.is_awaiting());
        assert!(!gate.is_confirmed());
        assert_eq!(gate.confirmation(), None);
    }

    #[test]
    fn strict_startup_confirms_one_valid_geometry_with_exact_display_size() {
        let requested_at = Instant::now();
        let mut gate = HighPerformanceStartupGate::new(requested_at);

        let observation = gate
            .observe_server_state_at(GEOMETRY, requested_at + Duration::from_secs(1))
            .unwrap();

        let HighPerformanceObservation::Confirmed(confirmation) = observation else {
            panic!("首个严格 ServerState 必须确认高性能显示器");
        };
        assert_eq!(confirmation.size.width, 1440);
        assert_eq!(confirmation.size.height, 2560);
        let confirmed = gate.confirmation().unwrap();
        assert_eq!(confirmed.size.width, 1440);
        assert_eq!(confirmed.size.height, 2560);
        assert!(!gate.is_awaiting());
        assert!(gate.is_confirmed());
    }

    #[test]
    fn strict_startup_confirms_the_captured_active_framebuffer_group() {
        let requested_at = Instant::now();
        let mut gate = HighPerformanceStartupGate::new(requested_at);
        let geometry =
            parse_server_state_geometry(&CAPTURED_SERVER_STATE_WITH_ACTIVE_FRAMEBUFFER).unwrap();

        let observation = gate
            .observe_server_state_at(geometry, requested_at + Duration::from_secs(1))
            .unwrap();

        let HighPerformanceObservation::Confirmed(confirmation) = observation else {
            panic!("捕获的活动 framebuffer 几何必须确认高性能显示器");
        };
        assert_eq!(confirmation.size.width, 1331);
        assert_eq!(confirmation.size.height, 2365);
    }

    #[test]
    fn strict_startup_near_instant_limit_does_not_overflow_the_deadline() {
        let requested_at = latest_addable_instant();
        assert!(requested_at.checked_add(Duration::from_secs(5)).is_none());

        let mut gate = HighPerformanceStartupGate::new(requested_at);

        gate.ensure_not_timed_out(requested_at).unwrap();
        assert!(gate.is_awaiting());
    }

    #[test]
    fn strict_startup_accepts_matching_duplicate_observation() {
        let requested_at = Instant::now();
        let mut gate = HighPerformanceStartupGate::new(requested_at);
        gate.observe_server_state_at(GEOMETRY, requested_at + Duration::from_secs(1))
            .unwrap();

        let observation = gate
            .observe_server_state_at(GEOMETRY, requested_at + Duration::from_secs(2))
            .unwrap();

        assert_eq!(observation, HighPerformanceObservation::Duplicate);
        assert!(gate.is_confirmed());
    }

    #[test]
    fn strict_startup_timeout_at_boundary_persists_failure() {
        let requested_at = Instant::now();
        let mut gate = HighPerformanceStartupGate::new(requested_at);

        let error = gate
            .ensure_not_timed_out(requested_at + Duration::from_secs(5))
            .unwrap_err();
        assert_eq!(error.code(), APPLE_HIGH_PERFORMANCE_UNAVAILABLE);
        assert_eq!(error.stage_code(), "hp05_confirmation_timeout");
        assert!(!gate.is_awaiting());
        assert!(!gate.is_confirmed());
        assert_eq!(
            gate.ensure_not_timed_out(requested_at + Duration::from_secs(6))
                .unwrap_err()
                .stage_code(),
            "hp05_confirmation_timeout"
        );
    }

    #[test]
    fn high_performance_diagnostic_formatter_exposes_only_closed_vocabulary_fields() {
        for (diagnostic, expected_stage) in [
            (HighPerformanceDiagnostic::SinkReady, "hp00_sink_ready"),
            (
                HighPerformanceDiagnostic::FactoryCreate,
                "hp00_factory_create",
            ),
            (
                HighPerformanceDiagnostic::TcpConnected,
                "hp00_tcp_connected",
            ),
            (
                HighPerformanceDiagnostic::RfbBannerAccepted,
                "hp00_rfb_banner_accepted",
            ),
            (
                HighPerformanceDiagnostic::SecurityOfferReceived,
                "hp00_security_offer_received",
            ),
            (
                HighPerformanceDiagnostic::SrpStep1Written,
                "hp00_srp_step1_written",
            ),
            (
                HighPerformanceDiagnostic::SrpChallengeAccepted,
                "hp00_srp_challenge_accepted",
            ),
            (
                HighPerformanceDiagnostic::SrpProofComputed,
                "hp00_srp_proof_computed",
            ),
            (
                HighPerformanceDiagnostic::SrpStep2Written,
                "hp00_srp_step2_written",
            ),
            (
                HighPerformanceDiagnostic::SrpResponseAccepted,
                "hp00_srp_response_accepted",
            ),
            (
                HighPerformanceDiagnostic::SrpAuthenticated,
                "hp00_srp_authenticated",
            ),
            (
                HighPerformanceDiagnostic::ClientInitWritten,
                "hp00_client_init_written",
            ),
            (
                HighPerformanceDiagnostic::ServerInitAccepted,
                "hp00_server_init_accepted",
            ),
            (
                HighPerformanceDiagnostic::EncryptionRequestWritten,
                "hp00_encryption_request_written",
            ),
            (
                HighPerformanceDiagnostic::EncryptionInfoAccepted,
                "hp00_encryption_info_accepted",
            ),
            (
                HighPerformanceDiagnostic::EncryptionActivated,
                "hp00_encryption_activated",
            ),
            (
                HighPerformanceDiagnostic::RuntimeHandoff,
                "hp00_runtime_handoff",
            ),
            (
                HighPerformanceDiagnostic::AuthenticationFailed,
                "hp10_authentication_failed",
            ),
        ] {
            assert_eq!(
                diagnostic.to_string(),
                format!("[apple-hp-stage] stage={expected_stage}")
            );
        }
        assert_eq!(
            HighPerformanceDiagnostic::StartupRequestWriteClosed {
                message: StartupRequestMessage::DisplayQuery,
            }
            .to_string(),
            "[apple-hp-stage] stage=hp04_startup_request_write_closed message=09"
        );
        assert_eq!(
            HighPerformanceDiagnostic::SetDisplayWritten {
                server_init_width: 1920,
                server_init_height: 1080,
                startup_width: 1920,
                startup_height: 1080,
            }
            .to_string(),
            "[apple-hp-stage] stage=hp00_set_display_written server_init_width=1920 server_init_height=1080 startup_width=1920 startup_height=1080"
        );
        assert_eq!(
            HighPerformanceDiagnostic::ServerStateAccepted {
                accepted_width: 1440,
                accepted_height: 2560,
                elapsed_ms: 371,
            }
            .to_string(),
            "[apple-hp-stage] stage=hp00_server_state_accepted accepted_width=1440 accepted_height=2560 elapsed_ms=371"
        );
    }

    #[test]
    fn strict_startup_observation_at_or_after_deadline_fails_after_predeadline_tick() {
        let requested_at = Instant::now();
        for observed_after_seconds in [5, 6] {
            let mut gate = HighPerformanceStartupGate::new(requested_at);
            gate.ensure_not_timed_out(requested_at + Duration::from_millis(4_990))
                .unwrap();

            let error = gate
                .observe_server_state_at(
                    GEOMETRY,
                    requested_at + Duration::from_secs(observed_after_seconds),
                )
                .unwrap_err();

            assert_eq!(error.code(), APPLE_HIGH_PERFORMANCE_UNAVAILABLE);
            assert_eq!(error.stage_code(), "hp05_confirmation_timeout");
            assert_eq!(
                gate.observe_server_state_at(
                    GEOMETRY,
                    requested_at + Duration::from_secs(observed_after_seconds + 1),
                )
                .unwrap_err()
                .stage_code(),
                "hp05_confirmation_timeout"
            );
        }
    }

    #[test]
    fn strict_startup_conflicting_direct_observation_fails() {
        let requested_at = Instant::now();
        let mut gate = HighPerformanceStartupGate::new(requested_at);
        gate.observe_server_state_at(GEOMETRY, requested_at + Duration::from_secs(1))
            .unwrap();

        let error = gate
            .observe_server_state_at(
                ServerStateGeometry {
                    message_version: 5,
                    display_count: 1,
                    width: 1920,
                    height: 1080,
                },
                requested_at + Duration::from_secs(2),
            )
            .unwrap_err();

        assert_eq!(error.code(), APPLE_HIGH_PERFORMANCE_UNAVAILABLE);
        assert_eq!(
            gate.ensure_not_timed_out(requested_at + Duration::from_secs(3))
                .unwrap_err()
                .stage_code(),
            "hp07_confirmation_malformed"
        );
    }

    #[test]
    fn strict_startup_zero_dimension_observation_fails_persistently() {
        let requested_at = Instant::now();
        for geometry in [
            ServerStateGeometry {
                message_version: 5,
                display_count: 1,
                width: 0,
                height: 2560,
            },
            ServerStateGeometry {
                message_version: 5,
                display_count: 1,
                width: 1440,
                height: 0,
            },
        ] {
            let mut gate = HighPerformanceStartupGate::new(requested_at);

            let error = gate
                .observe_server_state_at(geometry, requested_at + Duration::from_secs(1))
                .unwrap_err();

            assert_eq!(error.code(), APPLE_HIGH_PERFORMANCE_UNAVAILABLE);
            assert_eq!(
                gate.ensure_not_timed_out(requested_at + Duration::from_secs(2))
                    .unwrap_err()
                    .stage_code(),
                "hp07_confirmation_malformed"
            );
        }
    }

    #[test]
    fn strict_startup_unavailable_error_has_stable_code_and_survives_anyhow_downcast() {
        let unavailable =
            HighPerformanceUnavailable::diagnosed(HighPerformanceDiagnostic::ConfirmationMalformed);
        let error: Error = unavailable.into();

        assert_eq!(unavailable.code(), APPLE_HIGH_PERFORMANCE_UNAVAILABLE);
        assert_eq!(
            error.downcast_ref::<HighPerformanceUnavailable>(),
            Some(&unavailable)
        );
    }
}
