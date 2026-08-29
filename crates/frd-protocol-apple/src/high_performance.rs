#![allow(dead_code)] // 由后续 Apple 读取器任务接入。

use std::fmt;
use std::time::{Duration, Instant};

use crate::dynamic_resolution::DisplaySize;
use crate::hpss::ServerStateGeometry;

pub(crate) const APPLE_HIGH_PERFORMANCE_UNAVAILABLE: &str = "apple_high_performance_unavailable";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HighPerformanceUnavailable;

impl HighPerformanceUnavailable {
    pub(crate) const fn code(self) -> &'static str {
        APPLE_HIGH_PERFORMANCE_UNAVAILABLE
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
    Failed,
}

pub(crate) struct HighPerformanceStartupGate {
    deadline: Instant,
    state: HighPerformanceStartupState,
}

impl HighPerformanceStartupGate {
    pub(crate) fn new(requested_at: Instant) -> Self {
        Self {
            deadline: requested_at + Duration::from_secs(5),
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
            HighPerformanceStartupState::Awaiting | HighPerformanceStartupState::Failed => None,
        }
    }

    pub(crate) fn ensure_not_timed_out(
        &mut self,
        now: Instant,
    ) -> Result<(), HighPerformanceUnavailable> {
        match self.state {
            HighPerformanceStartupState::Failed => Err(HighPerformanceUnavailable),
            HighPerformanceStartupState::Confirmed(_) => Ok(()),
            HighPerformanceStartupState::Awaiting if now < self.deadline => Ok(()),
            HighPerformanceStartupState::Awaiting => self.fail(),
        }
    }

    pub(crate) fn observe_server_state_at(
        &mut self,
        geometry: ServerStateGeometry,
        observed_at: Instant,
    ) -> Result<HighPerformanceObservation, HighPerformanceUnavailable> {
        if matches!(self.state, HighPerformanceStartupState::Failed) {
            return Err(HighPerformanceUnavailable);
        }

        let Some(size) = DisplaySize::new(geometry.width, geometry.height) else {
            return self.fail();
        };
        let confirmation = HighPerformanceConfirmation { size };

        match self.state {
            HighPerformanceStartupState::Failed => Err(HighPerformanceUnavailable),
            HighPerformanceStartupState::Confirmed(existing) if existing == confirmation => {
                Ok(HighPerformanceObservation::Duplicate)
            }
            HighPerformanceStartupState::Confirmed(_) => self.fail(),
            HighPerformanceStartupState::Awaiting if observed_at < self.deadline => {
                self.state = HighPerformanceStartupState::Confirmed(confirmation);
                Ok(HighPerformanceObservation::Confirmed(confirmation))
            }
            HighPerformanceStartupState::Awaiting => self.fail(),
        }
    }

    fn fail<T>(&mut self) -> Result<T, HighPerformanceUnavailable> {
        self.state = HighPerformanceStartupState::Failed;
        Err(HighPerformanceUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use anyhow::Error;

    use super::{
        HighPerformanceObservation, HighPerformanceStartupGate, HighPerformanceUnavailable,
        APPLE_HIGH_PERFORMANCE_UNAVAILABLE,
    };
    use crate::dynamic_resolution::DisplaySize;
    use crate::hpss::ServerStateGeometry;

    const GEOMETRY: ServerStateGeometry = ServerStateGeometry {
        record_count: 5,
        width: 1440,
        height: 2560,
    };

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

        assert_eq!(
            observation,
            HighPerformanceObservation::Confirmed(super::HighPerformanceConfirmation {
                size: DisplaySize::new(1440, 2560).unwrap(),
            })
        );
        assert_eq!(
            gate.confirmation(),
            Some(super::HighPerformanceConfirmation {
                size: DisplaySize::new(1440, 2560).unwrap(),
            })
        );
        assert!(!gate.is_awaiting());
        assert!(gate.is_confirmed());
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
        assert!(!gate.is_awaiting());
        assert!(!gate.is_confirmed());
        assert_eq!(
            gate.ensure_not_timed_out(requested_at + Duration::from_secs(6))
                .unwrap_err(),
            HighPerformanceUnavailable
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
            assert_eq!(
                gate.observe_server_state_at(
                    GEOMETRY,
                    requested_at + Duration::from_secs(observed_after_seconds + 1),
                )
                .unwrap_err(),
                HighPerformanceUnavailable
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
                    record_count: 5,
                    width: 1920,
                    height: 1080,
                },
                requested_at + Duration::from_secs(2),
            )
            .unwrap_err();

        assert_eq!(error.code(), APPLE_HIGH_PERFORMANCE_UNAVAILABLE);
        assert_eq!(
            gate.ensure_not_timed_out(requested_at + Duration::from_secs(3))
                .unwrap_err(),
            HighPerformanceUnavailable
        );
    }

    #[test]
    fn strict_startup_unavailable_error_has_stable_code_and_survives_anyhow_downcast() {
        let unavailable = HighPerformanceUnavailable;
        let error: Error = unavailable.into();

        assert_eq!(
            HighPerformanceUnavailable.code(),
            APPLE_HIGH_PERFORMANCE_UNAVAILABLE
        );
        assert_eq!(
            error.downcast_ref::<HighPerformanceUnavailable>(),
            Some(&HighPerformanceUnavailable)
        );
    }
}
