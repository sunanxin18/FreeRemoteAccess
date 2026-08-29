//! P5 mode-4 离线实验状态机。
//!
//! stock Apple Audio Chat 由 IDS/Apple-ID 邀请状态门控，用户名/密码 HPSS viewer
//! 会在进入本模块前拒绝 `PcToMac`。这些类型仅保留确定性传输/回归测试，不代表可用
//! 的产品麦克风入口。

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use super::media_transport::AudioReceptionEvidence;

pub const P5_PROBE_SAMPLE_RATE_HZ: u32 = 48_000;
pub const P5_PROBE_SAMPLES_PER_FRAME: usize = 480;
pub const P5_PROBE_FRAME_COUNT: u16 = 500;
pub const P5_PROBE_AMPLITUDE: i16 = 10_000;
pub const P5_PROBE_HALF_PERIOD_SAMPLES: usize = 24;
pub const P5_PROBE_FRAME_INTERVAL: Duration = Duration::from_millis(10);
pub const P5_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(3);

const P5_CONFIRMATION_TIMEOUT_REASON: &str = "等待 Mac 确认 P5 音频探针超时";
const P5_GENERATION_MISMATCH_REASON: &str = "P5 音频探针 transport generation 不匹配";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioInputPhase {
    Disabled,
    Negotiating { generation: u64 },
    ProbeReady { generation: u64 },
    ProbeSending { generation: u64 },
    ProbeAwaitingReport { generation: u64 },
    ProbeConfirmed { generation: u64 },
    MicrophoneReady { generation: u64 },
    MicrophoneStreaming { generation: u64 },
    Degraded { generation: u64, reason: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioInputSourceMode {
    DeterministicProbe,
    WindowsMicrophone,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct P5ProbeFrame {
    pub token: u16,
    pub pcm: [i16; P5_PROBE_SAMPLES_PER_FRAME],
}

#[derive(Debug)]
pub struct AudioInputRuntime {
    source_mode: AudioInputSourceMode,
    phase: AudioInputPhase,
    started_at: Option<Instant>,
    next_frame_at: Option<Instant>,
    frames_offered: u16,
    frames_sent: u16,
    sent_tokens: BTreeSet<u16>,
    confirmation_deadline: Option<Instant>,
    confirmation_latched: bool,
}

impl AudioInputRuntime {
    pub fn new(source_mode: AudioInputSourceMode) -> Self {
        Self {
            source_mode,
            phase: AudioInputPhase::Disabled,
            started_at: None,
            next_frame_at: None,
            frames_offered: 0,
            frames_sent: 0,
            sent_tokens: BTreeSet::new(),
            confirmation_deadline: None,
            confirmation_latched: false,
        }
    }

    pub fn phase(&self) -> &AudioInputPhase {
        &self.phase
    }

    pub fn source_mode(&self) -> AudioInputSourceMode {
        self.source_mode
    }

    pub fn begin_negotiation(&mut self, generation: u64) {
        match self.phase {
            AudioInputPhase::Disabled => {
                self.phase = AudioInputPhase::Negotiating { generation };
            }
            _ if self.phase_generation() != Some(generation) => {
                self.reset();
                self.phase = AudioInputPhase::Negotiating { generation };
            }
            _ => {}
        }
    }

    pub fn mark_transport_active(&mut self, generation: u64) {
        if !self.require_generation(generation) {
            return;
        }
        if matches!(self.phase, AudioInputPhase::Negotiating { .. }) {
            self.phase = match self.source_mode {
                AudioInputSourceMode::DeterministicProbe => {
                    AudioInputPhase::ProbeReady { generation }
                }
                AudioInputSourceMode::WindowsMicrophone => {
                    AudioInputPhase::MicrophoneReady { generation }
                }
            };
        }
    }

    pub fn start_probe(&mut self, generation: u64, now: Instant) {
        if !self.require_generation(generation) {
            return;
        }
        if self.source_mode != AudioInputSourceMode::DeterministicProbe
            || !matches!(self.phase, AudioInputPhase::ProbeReady { .. })
        {
            return;
        }
        self.clear_probe_state();
        self.started_at = Some(now);
        self.next_frame_at = Some(now);
        self.phase = AudioInputPhase::ProbeSending { generation };
    }

    pub fn take_due_probe_frames(
        &mut self,
        generation: u64,
        now: Instant,
        budget: usize,
    ) -> Vec<P5ProbeFrame> {
        if !self.require_generation(generation)
            || self.source_mode != AudioInputSourceMode::DeterministicProbe
            || !matches!(self.phase, AudioInputPhase::ProbeSending { .. })
            || budget == 0
        {
            return Vec::new();
        }
        let Some(next_frame_at) = self.next_frame_at else {
            return Vec::new();
        };
        if now < next_frame_at || self.frames_offered == P5_PROBE_FRAME_COUNT {
            return Vec::new();
        }

        let elapsed_slots =
            now.duration_since(next_frame_at).as_nanos() / P5_PROBE_FRAME_INTERVAL.as_nanos();
        let due = elapsed_slots.saturating_add(1);
        let remaining = usize::from(P5_PROBE_FRAME_COUNT - self.frames_offered);
        let count = due.min(budget as u128).min(remaining as u128) as usize;
        let mut frames = Vec::with_capacity(count);
        for _ in 0..count {
            frames.push(P5ProbeFrame {
                token: self.frames_offered,
                pcm: p5_probe_pcm_frame(),
            });
            self.frames_offered += 1;
            self.next_frame_at = self
                .next_frame_at
                .map(|due_at| due_at + P5_PROBE_FRAME_INTERVAL);
        }
        frames
    }

    pub fn record_probe_frame_sent(&mut self, generation: u64, token: u16, now: Instant) {
        if !self.require_generation(generation)
            || !matches!(self.phase, AudioInputPhase::ProbeSending { .. })
            || token >= self.frames_offered
            || !self.sent_tokens.insert(token)
        {
            return;
        }
        self.frames_sent += 1;
        if self.frames_sent != P5_PROBE_FRAME_COUNT {
            return;
        }
        self.confirmation_deadline = Some(now + P5_CONFIRMATION_TIMEOUT);
        self.phase = AudioInputPhase::ProbeAwaitingReport { generation };
        self.confirm_if_latched(generation);
    }

    pub fn observe_transport_evidence(
        &mut self,
        generation: u64,
        evidence: AudioReceptionEvidence,
    ) {
        if !self.require_generation(generation)
            || !matches!(evidence, AudioReceptionEvidence::Confirmed { .. })
            || !matches!(
                self.phase,
                AudioInputPhase::ProbeSending { .. } | AudioInputPhase::ProbeAwaitingReport { .. }
            )
        {
            return;
        }
        self.confirmation_latched = true;
        self.confirm_if_latched(generation);
    }

    pub fn poll(&mut self, generation: u64, now: Instant) {
        if !self.require_generation(generation)
            || !matches!(self.phase, AudioInputPhase::ProbeAwaitingReport { .. })
        {
            return;
        }
        if self.confirmation_latched {
            self.confirm_if_latched(generation);
            return;
        }
        if self
            .confirmation_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.phase = AudioInputPhase::Degraded {
                generation,
                reason: P5_CONFIRMATION_TIMEOUT_REASON.to_owned(),
            };
            self.clear_probe_state();
        }
    }

    pub fn fail_probe(&mut self, generation: u64, reason: impl Into<String>) {
        if !self.require_generation(generation)
            || matches!(
                self.phase,
                AudioInputPhase::Degraded { .. } | AudioInputPhase::ProbeConfirmed { .. }
            )
        {
            return;
        }
        self.phase = AudioInputPhase::Degraded {
            generation,
            reason: reason.into(),
        };
        self.clear_probe_state();
    }

    pub fn mark_microphone_streaming(&mut self, generation: u64) {
        if !self.require_generation(generation) {
            return;
        }
        if self.source_mode == AudioInputSourceMode::WindowsMicrophone
            && matches!(self.phase, AudioInputPhase::MicrophoneReady { .. })
        {
            self.phase = AudioInputPhase::MicrophoneStreaming { generation };
        }
    }

    pub fn teardown(&mut self) {
        self.reset();
    }

    #[cfg(test)]
    pub fn frames_offered(&self) -> u16 {
        self.frames_offered
    }

    #[cfg(test)]
    pub fn frames_sent(&self) -> u16 {
        self.frames_sent
    }

    #[cfg(test)]
    pub fn started_at(&self) -> Option<Instant> {
        self.started_at
    }

    #[cfg(test)]
    pub fn next_frame_at(&self) -> Option<Instant> {
        self.next_frame_at
    }

    #[cfg(test)]
    pub fn confirmation_deadline(&self) -> Option<Instant> {
        self.confirmation_deadline
    }

    fn phase_generation(&self) -> Option<u64> {
        match self.phase {
            AudioInputPhase::Disabled => None,
            AudioInputPhase::Negotiating { generation }
            | AudioInputPhase::ProbeReady { generation }
            | AudioInputPhase::ProbeSending { generation }
            | AudioInputPhase::ProbeAwaitingReport { generation }
            | AudioInputPhase::ProbeConfirmed { generation }
            | AudioInputPhase::MicrophoneReady { generation }
            | AudioInputPhase::MicrophoneStreaming { generation }
            | AudioInputPhase::Degraded { generation, .. } => Some(generation),
        }
    }

    fn require_generation(&mut self, generation: u64) -> bool {
        match self.phase_generation() {
            Some(current_generation) if current_generation == generation => true,
            Some(current_generation) => {
                self.phase = AudioInputPhase::Degraded {
                    generation: current_generation,
                    reason: P5_GENERATION_MISMATCH_REASON.to_owned(),
                };
                self.clear_probe_state();
                false
            }
            None => false,
        }
    }

    fn confirm_if_latched(&mut self, generation: u64) {
        if self.confirmation_latched
            && self.frames_sent == P5_PROBE_FRAME_COUNT
            && matches!(self.phase, AudioInputPhase::ProbeAwaitingReport { .. })
        {
            self.phase = AudioInputPhase::ProbeConfirmed { generation };
            self.clear_probe_state();
        }
    }

    fn reset(&mut self) {
        self.phase = AudioInputPhase::Disabled;
        self.clear_probe_state();
    }

    fn clear_probe_state(&mut self) {
        self.started_at = None;
        self.next_frame_at = None;
        self.frames_offered = 0;
        self.frames_sent = 0;
        self.sent_tokens.clear();
        self.confirmation_deadline = None;
        self.confirmation_latched = false;
    }
}

pub fn p5_probe_pcm_frame() -> [i16; P5_PROBE_SAMPLES_PER_FRAME] {
    std::array::from_fn(|index| {
        if (index / P5_PROBE_HALF_PERIOD_SAMPLES).is_multiple_of(2) {
            P5_PROBE_AMPLITUDE
        } else {
            -P5_PROBE_AMPLITUDE
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        p5_probe_pcm_frame, AudioInputPhase, AudioInputRuntime, AudioInputSourceMode,
        P5_CONFIRMATION_TIMEOUT, P5_PROBE_AMPLITUDE, P5_PROBE_FRAME_COUNT, P5_PROBE_FRAME_INTERVAL,
        P5_PROBE_SAMPLES_PER_FRAME, P5_PROBE_SAMPLE_RATE_HZ,
    };
    use crate::media_transport::AudioReceptionEvidence;

    const GENERATION: u64 = 7;

    fn ready_runtime(now: Instant) -> AudioInputRuntime {
        let mut runtime = AudioInputRuntime::new(AudioInputSourceMode::DeterministicProbe);
        runtime.begin_negotiation(GENERATION);
        runtime.mark_transport_active(GENERATION);
        runtime.start_probe(GENERATION, now);
        runtime
    }

    #[test]
    fn phase_machine_follows_the_p5_happy_path_in_order() {
        let now = Instant::now();
        let mut runtime = AudioInputRuntime::new(AudioInputSourceMode::DeterministicProbe);
        assert_eq!(runtime.phase(), &AudioInputPhase::Disabled);

        runtime.begin_negotiation(GENERATION);
        assert_eq!(
            runtime.phase(),
            &AudioInputPhase::Negotiating {
                generation: GENERATION
            }
        );
        runtime.mark_transport_active(GENERATION);
        assert_eq!(
            runtime.phase(),
            &AudioInputPhase::ProbeReady {
                generation: GENERATION
            }
        );
        runtime.start_probe(GENERATION, now);
        assert_eq!(
            runtime.phase(),
            &AudioInputPhase::ProbeSending {
                generation: GENERATION
            }
        );

        runtime.observe_transport_evidence(
            GENERATION,
            AudioReceptionEvidence::Confirmed {
                extended_highest_sequence: 100,
                cumulative_packets_lost: 0,
            },
        );
        for frame_index in 0..P5_PROBE_FRAME_COUNT {
            let sent_at = now + P5_PROBE_FRAME_INTERVAL * u32::from(frame_index);
            let frames = runtime.take_due_probe_frames(GENERATION, sent_at, 1);
            assert_eq!(frames.len(), 1);
            runtime.record_probe_frame_sent(GENERATION, frames[0].token, sent_at);
        }
        assert_eq!(
            runtime.phase(),
            &AudioInputPhase::ProbeConfirmed {
                generation: GENERATION
            }
        );
    }

    #[test]
    fn local_error_is_a_single_terminal_degraded_transition() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        runtime.fail_probe(GENERATION, "编码失败");
        assert_eq!(
            runtime.phase(),
            &AudioInputPhase::Degraded {
                generation: GENERATION,
                reason: "编码失败".to_owned(),
            }
        );
        runtime.fail_probe(GENERATION, "发送失败");
        assert_eq!(
            runtime.phase(),
            &AudioInputPhase::Degraded {
                generation: GENERATION,
                reason: "编码失败".to_owned(),
            }
        );
    }

    #[test]
    fn runtime_generation_mismatch_degrades_and_clears_probe_state() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        assert_eq!(runtime.started_at(), Some(now));
        let frames = runtime.take_due_probe_frames(GENERATION, now, 2);
        assert_eq!(frames.len(), 1);
        runtime.record_probe_frame_sent(GENERATION, frames[0].token, now);
        runtime.take_due_probe_frames(GENERATION + 1, now, 1);
        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::Degraded {
                generation: GENERATION,
                ..
            }
        ));
        assert_eq!(runtime.frames_offered(), 0);
        assert_eq!(runtime.frames_sent(), 0);
        assert_eq!(runtime.started_at(), None);
        assert_eq!(runtime.next_frame_at(), None);
        assert_eq!(runtime.confirmation_deadline(), None);
    }

    #[test]
    fn probe_tone_has_exact_frame_shape_and_one_kilohertz_period() {
        let frame = p5_probe_pcm_frame();
        assert_eq!(frame.len(), P5_PROBE_SAMPLES_PER_FRAME);
        for sample in &frame[..24] {
            assert_eq!(*sample, P5_PROBE_AMPLITUDE);
        }
        for sample in &frame[24..48] {
            assert_eq!(*sample, -P5_PROBE_AMPLITUDE);
        }
        assert_eq!(frame[47], -P5_PROBE_AMPLITUDE);
        assert_eq!(frame[48], P5_PROBE_AMPLITUDE);
        assert_eq!(frame[479], -P5_PROBE_AMPLITUDE);
        assert_eq!(P5_PROBE_SAMPLE_RATE_HZ / 48, 1_000);
    }

    #[test]
    fn probe_sends_exactly_500_frames_before_awaiting_report() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        assert!(runtime
            .take_due_probe_frames(GENERATION, now - Duration::from_millis(1), 16)
            .is_empty());
        let first = runtime.take_due_probe_frames(GENERATION, now, 3);
        assert_eq!(first.len(), 1);
        runtime.record_probe_frame_sent(GENERATION, first[0].token, now);
        assert!(runtime
            .take_due_probe_frames(GENERATION, now + Duration::from_millis(9), 16)
            .is_empty());
        let next = runtime.take_due_probe_frames(GENERATION, now + Duration::from_millis(50), 2);
        assert_eq!(next.len(), 2);
        for frame in next {
            runtime.record_probe_frame_sent(
                GENERATION,
                frame.token,
                now + Duration::from_millis(50),
            );
        }

        let mut frames = 3;
        while frames < usize::from(P5_PROBE_FRAME_COUNT) {
            let offered =
                runtime.take_due_probe_frames(GENERATION, now + Duration::from_secs(10), 37);
            assert!(offered.len() <= 37);
            assert!(offered.iter().all(|frame| frame.pcm.len() == 480));
            frames += offered.len();
            for frame in offered {
                runtime.record_probe_frame_sent(
                    GENERATION,
                    frame.token,
                    now + Duration::from_secs(10),
                );
            }
        }
        assert_eq!(frames, usize::from(P5_PROBE_FRAME_COUNT));
        assert_eq!(runtime.frames_sent(), P5_PROBE_FRAME_COUNT);
        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::ProbeAwaitingReport {
                generation: GENERATION
            }
        ));
        assert!(runtime
            .take_due_probe_frames(GENERATION, now + Duration::from_secs(10), 1)
            .is_empty());
    }

    #[test]
    fn early_in_range_confirmation_latches_until_frame_500() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        runtime.observe_transport_evidence(
            GENERATION,
            AudioReceptionEvidence::Confirmed {
                extended_highest_sequence: 1,
                cumulative_packets_lost: 0,
            },
        );
        let frames = runtime.take_due_probe_frames(
            GENERATION,
            now + Duration::from_secs(5),
            usize::from(P5_PROBE_FRAME_COUNT),
        );
        assert_eq!(frames.len(), usize::from(P5_PROBE_FRAME_COUNT));
        for frame in &frames[..frames.len() - 1] {
            runtime.record_probe_frame_sent(GENERATION, frame.token, now);
        }
        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::ProbeSending { .. }
        ));
        runtime.record_probe_frame_sent(GENERATION, frames.last().unwrap().token, now);
        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::ProbeConfirmed {
                generation: GENERATION
            }
        ));
    }

    #[test]
    fn missing_confirmation_degrades_three_seconds_after_last_successful_frame() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        let last_sent_at = now + Duration::from_secs(5);
        let frames = runtime.take_due_probe_frames(GENERATION, last_sent_at, 500);
        assert_eq!(frames.len(), 500);
        for frame in frames {
            runtime.record_probe_frame_sent(GENERATION, frame.token, last_sent_at);
        }
        runtime.poll(
            GENERATION,
            last_sent_at + P5_CONFIRMATION_TIMEOUT - Duration::from_nanos(1),
        );
        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::ProbeAwaitingReport { .. }
        ));
        runtime.poll(GENERATION, last_sent_at + P5_CONFIRMATION_TIMEOUT);
        assert_eq!(
            runtime.phase(),
            &AudioInputPhase::Degraded {
                generation: GENERATION,
                reason: "等待 Mac 确认 P5 音频探针超时".to_owned(),
            }
        );
    }

    #[test]
    fn send_failure_stops_the_probe_without_retrying() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        assert_eq!(runtime.take_due_probe_frames(GENERATION, now, 1).len(), 1);
        runtime.fail_probe(GENERATION, "探针编码或发送失败");
        assert!(matches!(runtime.phase(), AudioInputPhase::Degraded { .. }));
        assert!(runtime
            .take_due_probe_frames(GENERATION, now + Duration::from_secs(30), 500)
            .is_empty());
    }

    #[test]
    fn duplicate_offered_token_cannot_count_as_multiple_successful_sends() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        let frames = runtime.take_due_probe_frames(GENERATION, now + Duration::from_secs(5), 500);
        assert_eq!(frames.len(), usize::from(P5_PROBE_FRAME_COUNT));
        assert!(frames
            .iter()
            .enumerate()
            .all(|(ordinal, frame)| frame.token == ordinal as u16));
        let repeated_token = frames[0].token;

        for _ in 0..P5_PROBE_FRAME_COUNT {
            runtime.record_probe_frame_sent(GENERATION, repeated_token, now);
        }

        assert_eq!(runtime.frames_sent(), 1);
        assert_eq!(runtime.confirmation_deadline(), None);
        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::ProbeSending { .. }
        ));
    }

    #[test]
    fn generation_mismatch_clears_an_existing_deadline_before_a_new_probe() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        let old_last_sent_at = now + Duration::from_secs(5);
        let old_frames = runtime.take_due_probe_frames(GENERATION, old_last_sent_at, 500);
        for frame in old_frames {
            runtime.record_probe_frame_sent(GENERATION, frame.token, old_last_sent_at);
        }
        assert_eq!(
            runtime.confirmation_deadline(),
            Some(old_last_sent_at + P5_CONFIRMATION_TIMEOUT)
        );

        runtime.take_due_probe_frames(GENERATION + 1, old_last_sent_at, 1);
        runtime.begin_negotiation(GENERATION + 1);
        runtime.mark_transport_active(GENERATION + 1);
        runtime.start_probe(GENERATION + 1, old_last_sent_at);
        runtime.poll(
            GENERATION + 1,
            old_last_sent_at + P5_CONFIRMATION_TIMEOUT + Duration::from_secs(1),
        );

        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::ProbeSending {
                generation: new_generation
            } if *new_generation == GENERATION + 1
        ));
        assert_eq!(runtime.confirmation_deadline(), None);
    }

    #[test]
    fn teardown_clears_source_mode_probe_and_confirmation_state() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        runtime.observe_transport_evidence(
            GENERATION,
            AudioReceptionEvidence::Confirmed {
                extended_highest_sequence: 1,
                cumulative_packets_lost: 0,
            },
        );
        runtime.teardown();

        let new_generation = GENERATION + 1;
        runtime.begin_negotiation(new_generation);
        runtime.mark_transport_active(new_generation);
        let new_last_sent_at = now + Duration::from_secs(5);
        runtime.start_probe(new_generation, now);
        for frame in runtime.take_due_probe_frames(new_generation, new_last_sent_at, 500) {
            runtime.record_probe_frame_sent(new_generation, frame.token, new_last_sent_at);
        }

        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::ProbeAwaitingReport {
                generation: active_generation
            } if *active_generation == new_generation
        ));
        runtime.poll(new_generation, new_last_sent_at + P5_CONFIRMATION_TIMEOUT);
        assert!(matches!(runtime.phase(), AudioInputPhase::Degraded { .. }));
    }

    #[test]
    fn wrong_generation_and_unconfirmed_evidence_never_confirm() {
        let now = Instant::now();
        let mut runtime = ready_runtime(now);
        runtime.observe_transport_evidence(GENERATION, AudioReceptionEvidence::NotObserved);
        runtime.observe_transport_evidence(
            GENERATION,
            AudioReceptionEvidence::MatchingReport {
                extended_highest_sequence: 1,
                cumulative_packets_lost: 0,
            },
        );
        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::ProbeSending { .. }
        ));
        runtime.observe_transport_evidence(
            GENERATION + 1,
            AudioReceptionEvidence::Confirmed {
                extended_highest_sequence: 1,
                cumulative_packets_lost: 0,
            },
        );

        assert!(matches!(
            runtime.phase(),
            AudioInputPhase::Degraded {
                generation: GENERATION,
                ..
            }
        ));
        assert_eq!(runtime.frames_offered(), 0);
        assert_eq!(runtime.frames_sent(), 0);
        assert_eq!(runtime.confirmation_deadline(), None);
    }

    #[test]
    fn microphone_phase_cannot_start_in_deterministic_probe_mode() {
        let mut runtime = AudioInputRuntime::new(AudioInputSourceMode::DeterministicProbe);
        runtime.begin_negotiation(GENERATION);
        runtime.mark_transport_active(GENERATION);

        runtime.mark_microphone_streaming(GENERATION);

        assert_eq!(
            runtime.phase(),
            &AudioInputPhase::ProbeReady {
                generation: GENERATION
            }
        );
    }
}
