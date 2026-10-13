//! 显式合成测量；默认构建不包含，不能作为生产 codec 注册证据。
use super::codec_fixture_common::*;
use frd_media_api::*;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[derive(Debug, PartialEq, Eq)]
struct Statistics {
    n: usize,
    median_ns: u128,
    p95_ns: u128,
    max_ns: u128,
    sum_ns: u128,
}
fn statistics(samples: &[u128]) -> Option<Statistics> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let median_ns = if n % 2 == 0 {
        sorted[n / 2 - 1] + (sorted[n / 2] - sorted[n / 2 - 1]) / 2
    } else {
        sorted[n / 2]
    };
    Some(Statistics {
        n,
        median_ns,
        p95_ns: sorted[(n * 95).div_ceil(100) - 1],
        max_ns: sorted[n - 1],
        sum_ns: sorted.iter().sum(),
    })
}
fn budget(global: Instant, case: Instant, cancel: &AtomicBool) -> bool {
    !cancel.load(Ordering::Acquire)
        && global.elapsed() < Duration::from_secs(45)
        && case.elapsed() < Duration::from_secs(10)
}
#[derive(Default)]
struct Samples {
    idr: Vec<u128>,
    p: Vec<u128>,
    reset: Vec<u128>,
    drop_frame: Vec<u128>,
    sequences: usize,
}
fn check(global: Instant, began: Instant, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(Failure::Cancelled);
    }
    if !budget(global, began, cancel) {
        return Err(Failure::Deadline);
    }
    Ok(())
}
// 每次重放先 reset/new generation，完整 IDR/P 序列才增加 sequences。
#[allow(clippy::too_many_arguments)]
fn sequence(
    case: &Case,
    config: &VideoStreamConfig,
    decoder: &mut dyn VideoDecoder,
    generation: u64,
    verify: bool,
    samples: &mut Samples,
    global: Instant,
    began: Instant,
    cancel: &AtomicBool,
) -> Result<()> {
    check(global, began, cancel)?;
    if generation > 1 {
        let start = Instant::now();
        decoder.reset(generation)?;
        samples.reset.push(start.elapsed().as_nanos());
        check(global, began, cancel)?;
    }
    for index in 0..case.units.len() {
        check(global, began, cancel)?;
        let au = access(config, case.units[index], generation, index)?;
        check(global, began, cancel)?;
        let start = Instant::now();
        let result = decoder.submit(au);
        let elapsed = start.elapsed().as_nanos();
        let frame = one_frame(result?)?;
        verify_metadata(case, config, &frame, index, generation)?;
        check(global, began, cancel)?;
        if verify {
            verify_frame(case, config, &frame, index, generation)?;
        }
        check(global, began, cancel)?;
        if index == 0 {
            samples.idr.push(elapsed);
        } else {
            samples.p.push(elapsed);
        }
        let start = Instant::now();
        drop(frame);
        samples.drop_frame.push(start.elapsed().as_nanos());
        check(global, began, cancel)?;
    }
    samples.sequences += 1;
    Ok(())
}
fn profile_completion(sequences: usize) -> Result<()> {
    if sequences == 0 {
        Err(Failure::Contract)
    } else {
        Ok(())
    }
}
fn hevc_case() -> Case {
    Case {
        id: "synthetic-hevc444-256x144-2frames",
        width: 256,
        height: 144,
        visible_width: 256,
        visible_height: 144,
        hevc: true,
        y420: false,
        units: &[
            include_bytes!("../tests/codec-fixtures/synthetic-hevc444-256x144-2frames-au0.hevc"),
            include_bytes!("../tests/codec-fixtures/synthetic-hevc444-256x144-2frames-au1.hevc"),
        ],
        hashes: &[
            "950c99a38c656f3ed945080548ded1658488e34e781935ec6fe61483bf32b6ca",
            "3edca5b966a900e59d553b40f177c690644bafb86bd971f87bc4806bb56e4773",
        ],
        references: &[
            include_bytes!(
                "../tests/codec-fixtures/synthetic-hevc444-256x144-2frames-frame0.yuv444p"
            ),
            include_bytes!(
                "../tests/codec-fixtures/synthetic-hevc444-256x144-2frames-frame1.yuv444p"
            ),
        ],
    }
}
#[cfg(target_env = "ohos")]
mod runtime {
    use super::*;
    use frd_video_ffmpeg::FfmpegBackend;
    use std::{sync::Arc, thread::JoinHandle};
    pub(crate) struct Worker {
        cancel: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }
    impl Worker {
        pub(crate) fn start() -> Self {
            static STARTED: AtomicBool = AtomicBool::new(false);
            let cancel = Arc::new(AtomicBool::new(false));
            let mut worker = Self {
                cancel: cancel.clone(),
                handle: None,
            };
            if STARTED.swap(true, Ordering::AcqRel) {
                return worker;
            }
            worker.handle = std::thread::Builder::new()
                .name("frd-codec-measure".into())
                .spawn(move || {
                    let _scope = crate::panic_scope::Scope::enter();
                    if std::panic::catch_unwind(|| run(&cancel)).is_err() {
                        emit("phase=worker_end completed=0 gate=Panic");
                    }
                })
                .ok();
            if worker.handle.is_none() {
                emit("phase=worker_end completed=0 gate=WorkerSpawn");
            }
            worker
        }
        pub(crate) fn reap(&mut self) {
            if self.handle.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(handle) = self.handle.take() {
                    let _ = handle.join();
                }
            }
        }
    }
    impl Drop for Worker {
        fn drop(&mut self) {
            self.cancel.store(true, Ordering::Release);
            self.reap();
        }
    }
    fn mode() -> &'static str {
        if cfg!(feature = "normal-hap-avc444-profile") {
            "avc444_profile"
        } else if cfg!(feature = "normal-hap-hevc444-profile") {
            "hevc444_profile"
        } else {
            "benchmark"
        }
    }
    // 公开 CLOCK_MONOTONIC 与 TID；供本应用 profiler 阶段归属，无 ASLR 地址输出。
    fn emit(text: &str) {
        #[repr(C)]
        struct Timespec {
            sec: i64,
            ns: i64,
        }
        #[link(name = "hilog_ndk.z")]
        extern "C" {
            fn gettid() -> i32;
            fn clock_gettime(clock: i32, out: *mut Timespec) -> i32;
            fn OH_LOG_Print(
                t: u32,
                l: u32,
                d: u32,
                tag: *const std::ffi::c_char,
                fmt: *const std::ffi::c_char,
                ...
            ) -> i32;
        }
        let mut stamp = Timespec { sec: 0, ns: 0 };
        // SAFETY: 固定时钟、有效输出存储；日志仅固定枚举/合成 ID/计数。
        let (tid, valid) = unsafe { (gettid(), clock_gettime(1, &mut stamp) == 0) };
        let mono = if valid {
            stamp.sec as u128 * 1_000_000_000 + stamp.ns as u128
        } else {
            0
        };
        if let Ok(line) = std::ffi::CString::new(format!(
            "fixture=4c4 run=synthetic-v1 mode={} tid={tid} mono_ns={mono} clock_valid={} {text}",
            mode(),
            u8::from(valid)
        )) {
            unsafe {
                OH_LOG_Print(
                    0,
                    4,
                    0xFF00,
                    c"FrdCodecMeasure".as_ptr(),
                    c"%{public}s".as_ptr(),
                    line.as_ptr(),
                );
            }
        }
    }
    fn report(case: &Case, length: bool, phase: &str, samples: &Samples) {
        for (group, values) in [
            ("IDR", &samples.idr),
            ("P", &samples.p),
            ("reset", &samples.reset),
            ("frame_drop", &samples.drop_frame),
        ] {
            if let Some(s) = statistics(values) {
                emit(&format!("case={} length={} phase={phase} group={group} n={} median_ns={} p95_ns={} max_ns={} sum_ns={}",case.id,u8::from(length),s.n,s.median_ns,s.p95_ns,s.max_ns,s.sum_ns));
            } else {
                emit(&format!(
                    "case={} length={} phase={phase} group={group} n=0",
                    case.id,
                    u8::from(length)
                ));
            }
        }
        let n = samples.idr.len() + samples.p.len();
        let ns = samples.idr.iter().chain(&samples.p).sum::<u128>();
        if ns > 0 {
            emit(&format!("case={} length={} phase={phase} sequences={} decoded_frames={n} compute_only_fps={:.3} visible_pixels_per_sec={:.3}",case.id,u8::from(length),samples.sequences,n as f64*1e9/ns as f64,n as f64*case.visible_width as f64*case.visible_height as f64*1e9/ns as f64));
        }
    }
    fn run_case(case: &Case, length: bool, global: Instant, cancel: &AtomicBool) -> Result<()> {
        let began = Instant::now();
        check(global, began, cancel)?;
        let config = config(case, length)?;
        emit(&format!(
            "case={} length={} phase=case_begin",
            case.id,
            u8::from(length)
        ));
        let load = Instant::now();
        let backend = FfmpegBackend::load()?;
        emit(&format!(
            "case={} phase=load_end load_ns={} timing_excluded=1",
            case.id,
            load.elapsed().as_nanos()
        ));
        check(global, began, cancel)?;
        let create = Instant::now();
        let mut decoder = backend.create(&config)?;
        let create_ns = create.elapsed().as_nanos();
        let mut cold = Samples::default();
        let mut measured = Samples::default();
        let result: Result<()> = (|| {
            check(global, began, cancel)?;
            emit(&format!(
                "case={} phase=cold_begin create_ns={create_ns}",
                case.id
            ));
            sequence(
                case,
                &config,
                decoder.as_mut(),
                1,
                true,
                &mut cold,
                global,
                began,
                cancel,
            )?;
            emit(&format!(
                "case={} phase=before pixels=pass no_eos=pass",
                case.id
            ));
            report(case, length, "cold", &cold);
            let mut generation = 2;
            if mode() == "benchmark" {
                emit(&format!("case={} phase=warmup_begin", case.id));
                let mut warmup = Samples::default();
                for _ in 0..4 {
                    sequence(
                        case,
                        &config,
                        decoder.as_mut(),
                        generation,
                        false,
                        &mut warmup,
                        global,
                        began,
                        cancel,
                    )?;
                    generation += 1;
                }
                emit(&format!("case={} phase=warmup_end sequences=4", case.id));
                for batch in 0..3 {
                    emit(&format!("case={} phase=batch_begin batch={batch}", case.id));
                    let mut current = Samples::default();
                    let result: Result<()> = (|| {
                        for _ in 0..20 {
                            sequence(
                                case,
                                &config,
                                decoder.as_mut(),
                                generation,
                                false,
                                &mut current,
                                global,
                                began,
                                cancel,
                            )?;
                            generation += 1;
                        }
                        Ok(())
                    })();
                    report(case, length, &format!("batch{batch}"), &current);
                    measured.idr.extend(current.idr);
                    measured.p.extend(current.p);
                    measured.reset.extend(current.reset);
                    measured.drop_frame.extend(current.drop_frame);
                    measured.sequences += current.sequences;
                    check(global, began, cancel)?;
                    emit(&format!(
                        "case={} phase=batch_end batch={batch} complete={}",
                        case.id,
                        u8::from(result.is_ok())
                    ));
                    result?;
                }
            } else {
                // 保留两秒给最终像素 oracle 和清理；不得只重复依赖 P。
                let phase = Instant::now();
                emit(&format!("case={} phase=profile_begin", case.id));
                let result: Result<()> = (|| {
                    while phase.elapsed() < Duration::from_secs(7)
                        && began.elapsed() < Duration::from_secs(8)
                    {
                        sequence(
                            case,
                            &config,
                            decoder.as_mut(),
                            generation,
                            false,
                            &mut measured,
                            global,
                            began,
                            cancel,
                        )?;
                        generation += 1;
                    }
                    Ok(())
                })();
                let result = result.and_then(|()| profile_completion(measured.sequences));
                emit(&format!(
                    "case={} phase=profile_end sequences={} complete={}",
                    case.id,
                    measured.sequences,
                    u8::from(result.is_ok())
                ));
                result?;
            }
            check(global, began, cancel)?;
            let mut after = Samples::default();
            sequence(
                case,
                &config,
                decoder.as_mut(),
                generation,
                true,
                &mut after,
                global,
                began,
                cancel,
            )?;
            emit(&format!(
                "case={} phase=after pixels=pass no_eos=pass",
                case.id
            ));
            Ok(())
        })();
        report(case, length, "measured", &measured);
        let destroy = Instant::now();
        drop(decoder);
        drop(backend);
        let destroy_ns = destroy.elapsed().as_nanos();
        let result = result.and_then(|()| check(global, began, cancel));
        emit(&format!("case={} length={} phase=case_end completed={} sequences={} gate={:?} destroy_ns={destroy_ns} elapsed_ns={}",case.id,u8::from(length),u8::from(result.is_ok()),measured.sequences,result.as_ref().err(),began.elapsed().as_nanos()));
        result
    }
    fn run(cancel: &AtomicBool) {
        let global = Instant::now();
        emit(&format!(
            "phase=worker_begin optimized={} network=0 registry=0 profiler_required=0",
            u8::from(!cfg!(debug_assertions))
        ));
        let mut selected = cases();
        if mode() == "avc444_profile" {
            selected.retain(|c| c.id == "synthetic-avc444-256x144-2frames");
        } else if mode() == "hevc444_profile" {
            selected = vec![hevc_case()];
        } else {
            selected.retain(|c| c.width >= 256 || c.hevc);
            selected.push(hevc_case());
        }
        let expected: usize = selected
            .iter()
            .map(|c| {
                if mode() == "benchmark" && !c.hevc {
                    2
                } else {
                    1
                }
            })
            .sum();
        emit(&format!("phase=selection expected_cases={expected}"));
        let mut passed = 0;
        let mut failed = 0;
        for case in selected {
            for length in [false, true] {
                if length && (case.hevc || mode() != "benchmark") {
                    continue;
                }
                match run_case(&case, length, global, cancel) {
                    Ok(()) => passed += 1,
                    Err(e) => {
                        failed += 1;
                        emit(&format!("case={} phase=failure gate={e:?}", case.id));
                        if matches!(e, Failure::Cancelled | Failure::Deadline) {
                            emit(&format!("phase=worker_end completed=0 passed={passed} failed={failed} expected_cases={expected} elapsed_ns={}",global.elapsed().as_nanos()));
                            return;
                        }
                    }
                }
            }
        }
        emit(&format!("phase=worker_end completed={} passed={passed} failed={failed} expected_cases={expected} elapsed_ns={}",u8::from(failed==0 && passed==expected),global.elapsed().as_nanos()));
    }
}
#[cfg(target_env = "ohos")]
pub(crate) use runtime::Worker;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_hevc_identity_is_exact_and_both_units_have_distinct_reference_frames() {
        use sha2::{Digest, Sha256};
        let case = hevc_case();
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/codec-fixtures/hevc-measurement-provenance.json"
        ))
        .unwrap();
        assert_eq!(case.id, "synthetic-hevc444-256x144-2frames");
        assert!(config(&case, false).is_ok());
        assert_eq!(case.units.len(), 2);
        for index in 0..2 {
            assert_eq!(
                format!("{:x}", Sha256::digest(case.units[index])),
                manifest["access_units"][index]["sha256"]
            );
            assert_eq!(
                format!("{:x}", Sha256::digest(case.references[index])),
                case.hashes[index]
            );
            assert_eq!(case.references[index].len(), 110592);
        }
        assert_ne!(case.hashes[0], case.hashes[1]);
    }
    struct RecordingDecoder {
        case: Case,
        config: VideoStreamConfig,
        generation: u64,
        pending_p: bool,
        wrong_generation: bool,
        resets: Vec<u64>,
        aus: Vec<(u64, u64)>,
    }
    impl VideoDecoder for RecordingDecoder {
        fn submit(
            &mut self,
            au: EncodedVideoAccessUnit,
        ) -> std::result::Result<DecodeOutcome, VideoDecodeError> {
            assert_eq!(au.generation(), self.generation);
            let index = (au.timestamp().ticks / 3000) as usize;
            assert_eq!(au.random_access(), index == 0);
            self.aus.push((au.generation(), au.timestamp().ticks));
            if self.pending_p && index == 1 {
                return Ok(DecodeOutcome::NeedMoreData);
            }
            let mut offset = 0;
            let mut planes = Vec::new();
            for i in 0..3 {
                let d = if i > 0 && self.case.y420 { 2 } else { 1 };
                let width = self.case.width / d;
                let height = self.case.height / d;
                let size = (width * height) as usize;
                planes.push(
                    VideoPlane::try_new(
                        width,
                        height,
                        width,
                        self.case.references[index][offset..offset + size]
                            .to_vec()
                            .into_boxed_slice(),
                    )
                    .unwrap(),
                );
                offset += size;
            }
            let frame = DecodedVideoFrame::try_new(DecodedVideoFrameInput {
                identity: self.config.as_input().identity,
                generation: self.generation + u64::from(self.wrong_generation),
                timestamp: au.timestamp(),
                coded_size: self.config.as_input().coded_size,
                visible_rect: self.config.as_input().visible_rect,
                format: VideoPixelFormat::Yuv420P8,
                colorimetry: VideoColorimetry::Bt709,
                range: VideoRange::Limited,
                planes: planes.into_boxed_slice(),
            })
            .unwrap();
            Ok(DecodeOutcome::Frames(vec![frame].into_boxed_slice()))
        }
        fn reset(&mut self, generation: u64) -> std::result::Result<(), VideoDecodeError> {
            self.generation = generation;
            self.resets.push(generation);
            Ok(())
        }
        fn flush(&mut self) -> std::result::Result<Box<[DecodedVideoFrame]>, VideoDecodeError> {
            panic!("测量不得使用 EOS")
        }
    }
    fn recording() -> RecordingDecoder {
        let case = cases().remove(3);
        let config = config(&case, false).unwrap();
        RecordingDecoder {
            case,
            config,
            generation: 1,
            pending_p: false,
            wrong_generation: false,
            resets: Vec::new(),
            aus: Vec::new(),
        }
    }
    #[test]
    fn replay_resets_each_generation_and_submits_complete_idr_then_p() {
        let mut decoder = recording();
        let case = cases().remove(3);
        let config = decoder.config.clone();
        let mut samples = Samples::default();
        let now = Instant::now();
        for generation in [2, 3] {
            sequence(
                &case,
                &config,
                &mut decoder,
                generation,
                true,
                &mut samples,
                now,
                now,
                &AtomicBool::new(false),
            )
            .unwrap();
        }
        assert_eq!(decoder.resets, vec![2, 3]);
        assert_eq!(decoder.aus, vec![(2, 0), (2, 3000), (3, 0), (3, 3000)]);
        assert_eq!(samples.sequences, 2);
        assert_eq!(samples.idr.len(), 2);
        assert_eq!(samples.p.len(), 2);
        assert_eq!(samples.reset.len(), 2);
        assert_eq!(samples.drop_frame.len(), 4);
    }
    #[test]
    fn missing_last_p_is_incomplete_even_when_idr_was_valid() {
        let mut decoder = recording();
        decoder.pending_p = true;
        let case = cases().remove(3);
        let config = decoder.config.clone();
        let mut samples = Samples::default();
        let now = Instant::now();
        assert_eq!(
            sequence(
                &case,
                &config,
                &mut decoder,
                2,
                false,
                &mut samples,
                now,
                now,
                &AtomicBool::new(false)
            ),
            Err(Failure::BeforeEos)
        );
        assert_eq!(samples.idr.len(), 1);
        assert!(samples.p.is_empty());
        assert_eq!(samples.sequences, 0);
    }
    #[test]
    fn wrong_generation_frame_does_not_enter_timed_sample_set() {
        let mut decoder = recording();
        decoder.wrong_generation = true;
        let case = cases().remove(3);
        let config = decoder.config.clone();
        let mut samples = Samples::default();
        let now = Instant::now();
        assert_eq!(
            sequence(
                &case,
                &config,
                &mut decoder,
                2,
                false,
                &mut samples,
                now,
                now,
                &AtomicBool::new(false)
            ),
            Err(Failure::Geometry)
        );
        assert!(samples.idr.is_empty());
        assert!(samples.p.is_empty());
        assert_eq!(samples.sequences, 0);
    }
    #[test]
    fn empty_profile_repeat_phase_is_incomplete_despite_a_valid_cold_oracle() {
        assert_eq!(profile_completion(0), Err(Failure::Contract));
        assert_eq!(profile_completion(1), Ok(()));
    }
    struct PendingDecoder;
    impl VideoDecoder for PendingDecoder {
        fn submit(
            &mut self,
            _au: EncodedVideoAccessUnit,
        ) -> std::result::Result<DecodeOutcome, VideoDecodeError> {
            Ok(DecodeOutcome::NeedMoreData)
        }
        fn reset(&mut self, _generation: u64) -> std::result::Result<(), VideoDecodeError> {
            Ok(())
        }
        fn flush(&mut self) -> std::result::Result<Box<[DecodedVideoFrame]>, VideoDecodeError> {
            panic!("EOS 不属于测量合同")
        }
    }
    #[test]
    fn missing_immediate_frame_never_enters_statistics_or_completed_sequence() {
        let case = cases().remove(3);
        let config = config(&case, false).unwrap();
        let mut samples = Samples::default();
        let now = Instant::now();
        assert_eq!(
            sequence(
                &case,
                &config,
                &mut PendingDecoder,
                2,
                false,
                &mut samples,
                now,
                now,
                &AtomicBool::new(false)
            ),
            Err(Failure::BeforeEos)
        );
        assert!(samples.idr.is_empty());
        assert!(samples.p.is_empty());
        assert_eq!(samples.sequences, 0);
    }

    #[test]
    fn statistics_sort_and_use_nearest_rank_p95_without_inventing_samples() {
        assert_eq!(statistics(&[]), None);
        assert_eq!(
            statistics(&[9]),
            Some(Statistics {
                n: 1,
                median_ns: 9,
                p95_ns: 9,
                max_ns: 9,
                sum_ns: 9
            })
        );
        assert_eq!(
            statistics(&[40, 10, 20, 30]),
            Some(Statistics {
                n: 4,
                median_ns: 25,
                p95_ns: 40,
                max_ns: 40,
                sum_ns: 100
            })
        );
        let mut samples: Vec<u128> = (1..=20).collect();
        samples.reverse();
        assert_eq!(
            statistics(&samples),
            Some(Statistics {
                n: 20,
                median_ns: 10,
                p95_ns: 19,
                max_ns: 20,
                sum_ns: 210
            })
        );
    }
    #[test]
    fn cooperative_budget_observes_cancel_and_both_deadlines() {
        let now = Instant::now();
        assert!(budget(now, now, &AtomicBool::new(false)));
        assert!(!budget(now, now, &AtomicBool::new(true)));
        assert!(!budget(
            now - Duration::from_secs(46),
            now,
            &AtomicBool::new(false)
        ));
        assert!(!budget(
            now,
            now - Duration::from_secs(11),
            &AtomicBool::new(false)
        ));
    }
}
