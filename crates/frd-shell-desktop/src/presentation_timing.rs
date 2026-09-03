use std::time::{Duration, Instant};

use frd_media_api::VideoStreamIdentity;
use frd_protocol_api::{PresentationTiming, PresentationTimingSource};

const TITLE_PUBLICATION_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PresentationTimingKey {
    pub(crate) identity: VideoStreamIdentity,
    pub(crate) generation: u64,
    pub(crate) worker_epoch_serial: u64,
}

struct PresentationTimingState {
    key: PresentationTimingKey,
    smoothed_ms: u32,
    last_published_at: Instant,
}

#[derive(Default)]
pub(crate) struct PresentationTimingTracker {
    state: Option<PresentationTimingState>,
}

impl PresentationTimingTracker {
    pub(crate) fn observe(
        &mut self,
        key: PresentationTimingKey,
        local_ingress_at: Instant,
        presented_at: Instant,
    ) -> Option<PresentationTiming> {
        let sample_ms = duration_ms(presented_at.saturating_duration_since(local_ingress_at));
        let is_new_source = self.state.as_ref().is_none_or(|state| state.key != key);
        if is_new_source {
            self.state = Some(PresentationTimingState {
                key,
                smoothed_ms: sample_ms,
                last_published_at: presented_at,
            });
            return Some(timing(key, sample_ms, sample_ms));
        }

        let state = self.state.as_mut().expect("matching timing state exists");
        state.smoothed_ms = ewma(state.smoothed_ms, sample_ms);
        if presented_at.saturating_duration_since(state.last_published_at)
            < TITLE_PUBLICATION_INTERVAL
        {
            return None;
        }
        state.last_published_at = presented_at;
        Some(timing(key, sample_ms, state.smoothed_ms))
    }

    pub(crate) fn reset(&mut self) {
        self.state = None;
    }
}

fn timing(key: PresentationTimingKey, sample_ms: u32, smoothed_ms: u32) -> PresentationTiming {
    PresentationTiming {
        session_id: key.identity.session_id,
        generation: key.generation,
        source: PresentationTimingSource::MediaIngressToPresent,
        sample_ms,
        smoothed_ms,
    }
}

fn duration_ms(duration: Duration) -> u32 {
    duration.as_millis().min(u128::from(u32::MAX)) as u32
}

fn ewma(previous: u32, sample: u32) -> u32 {
    ((u64::from(previous) * 7 + u64::from(sample)) / 8) as u32
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use frd_core::SessionId;
    use frd_media_api::{VideoStreamIdentity, VideoTimestamp};

    use super::{PresentationTimingKey, PresentationTimingTracker};

    fn key(serial: u64) -> PresentationTimingKey {
        PresentationTimingKey {
            identity: VideoStreamIdentity {
                session_id: SessionId::allocate(),
                stream_id: 1,
            },
            generation: 7,
            worker_epoch_serial: serial,
        }
    }

    #[test]
    fn first_sample_is_immediate_then_ewma_is_throttled_to_half_a_second() {
        let mut tracker = PresentationTimingTracker::default();
        let base = Instant::now();
        let key = key(3);

        let first = tracker
            .observe(key, base, base + Duration::from_millis(80))
            .unwrap();
        assert_eq!(first.sample_ms, 80);
        assert_eq!(first.smoothed_ms, 80);

        assert_eq!(
            tracker.observe(
                key,
                base + Duration::from_millis(100),
                base + Duration::from_millis(260),
            ),
            None,
            "the EWMA updates but the title publication is throttled"
        );
        let published = tracker
            .observe(
                key,
                base + Duration::from_millis(500),
                base + Duration::from_millis(580),
            )
            .unwrap();
        assert_eq!(published.sample_ms, 80);
        assert_eq!(published.smoothed_ms, 88, "80 -> 90 -> 88 with 7/8 EWMA");
    }

    #[test]
    fn new_worker_epoch_and_reset_publish_a_fresh_first_sample() {
        let mut tracker = PresentationTimingTracker::default();
        let base = Instant::now();
        let first_key = key(3);
        tracker
            .observe(first_key, base, base + Duration::from_millis(80))
            .unwrap();

        let mut replacement = first_key;
        replacement.worker_epoch_serial = 4;
        assert_eq!(
            tracker
                .observe(
                    replacement,
                    base + Duration::from_millis(100),
                    base + Duration::from_millis(130),
                )
                .unwrap()
                .smoothed_ms,
            30
        );
        tracker.reset();
        assert_eq!(
            tracker
                .observe(
                    replacement,
                    base + Duration::from_millis(140),
                    base + Duration::from_millis(160),
                )
                .unwrap()
                .smoothed_ms,
            20
        );
    }

    #[test]
    fn wire_timestamp_is_not_part_of_the_local_timing_key() {
        let timestamp = VideoTimestamp {
            ticks: 90_000,
            timescale: std::num::NonZeroU32::new(90_000).unwrap(),
        };
        assert_eq!(timestamp.ticks, 90_000);
        assert_eq!(key(1).worker_epoch_serial, 1);
    }
}
