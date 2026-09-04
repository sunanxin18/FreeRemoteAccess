use std::time::{Duration, Instant};

use frd_core::SessionId;
use frd_media_api::VideoStreamIdentity;
use frd_protocol_api::SessionCommand;

use crate::video_decode_worker::VideoDecodeLoadSnapshot;

const OVERLOAD_LATENCY: Duration = Duration::from_millis(200);
const OVERLOAD_QUEUE_DEPTH: usize = 4;
const OVERLOAD_DURATION: Duration = Duration::from_secs(2);
const FALLBACK_MAX_FRAMES_PER_SECOND: u16 = 30;
const LOAD_OBSERVATION_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug)]
pub(crate) struct VideoRateObservation {
    pub session_id: SessionId,
    pub generation: u64,
    pub observed_at: Instant,
    pub smoothed_media_ingress_to_present: Duration,
    pub encoded_access_unit_queue_depth: usize,
    pub supports_fallback: bool,
}

#[derive(Default)]
pub(crate) struct VideoRateFallbackController {
    current: Option<GenerationState>,
    retired_session_id: Option<SessionId>,
    load_source: Option<RetainedLoadSource>,
    next_load_observation_at: Option<Instant>,
}

#[derive(Clone, Copy)]
struct RetainedLoadSource {
    identity: VideoStreamIdentity,
    generation: u64,
    smoothed_media_ingress_to_present: Option<Duration>,
}

struct GenerationState {
    session_id: SessionId,
    generation: u64,
    overloaded_since: Option<Instant>,
    fired: bool,
}

impl VideoRateFallbackController {
    pub(crate) fn retain_stream(
        &mut self,
        identity: VideoStreamIdentity,
        generation: u64,
        observed_at: Instant,
    ) {
        let session_id = identity.session_id;
        if self.retired_session_id == Some(session_id)
            || self
                .current
                .as_ref()
                .is_some_and(|state| state.session_id == session_id && state.fired)
        {
            return;
        }
        match self.current.as_mut() {
            Some(state) if state.session_id == session_id => {
                if state.generation != generation {
                    state.generation = generation;
                    state.overloaded_since = None;
                }
            }
            _ => {
                self.current = Some(GenerationState {
                    session_id,
                    generation,
                    overloaded_since: None,
                    fired: false,
                });
                self.retired_session_id = None;
            }
        }
        let source_changed = self
            .load_source
            .is_none_or(|source| source.identity != identity || source.generation != generation);
        if source_changed {
            self.load_source = Some(RetainedLoadSource {
                identity,
                generation,
                smoothed_media_ingress_to_present: None,
            });
            self.next_load_observation_at = Some(observed_at);
        }
    }

    pub(crate) fn retain_present_timing(
        &mut self,
        identity: VideoStreamIdentity,
        generation: u64,
        _presented_at: Instant,
        smoothed_media_ingress_to_present: Duration,
    ) {
        let Some(source) = self.load_source.as_mut() else {
            return;
        };
        if source.identity == identity && source.generation == generation {
            source.smoothed_media_ingress_to_present = Some(smoothed_media_ingress_to_present);
        }
    }

    pub(crate) fn clear_load_source(&mut self) {
        self.load_source = None;
        self.next_load_observation_at = None;
    }

    pub(crate) fn retire_session(&mut self, session_id: SessionId) {
        self.retired_session_id = Some(session_id);
        if self
            .load_source
            .is_some_and(|source| source.identity.session_id == session_id)
        {
            self.clear_load_source();
        }
        if let Some(state) = self
            .current
            .as_mut()
            .filter(|state| state.session_id == session_id)
        {
            state.overloaded_since = None;
        }
    }

    pub(crate) fn take_due_load_target(
        &mut self,
        observed_at: Instant,
        supports_fallback: bool,
    ) -> Option<(VideoStreamIdentity, u64)> {
        if !supports_fallback {
            return None;
        }
        let source = self.load_source?;
        let due_at = self.next_load_observation_at.get_or_insert(observed_at);
        if observed_at < *due_at {
            return None;
        }
        self.next_load_observation_at = Some(observed_at + LOAD_OBSERVATION_INTERVAL);
        Some((source.identity, source.generation))
    }

    pub(crate) fn next_load_observation_deadline(
        &self,
        supports_fallback: bool,
    ) -> Option<Instant> {
        supports_fallback
            .then_some(self.load_source?)
            .and(self.next_load_observation_at)
    }

    pub(crate) fn observe_load_snapshot(
        &mut self,
        observed_at: Instant,
        supports_fallback: bool,
        snapshot: Option<VideoDecodeLoadSnapshot>,
    ) -> Option<SessionCommand> {
        let source = self.load_source?;
        let snapshot = snapshot?;
        let oldest_access_unit_age = snapshot
            .oldest_local_ingress_at
            .map(|ingress_at| observed_at.saturating_duration_since(ingress_at))
            .unwrap_or_default();
        self.observe(VideoRateObservation {
            session_id: source.identity.session_id,
            generation: source.generation,
            observed_at,
            smoothed_media_ingress_to_present: source
                .smoothed_media_ingress_to_present
                .unwrap_or_default()
                .max(oldest_access_unit_age),
            encoded_access_unit_queue_depth: snapshot.queued_access_units,
            supports_fallback,
        })
    }

    #[cfg(test)]
    pub(crate) fn observe_available(
        &mut self,
        observation: Option<VideoRateObservation>,
    ) -> Option<SessionCommand> {
        observation.and_then(|observation| self.observe(observation))
    }

    pub(crate) fn observe(&mut self, observation: VideoRateObservation) -> Option<SessionCommand> {
        let state = self.current.get_or_insert_with(|| GenerationState {
            session_id: observation.session_id,
            generation: observation.generation,
            overloaded_since: None,
            fired: false,
        });
        if state.session_id != observation.session_id {
            *state = GenerationState {
                session_id: observation.session_id,
                generation: observation.generation,
                overloaded_since: None,
                fired: false,
            };
        } else if state.generation != observation.generation {
            state.generation = observation.generation;
            state.overloaded_since = None;
        }

        let overloaded = observation.supports_fallback
            && observation.smoothed_media_ingress_to_present > OVERLOAD_LATENCY
            && observation.encoded_access_unit_queue_depth >= OVERLOAD_QUEUE_DEPTH;
        if !overloaded {
            state.overloaded_since = None;
            return None;
        }
        if state.fired {
            return None;
        }
        let overloaded_since = state
            .overloaded_since
            .get_or_insert(observation.observed_at);
        if observation
            .observed_at
            .saturating_duration_since(*overloaded_since)
            < OVERLOAD_DURATION
        {
            return None;
        }
        state.fired = true;
        let command = SessionCommand::SetMaxSourceFrameRate {
            session_id: observation.session_id,
            generation: observation.generation,
            max_frames_per_second: FALLBACK_MAX_FRAMES_PER_SECOND,
        };
        self.clear_load_source();
        Some(command)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use frd_core::SessionId;
    use frd_media_api::VideoStreamIdentity;
    use frd_protocol_api::SessionCommand;

    use crate::video_decode_worker::VideoDecodeLoadSnapshot;

    use super::{VideoRateFallbackController, VideoRateObservation};

    fn observation(
        session_id: SessionId,
        generation: u64,
        observed_at: Instant,
        latency_ms: u64,
        queue_depth: usize,
        supports_fallback: bool,
    ) -> VideoRateObservation {
        VideoRateObservation {
            session_id,
            generation,
            observed_at,
            smoothed_media_ingress_to_present: Duration::from_millis(latency_ms),
            encoded_access_unit_queue_depth: queue_depth,
            supports_fallback,
        }
    }

    fn assert_30_hz_command(
        command: Option<SessionCommand>,
        expected_session_id: SessionId,
        expected_generation: u64,
    ) {
        assert!(matches!(
            command,
            Some(SessionCommand::SetMaxSourceFrameRate {
                session_id,
                generation,
                max_frames_per_second: 30,
            }) if session_id == expected_session_id && generation == expected_generation
        ));
    }

    #[test]
    fn fallback_requires_both_overload_signals_for_two_continuous_seconds() {
        let session_id = SessionId::allocate();
        let generation = 7;
        let origin = Instant::now();
        let mut controller = VideoRateFallbackController::default();

        assert!(controller
            .observe(observation(session_id, generation, origin, 201, 3, true,))
            .is_none());
        assert!(controller
            .observe(observation(
                session_id,
                generation,
                origin + Duration::from_secs(3),
                200,
                4,
                true,
            ))
            .is_none());
        let overload_origin = origin + Duration::from_secs(4);
        assert!(controller
            .observe(observation(
                session_id,
                generation,
                overload_origin,
                201,
                4,
                true,
            ))
            .is_none());
        assert!(controller
            .observe(observation(
                session_id,
                generation,
                overload_origin + Duration::from_millis(1_999),
                250,
                8,
                true,
            ))
            .is_none());
        assert_30_hz_command(
            controller.observe(observation(
                session_id,
                generation,
                overload_origin + Duration::from_secs(2),
                250,
                8,
                true,
            )),
            session_id,
            generation,
        );
    }

    #[test]
    fn transient_recovery_restarts_the_continuous_overload_interval() {
        let session_id = SessionId::allocate();
        let origin = Instant::now();
        let mut controller = VideoRateFallbackController::default();

        assert!(controller
            .observe(observation(session_id, 1, origin, 240, 5, true))
            .is_none());
        assert!(controller
            .observe(observation(
                session_id,
                1,
                origin + Duration::from_millis(1_500),
                180,
                5,
                true,
            ))
            .is_none());
        let restarted = origin + Duration::from_millis(1_600);
        assert!(controller
            .observe(observation(session_id, 1, restarted, 240, 5, true))
            .is_none());
        assert!(controller
            .observe(observation(
                session_id,
                1,
                restarted + Duration::from_millis(1_999),
                240,
                5,
                true,
            ))
            .is_none());
        assert_30_hz_command(
            controller.observe(observation(
                session_id,
                1,
                restarted + Duration::from_secs(2),
                240,
                5,
                true,
            )),
            session_id,
            1,
        );
    }

    #[test]
    fn fallback_fires_once_per_session_and_generation_changes_do_not_rearm_it() {
        let session_id = SessionId::allocate();
        let origin = Instant::now();
        let mut controller = VideoRateFallbackController::default();

        assert!(controller
            .observe(observation(session_id, 9, origin, 240, 5, true))
            .is_none());
        assert_30_hz_command(
            controller.observe(observation(
                session_id,
                9,
                origin + Duration::from_secs(2),
                240,
                5,
                true,
            )),
            session_id,
            9,
        );
        assert!(controller
            .observe(observation(
                session_id,
                9,
                origin + Duration::from_secs(20),
                50,
                0,
                true,
            ))
            .is_none());
        assert!(controller
            .observe(observation(
                session_id,
                9,
                origin + Duration::from_secs(21),
                240,
                5,
                true,
            ))
            .is_none());

        let next_generation = origin + Duration::from_secs(30);
        assert!(controller
            .observe(observation(session_id, 10, next_generation, 240, 5, true,))
            .is_none());
        assert!(controller
            .observe(observation(
                session_id,
                10,
                next_generation + Duration::from_secs(2),
                240,
                5,
                true,
            ))
            .is_none());

        let next_session = SessionId::allocate();
        let next_session_origin = origin + Duration::from_secs(40);
        assert!(controller
            .observe(observation(
                next_session,
                1,
                next_session_origin,
                240,
                5,
                true,
            ))
            .is_none());
        assert_30_hz_command(
            controller.observe(observation(
                next_session,
                1,
                next_session_origin + Duration::from_secs(2),
                240,
                5,
                true,
            )),
            next_session,
            1,
        );
    }

    #[test]
    fn unavailable_telemetry_skips_without_resetting_a_sustained_interval() {
        let session_id = SessionId::allocate();
        let origin = Instant::now();
        let mut controller = VideoRateFallbackController::default();

        assert!(controller
            .observe(observation(session_id, 3, origin, 240, 5, true))
            .is_none());
        assert!(controller.observe_available(None).is_none());
        assert_30_hz_command(
            controller.observe_available(Some(observation(
                session_id,
                3,
                origin + Duration::from_secs(2),
                240,
                5,
                true,
            ))),
            session_id,
            3,
        );
    }

    #[test]
    fn periodic_load_sampling_retains_present_timing_and_grows_with_oldest_au_age() {
        let session_id = SessionId::allocate();
        let identity = VideoStreamIdentity {
            session_id,
            stream_id: 1,
        };
        let origin = Instant::now();
        let mut controller = VideoRateFallbackController::default();
        controller.retain_stream(identity, 7, origin + Duration::from_millis(50));
        controller.retain_present_timing(
            identity,
            7,
            origin + Duration::from_millis(50),
            Duration::from_millis(80),
        );

        assert_eq!(
            controller.take_due_load_target(origin + Duration::from_millis(49), true),
            None
        );
        assert_eq!(
            controller.take_due_load_target(origin + Duration::from_millis(50), true),
            Some((identity, 7))
        );
        assert!(controller
            .observe_load_snapshot(
                origin + Duration::from_millis(50),
                true,
                Some(VideoDecodeLoadSnapshot {
                    queued_access_units: 0,
                    oldest_local_ingress_at: None,
                }),
            )
            .is_none());

        let overload_started = origin + Duration::from_millis(201);
        assert_eq!(
            controller.take_due_load_target(overload_started, true),
            Some((identity, 7))
        );
        let blocked = VideoDecodeLoadSnapshot {
            queued_access_units: 4,
            oldest_local_ingress_at: Some(origin),
        };
        assert!(controller
            .observe_load_snapshot(overload_started, true, Some(blocked))
            .is_none());
        assert!(controller
            .observe_load_snapshot(overload_started + Duration::from_secs(1), true, None,)
            .is_none());
        assert!(controller
            .observe_load_snapshot(
                overload_started + Duration::from_millis(1_999),
                true,
                Some(blocked),
            )
            .is_none());
        assert_30_hz_command(
            controller.observe_load_snapshot(
                overload_started + Duration::from_secs(2),
                true,
                Some(blocked),
            ),
            session_id,
            7,
        );
    }

    #[test]
    fn firing_disarms_the_periodic_load_source_and_deadline() {
        let session_id = SessionId::allocate();
        let identity = VideoStreamIdentity {
            session_id,
            stream_id: 1,
        };
        let origin = Instant::now();
        let mut controller = VideoRateFallbackController::default();
        controller.retain_stream(identity, 7, origin);
        let blocked = VideoDecodeLoadSnapshot {
            queued_access_units: 4,
            oldest_local_ingress_at: Some(origin),
        };

        assert!(controller
            .observe_load_snapshot(origin + Duration::from_millis(201), true, Some(blocked))
            .is_none());
        assert_30_hz_command(
            controller.observe_load_snapshot(
                origin + Duration::from_millis(2_201),
                true,
                Some(blocked),
            ),
            session_id,
            7,
        );
        assert_eq!(controller.next_load_observation_deadline(true), None);
        assert_eq!(
            controller.take_due_load_target(origin + Duration::from_secs(30), true),
            None
        );
        controller.retain_stream(identity, 8, origin + Duration::from_secs(31));
        assert_eq!(controller.next_load_observation_deadline(true), None);
        assert_eq!(
            controller.take_due_load_target(origin + Duration::from_secs(31), true),
            None
        );
    }

    #[test]
    fn retirement_rejects_late_old_admission_even_after_support_returns() {
        let old_identity = VideoStreamIdentity {
            session_id: SessionId::allocate(),
            stream_id: 1,
        };
        let new_identity = VideoStreamIdentity {
            session_id: SessionId::allocate(),
            stream_id: 1,
        };
        let origin = Instant::now();
        let mut controller = VideoRateFallbackController::default();
        controller.retain_stream(old_identity, 7, origin);
        assert_eq!(
            controller.next_load_observation_deadline(true),
            Some(origin)
        );

        controller.retire_session(old_identity.session_id);
        assert_eq!(controller.next_load_observation_deadline(true), None);
        assert_eq!(
            controller.take_due_load_target(origin + Duration::from_secs(10), true),
            None
        );
        controller.retain_stream(old_identity, 8, origin + Duration::from_secs(10));
        assert_eq!(controller.next_load_observation_deadline(true), None);
        assert_eq!(
            controller.take_due_load_target(origin + Duration::from_secs(10), true),
            None
        );
        controller.retain_present_timing(
            new_identity,
            1,
            origin + Duration::from_secs(11),
            Duration::from_millis(50),
        );
        assert_eq!(controller.next_load_observation_deadline(true), None);

        let admitted_at = origin + Duration::from_secs(12);
        controller.retain_stream(new_identity, 1, admitted_at);
        assert_eq!(
            controller.next_load_observation_deadline(true),
            Some(admitted_at)
        );
        assert_eq!(
            controller.take_due_load_target(admitted_at, true),
            Some((new_identity, 1))
        );
    }

    #[test]
    fn different_session_admission_clears_retirement_and_rearms() {
        let old_session_id = SessionId::allocate();
        let new_identity = VideoStreamIdentity {
            session_id: SessionId::allocate(),
            stream_id: 1,
        };
        let origin = Instant::now();
        let admitted_at = origin + Duration::from_secs(1);
        let mut controller = VideoRateFallbackController::default();
        controller.retire_session(old_session_id);

        controller.retain_stream(new_identity, 1, admitted_at);
        assert_eq!(
            controller.next_load_observation_deadline(true),
            Some(admitted_at)
        );
        assert_eq!(
            controller.take_due_load_target(admitted_at, true),
            Some((new_identity, 1))
        );
    }

    #[test]
    fn unsupported_protocol_observations_never_emit_a_rate_command() {
        let session_id = SessionId::allocate();
        let origin = Instant::now();
        let mut controller = VideoRateFallbackController::default();

        assert!(controller
            .observe(observation(session_id, 1, origin, 300, 8, false))
            .is_none());
        assert!(controller
            .observe(observation(
                session_id,
                1,
                origin + Duration::from_secs(30),
                300,
                8,
                false,
            ))
            .is_none());
    }
}
