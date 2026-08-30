use std::time::{Duration, Instant};

use frd_core::SessionId;
use frd_frame::EnqueuedSurfaceUpdate;
use frd_protocol_api::FrameResponseTiming;
use frd_render_wgpu::{ApplyOutcome, GpuScopeObservation, RendererError};

use crate::frame_metrics_sink::{
    duration_us, FrameMetricsSink, MetricBatchFailureClass, MetricBatchResult, MetricEventKind,
    MetricPhase, MetricSinkError,
};

pub(crate) fn checked_mailbox_age(
    apply_or_drain_started_at: Instant,
    enqueued_at: Instant,
) -> Option<Duration> {
    apply_or_drain_started_at.checked_duration_since(enqueued_at)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MetricIdentity {
    pub(crate) session_id: SessionId,
    pub(crate) generation: u64,
}

struct InputToNextPresentProbe {
    identity: MetricIdentity,
    accepted_at: Instant,
}

struct MetricPhaseState {
    phase: MetricPhase,
    run_started_at: Instant,
    phase_started_at: Instant,
}

pub(crate) struct FramePipelineMetrics {
    sink: Option<FrameMetricsSink>,
    active_identity: Option<MetricIdentity>,
    pending_input: Option<InputToNextPresentProbe>,
    last_presented: Option<(MetricIdentity, Instant)>,
    run_phase: Option<MetricPhaseState>,
    failure: Option<MetricSinkError>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BatchMetricContext {
    pub(crate) batch_started_at: Instant,
    pub(crate) source_update_count: usize,
    pub(crate) oldest_age: Option<Duration>,
    pub(crate) transaction_count: usize,
}

pub(crate) struct DrainedFrameUpdates {
    pub(crate) updates: Vec<EnqueuedSurfaceUpdate>,
    pub(crate) drain_started_at: Instant,
    pub(crate) oldest_age: Duration,
}

impl DrainedFrameUpdates {
    pub(crate) fn new(
        updates: Vec<EnqueuedSurfaceUpdate>,
        drain_started_at: Instant,
    ) -> Result<Self, MetricSinkError> {
        let earliest = updates
            .iter()
            .map(|entry| entry.enqueued_at)
            .min()
            .ok_or(MetricSinkError::InvalidObservation)?;
        let oldest_age = checked_mailbox_age(drain_started_at, earliest)
            .ok_or(MetricSinkError::InvalidObservation)?;
        Ok(Self {
            updates,
            drain_started_at,
            oldest_age,
        })
    }
}

#[derive(Default)]
pub(crate) struct SerialDrainAggregate {
    pub(crate) successful_updates: usize,
    pub(crate) uploaded_rectangles: usize,
    pub(crate) scope: GpuScopeObservation,
}

impl SerialDrainAggregate {
    pub(crate) fn observe(
        &mut self,
        outcome: ApplyOutcome,
        actual_delta: GpuScopeObservation,
    ) -> Result<(), MetricSinkError> {
        self.observe_scope(actual_delta)?;
        self.successful_updates = self
            .successful_updates
            .checked_add(1)
            .ok_or(MetricSinkError::InvalidObservation)?;
        if let ApplyOutcome::Damage {
            uploaded_rectangles,
        } = outcome
        {
            self.uploaded_rectangles = self
                .uploaded_rectangles
                .checked_add(uploaded_rectangles)
                .ok_or(MetricSinkError::InvalidObservation)?;
        }
        Ok(())
    }

    pub(crate) fn observe_failed_scope(
        &mut self,
        actual_delta: GpuScopeObservation,
    ) -> Result<(), MetricSinkError> {
        self.observe_scope(actual_delta)
    }

    fn observe_scope(&mut self, actual_delta: GpuScopeObservation) -> Result<(), MetricSinkError> {
        self.scope.begins = self
            .scope
            .begins
            .checked_add(actual_delta.begins)
            .ok_or(MetricSinkError::InvalidObservation)?;
        self.scope.finishes = self
            .scope
            .finishes
            .checked_add(actual_delta.finishes)
            .ok_or(MetricSinkError::InvalidObservation)?;
        self.scope.polls = self
            .scope
            .polls
            .checked_add(actual_delta.polls)
            .ok_or(MetricSinkError::InvalidObservation)?;
        Ok(())
    }
}

impl FramePipelineMetrics {
    pub(crate) fn from_environment(started_at: Instant) -> Result<Self, MetricSinkError> {
        Ok(Self::new(FrameMetricsSink::open_from_environment(
            started_at,
        )?))
    }

    fn new(sink: Option<FrameMetricsSink>) -> Self {
        Self {
            sink,
            active_identity: None,
            pending_input: None,
            last_presented: None,
            run_phase: None,
            failure: None,
        }
    }

    pub(crate) fn disabled() -> Self {
        Self::new(None)
    }

    #[cfg(test)]
    fn test_sink(path: std::path::PathBuf, started_at: Instant) -> Result<Self, MetricSinkError> {
        Ok(Self::new(Some(FrameMetricsSink::open_for_test(
            path, started_at,
        )?)))
    }

    pub(crate) fn begin_session(&mut self, session_id: SessionId) {
        self.pending_input = None;
        self.active_identity = Some(MetricIdentity {
            session_id,
            generation: 0,
        });
    }

    pub(crate) fn observe_generation(&mut self, session_id: SessionId, generation: u64) {
        let identity = MetricIdentity {
            session_id,
            generation,
        };
        if self.active_identity != Some(identity) {
            self.pending_input = None;
            self.last_presented = None;
        }
        self.active_identity = Some(identity);
    }

    pub(crate) fn observe_input_sent(&mut self, identity: MetricIdentity, accepted_at: Instant) {
        if self.active_identity == Some(identity) && self.pending_input.is_none() {
            self.pending_input = Some(InputToNextPresentProbe {
                identity,
                accepted_at,
            });
        }
    }

    pub(crate) fn observe_presented(
        &mut self,
        identity: MetricIdentity,
        revision: u64,
        at: Instant,
    ) {
        if self.active_identity != Some(identity) {
            return;
        }
        if let Some(phase) = self.current_phase() {
            let mut row = match self.sink.as_ref() {
                Some(sink) => sink.new_row(phase, MetricEventKind::Presentation),
                None => {
                    self.complete_input_probe(identity, revision, at);
                    self.last_presented = Some((identity, at));
                    return;
                }
            };
            row.session_id = Some(identity.session_id.get());
            row.generation = Some(identity.generation);
            row.revision = Some(revision);
            self.write(row);
        }
        self.complete_input_probe(identity, revision, at);
        self.last_presented = Some((identity, at));
        if self.current_phase() == Some(MetricPhase::Restore) {
            self.close();
        }
    }

    pub(crate) fn observe_full_baseline_presented(
        &mut self,
        identity: MetricIdentity,
        revision: u64,
        at: Instant,
    ) {
        if self.active_identity == Some(identity) && self.run_phase.is_none() && self.sink.is_some()
        {
            self.set_phase(MetricPhase::VisibleWarmup, at, at);
        }
        self.observe_presented(identity, revision, at);
    }

    pub(crate) fn observe_frame_response_timing(&mut self, timing: FrameResponseTiming) {
        let Some(identity) = self.active_identity else {
            return;
        };
        if timing.generation != identity.generation {
            return;
        }
        let Some(phase) = self.current_phase() else {
            return;
        };
        let Some(sink) = self.sink.as_ref() else {
            return;
        };
        let mut row = sink.new_row(phase, MetricEventKind::FrameResponse);
        row.session_id = Some(identity.session_id.get());
        row.generation = Some(identity.generation);
        row.frame_response_ms = Some(u64::from(timing.sample_ms));
        self.write(row);
    }

    pub(crate) fn observe_serial_drain(
        &mut self,
        context: BatchMetricContext,
        aggregate: &SerialDrainAggregate,
        error: Option<RendererError>,
        completed_at: Instant,
    ) {
        let Some(phase) = self.current_phase() else {
            return;
        };
        let Some(sink) = self.sink.as_ref() else {
            return;
        };
        let mut row = sink.new_row(phase, MetricEventKind::SerialDrain);
        if let Some(identity) = self.active_identity {
            row.session_id = Some(identity.session_id.get());
            row.generation = Some(identity.generation);
        }
        row.source_updates = u64::try_from(context.source_update_count).ok();
        row.transactions = u64::try_from(context.transaction_count).ok();
        row.rectangles = u64::try_from(aggregate.uploaded_rectangles).ok();
        row.batch_cpu_us = completed_at
            .checked_duration_since(context.batch_started_at)
            .map(duration_us);
        row.mailbox_age_us = context.oldest_age.map(duration_us);
        row.scope_begins = Some(aggregate.scope.begins);
        row.scope_finishes = Some(aggregate.scope.finishes);
        row.scope_polls = Some(aggregate.scope.polls);
        match error {
            None => row.batch_result = Some(MetricBatchResult::Success),
            Some(error) => {
                row.batch_result = Some(MetricBatchResult::SerialFailure);
                if let RendererError::GpuFault(fault) = error {
                    row.batch_failure_class = Some(MetricBatchFailureClass::Gpu);
                    row.gpu_fault_code = Some(fault);
                } else {
                    row.batch_failure_class = Some(MetricBatchFailureClass::RendererExecution);
                }
            }
        }
        if row.source_updates.is_none()
            || row.transactions.is_none()
            || row.rectangles.is_none()
            || row.batch_cpu_us.is_none()
            || row.mailbox_age_us.is_none()
        {
            self.record_failure(MetricSinkError::InvalidObservation);
            return;
        }
        self.write(row);
    }

    pub(crate) fn clear_input_probe(&mut self) {
        self.pending_input = None;
    }

    pub(crate) fn detach(&mut self) {
        self.pending_input = None;
        self.last_presented = None;
        self.active_identity = None;
    }

    pub(crate) fn close(&mut self) {
        self.pending_input = None;
        if let Some(mut sink) = self.sink.take() {
            if let Err(error) = sink.close() {
                self.failure.get_or_insert(error);
            }
        }
        self.run_phase = None;
    }

    pub(crate) fn fatal(&mut self) {
        self.pending_input = None;
        self.close();
    }

    pub(crate) fn observe_occluded(&mut self, occluded: bool, at: Instant) {
        self.observe_window_minimized(occluded, at);
    }

    pub(crate) fn observe_window_minimized(&mut self, minimized: bool, at: Instant) {
        let Some(state) = self.run_phase.as_ref() else {
            return;
        };
        let next = if minimized
            && state.phase == MetricPhase::VisibleMeasurement
            && at.saturating_duration_since(state.phase_started_at) >= Duration::from_secs(30)
        {
            Some(MetricPhase::MinimizedWarmup)
        } else if !minimized
            && state.phase == MetricPhase::MinimizedMeasurement
            && at.saturating_duration_since(state.phase_started_at) >= Duration::from_secs(30)
        {
            Some(MetricPhase::Restore)
        } else {
            None
        };
        if let Some(next) = next {
            let run_started_at = state.run_started_at;
            self.set_phase(next, run_started_at, at);
        }
    }

    pub(crate) fn advance_phase(&mut self, at: Instant) {
        let Some(state) = self.run_phase.as_ref() else {
            return;
        };
        let next = match state.phase {
            MetricPhase::VisibleWarmup
                if at.saturating_duration_since(state.phase_started_at)
                    >= Duration::from_secs(5) =>
            {
                Some(MetricPhase::VisibleMeasurement)
            }
            MetricPhase::MinimizedWarmup
                if at.saturating_duration_since(state.phase_started_at)
                    >= Duration::from_secs(5) =>
            {
                Some(MetricPhase::MinimizedMeasurement)
            }
            _ => None,
        };
        if let Some(next) = next {
            let run_started_at = state.run_started_at;
            self.set_phase(next, run_started_at, at);
        }
    }

    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let state = self.run_phase.as_ref()?;
        let duration = match state.phase {
            MetricPhase::VisibleWarmup | MetricPhase::MinimizedWarmup => Duration::from_secs(5),
            MetricPhase::VisibleMeasurement | MetricPhase::MinimizedMeasurement => {
                Duration::from_secs(30)
            }
            MetricPhase::Restore => return None,
        };
        let deadline = state.phase_started_at + duration;
        (deadline > now).then_some(deadline)
    }

    pub(crate) fn take_failure(&mut self) -> Option<MetricSinkError> {
        self.failure.take()
    }

    pub(crate) fn invalidate(&mut self, error: MetricSinkError) {
        self.record_failure(error);
    }

    fn current_phase(&self) -> Option<MetricPhase> {
        self.run_phase.as_ref().map(|state| state.phase)
    }

    fn set_phase(&mut self, phase: MetricPhase, run_started_at: Instant, at: Instant) {
        self.run_phase = Some(MetricPhaseState {
            phase,
            run_started_at,
            phase_started_at: at,
        });
        let Some(sink) = self.sink.as_ref() else {
            return;
        };
        let row = sink.new_row(phase, MetricEventKind::PhaseBoundary);
        self.write(row);
    }

    fn complete_input_probe(&mut self, identity: MetricIdentity, _revision: u64, at: Instant) {
        let Some(probe) = self.pending_input.as_ref() else {
            return;
        };
        if probe.identity != identity {
            return;
        }
        let accepted_at = probe.accepted_at;
        self.pending_input = None;
        let Some(duration) = at.checked_duration_since(accepted_at) else {
            self.record_failure(MetricSinkError::InvalidObservation);
            return;
        };
        let Some(phase) = self.current_phase() else {
            return;
        };
        let Some(sink) = self.sink.as_ref() else {
            return;
        };
        let mut row = sink.new_row(phase, MetricEventKind::InputToNextPresent);
        row.session_id = Some(identity.session_id.get());
        row.generation = Some(identity.generation);
        row.input_to_next_present_us = Some(duration_us(duration));
        self.write(row);
    }

    fn write(&mut self, row: crate::frame_metrics_sink::FrameMetricRow) {
        let result = self
            .sink
            .as_mut()
            .expect("row construction requires an enabled sink")
            .write_row(row);
        if let Err(error) = result {
            self.record_failure(error);
        }
    }

    fn record_failure(&mut self, error: MetricSinkError) {
        self.failure.get_or_insert(error);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use frd_core::{PixelSize, SessionId};
    use frd_frame::{EnqueuedSurfaceUpdate, PixelFormat, SurfaceUpdate};
    use frd_protocol_api::FrameResponseTiming;
    use frd_render_wgpu::{ApplyOutcome, GpuScopeObservation};

    use crate::frame_metrics_sink::MetricPhase;

    use super::{
        checked_mailbox_age, BatchMetricContext, FramePipelineMetrics, MetricIdentity,
        MetricPhaseState, SerialDrainAggregate,
    };

    fn metrics_in_phase(
        label: &str,
        phase: MetricPhase,
        phase_started_at: Instant,
    ) -> (FramePipelineMetrics, std::path::PathBuf, std::path::PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("frd-window-phase-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();
        let path = directory.join("events.csv");
        let mut metrics = FramePipelineMetrics::test_sink(path.clone(), phase_started_at).unwrap();
        metrics.run_phase = Some(MetricPhaseState {
            phase,
            run_started_at: phase_started_at,
            phase_started_at,
        });
        (metrics, directory, path)
    }

    fn finish_phase_test(
        mut metrics: FramePipelineMetrics,
        directory: std::path::PathBuf,
        path: std::path::PathBuf,
    ) -> String {
        metrics.close();
        let output = fs::read_to_string(path).unwrap();
        fs::remove_dir_all(directory).unwrap();
        output
    }

    #[test]
    fn windows_minimized_resize_evidence_transitions_completed_visible_measurement() {
        let started_at = Instant::now();
        let observed_at = started_at + Duration::from_secs(30);
        let (mut metrics, directory, path) =
            metrics_in_phase("minimized", MetricPhase::VisibleMeasurement, started_at);

        metrics.observe_window_minimized(true, observed_at);
        metrics.observe_window_minimized(true, observed_at + Duration::from_secs(1));

        assert_eq!(metrics.current_phase(), Some(MetricPhase::MinimizedWarmup));
        assert_eq!(
            metrics.run_phase.as_ref().unwrap().phase_started_at,
            observed_at
        );
        let output = finish_phase_test(metrics, directory, path);
        assert_eq!(
            output
                .lines()
                .filter(|line| line.contains(",MinimizedWarmup,PhaseBoundary,"))
                .count(),
            1
        );
    }

    #[test]
    fn windows_restored_resize_evidence_transitions_completed_minimized_measurement() {
        let started_at = Instant::now();
        let observed_at = started_at + Duration::from_secs(30);
        let (mut metrics, directory, path) =
            metrics_in_phase("restored", MetricPhase::MinimizedMeasurement, started_at);

        metrics.observe_window_minimized(false, observed_at);
        metrics.observe_window_minimized(false, observed_at + Duration::from_secs(1));

        assert_eq!(metrics.current_phase(), Some(MetricPhase::Restore));
        assert_eq!(
            metrics.run_phase.as_ref().unwrap().phase_started_at,
            observed_at
        );
        let output = finish_phase_test(metrics, directory, path);
        assert_eq!(
            output
                .lines()
                .filter(|line| line.contains(",Restore,PhaseBoundary,"))
                .count(),
            1
        );
    }

    #[test]
    fn windows_visible_nonzero_resize_evidence_cannot_skip_phases() {
        let started_at = Instant::now();
        let observed_at = started_at + Duration::from_secs(30);
        let (mut metrics, directory, path) = metrics_in_phase(
            "visible-resize",
            MetricPhase::VisibleMeasurement,
            started_at,
        );

        metrics.observe_window_minimized(false, observed_at);

        assert_eq!(
            metrics.current_phase(),
            Some(MetricPhase::VisibleMeasurement)
        );
        assert_eq!(
            metrics.run_phase.as_ref().unwrap().phase_started_at,
            started_at
        );
        let output = finish_phase_test(metrics, directory, path);
        assert_eq!(
            output
                .lines()
                .filter(|line| line.contains(",PhaseBoundary,"))
                .count(),
            0
        );
    }

    #[test]
    fn mailbox_age_returns_none_when_observation_precedes_enqueue_time() {
        let enqueued_at = Instant::now();
        let observed_at = enqueued_at + Duration::from_millis(17);
        assert_eq!(
            checked_mailbox_age(observed_at, enqueued_at),
            Some(Duration::from_millis(17))
        );
        assert_eq!(checked_mailbox_age(enqueued_at, observed_at), None);
    }

    #[test]
    fn serial_drain_age_uses_earliest_envelope_after_unlock() {
        let session_id = SessionId::allocate();
        let now = Instant::now();
        let earlier = now - Duration::from_millis(40);
        let later = now - Duration::from_millis(10);
        let envelopes = vec![
            EnqueuedSurfaceUpdate {
                enqueued_at: later,
                update: SurfaceUpdate::Reset {
                    session_id,
                    generation: 2,
                    size: PixelSize::new(4, 3).unwrap(),
                    format: PixelFormat::Bgra8UnormSrgb,
                },
            },
            EnqueuedSurfaceUpdate {
                enqueued_at: earlier,
                update: SurfaceUpdate::FrameBoundary {
                    session_id,
                    generation: 2,
                    revision: 1,
                    completeness: frd_frame::FrameCompleteness::FullBaseline,
                },
            },
        ];
        let drain_started_at = Instant::now();
        let drained = super::DrainedFrameUpdates::new(envelopes, drain_started_at).unwrap();
        assert_eq!(drained.oldest_age, drain_started_at.duration_since(earlier));
    }

    #[test]
    fn serial_nonempty_drain_emits_one_aggregate_row() {
        let directory =
            std::env::temp_dir().join(format!("frd-serial-drain-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();
        let path = directory.join("events.csv");
        let started_at = Instant::now();
        let mut metrics = FramePipelineMetrics::test_sink(path.clone(), started_at).unwrap();
        let session_id = SessionId::allocate();
        let identity = MetricIdentity {
            session_id,
            generation: 1,
        };
        metrics.begin_session(session_id);
        metrics.observe_generation(session_id, 1);
        metrics.observe_full_baseline_presented(identity, 1, started_at);
        let mut aggregate = SerialDrainAggregate::default();
        for outcome in [
            ApplyOutcome::Reset,
            ApplyOutcome::Damage {
                uploaded_rectangles: 2,
            },
            ApplyOutcome::BoundaryPending(frd_render_wgpu::PresentationReceipt {
                session_id,
                generation: 1,
                revision: 1,
                completeness: frd_frame::FrameCompleteness::FullBaseline,
            }),
        ] {
            aggregate
                .observe(
                    outcome,
                    GpuScopeObservation {
                        begins: 1,
                        finishes: 1,
                        polls: 1,
                    },
                )
                .unwrap();
        }
        assert_eq!(aggregate.successful_updates, 3);
        assert_eq!(aggregate.uploaded_rectangles, 2);
        assert_eq!(
            aggregate.scope,
            GpuScopeObservation {
                begins: 3,
                finishes: 3,
                polls: 3
            }
        );
        metrics.observe_serial_drain(
            BatchMetricContext {
                batch_started_at: started_at,
                source_update_count: 3,
                oldest_age: Some(Duration::from_millis(4)),
                transaction_count: 0,
            },
            &aggregate,
            None,
            started_at + Duration::from_millis(7),
        );
        metrics.close();
        let output = fs::read_to_string(path).unwrap();
        let rows = output
            .lines()
            .filter(|line| line.contains(",SerialDrain,"))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        let fields = rows[0].split(',').collect::<Vec<_>>();
        assert_eq!(fields[5], "Success");
        assert_eq!(fields[13], "2");
        assert_eq!(&fields[16..=18], &["3", "3", "3"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn input_to_next_present_requires_same_session_and_generation_and_clears_on_boundaries() {
        let session_a = SessionId::allocate();
        let session_b = SessionId::allocate();
        let identity = MetricIdentity {
            session_id: session_a,
            generation: 7,
        };
        let at = Instant::now();
        let mut metrics = FramePipelineMetrics::disabled();
        metrics.begin_session(session_a);
        metrics.observe_generation(session_a, 7);
        metrics.observe_input_sent(identity, at);
        metrics.observe_presented(
            MetricIdentity {
                session_id: session_b,
                generation: 7,
            },
            1,
            at + Duration::from_millis(1),
        );
        assert!(metrics.pending_input.is_some());
        metrics.observe_presented(
            MetricIdentity {
                session_id: session_a,
                generation: 8,
            },
            1,
            at + Duration::from_millis(2),
        );
        assert!(metrics.pending_input.is_some());
        metrics.observe_presented(identity, 1, at + Duration::from_millis(3));
        assert!(metrics.pending_input.is_none());

        for clear in 0..6 {
            metrics.observe_generation(session_a, 7);
            metrics.observe_input_sent(identity, at);
            match clear {
                0 => metrics.clear_input_probe(),
                1 => metrics.detach(),
                2 => metrics.close(),
                3 => metrics.fatal(),
                4 => metrics.observe_generation(session_a, 8),
                5 => metrics.begin_session(session_b),
                _ => unreachable!(),
            }
            assert!(
                metrics.pending_input.is_none(),
                "boundary {clear} must clear probe"
            );
        }
    }

    #[test]
    fn frame_response_metric_uses_sample_not_ui_smoothed_value() {
        let directory =
            std::env::temp_dir().join(format!("frd-frame-response-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();
        let path = directory.join("events.csv");
        let mut metrics = FramePipelineMetrics::test_sink(path.clone(), Instant::now()).unwrap();
        let session_id = SessionId::allocate();
        metrics.begin_session(session_id);
        metrics.observe_generation(session_id, 3);
        metrics.observe_full_baseline_presented(
            MetricIdentity {
                session_id,
                generation: 3,
            },
            1,
            Instant::now(),
        );
        metrics.observe_frame_response_timing(FrameResponseTiming {
            generation: 3,
            sample_ms: 17,
            smoothed_ms: 83,
        });
        metrics.close();
        let output = fs::read_to_string(path).unwrap();
        let row = output
            .lines()
            .find(|line| line.contains(",FrameResponse,"))
            .unwrap();
        assert_eq!(row.split(',').nth(23), Some("17"));
        assert!(!row.contains(",83,"));
        fs::remove_dir_all(directory).unwrap();
    }
}
