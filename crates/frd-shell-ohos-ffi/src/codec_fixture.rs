//! 显式 debug 夹具：正常应用上下文中的真实 FFmpeg loader/API；不注册会话能力。
use super::codec_fixture_common::*;
#[cfg(test)]
use frd_core::{PixelRect, PixelSize};
use frd_media_api::*;
#[cfg(target_env = "ohos")]
use frd_video_ffmpeg::FfmpegBackend;
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[cfg(target_env = "ohos")]
use std::{sync::Arc, thread::JoinHandle};

#[cfg(target_env = "ohos")]
pub(crate) struct Worker {
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
#[cfg(target_env = "ohos")]
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
            .name("frd-codec-fixture".into())
            .spawn(move || {
                let _scope = crate::panic_scope::Scope::enter();
                let result = std::panic::catch_unwind(|| run(&cancel));
                if result.is_err() {
                    log("fixture=4c3e completed=0 gate=Panic");
                }
            })
            .ok();
        if worker.handle.is_none() {
            log("fixture=4c3e completed=0 gate=WorkerSpawn");
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
#[cfg(target_env = "ohos")]
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.reap();
        // 原生同步调用不能安全强制中断。独立线程持有所有资源，自行返回后释放；UI 从不 join 未完成线程。
    }
}

fn check_budget(start: Instant, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(Failure::Cancelled);
    }
    if start.elapsed() > Duration::from_secs(45) {
        return Err(Failure::Deadline);
    }
    Ok(())
}
#[cfg(target_env = "ohos")]
fn run(cancel: &AtomicBool) {
    let started = Instant::now();
    log("fixture=4c3e version=1 begin=1 network=0 registry=0 wait_us=0");
    let mut passed = 0;
    let mut failed = 0;
    for case in cases() {
        for length in [false, true] {
            if case.hevc && length {
                continue;
            }
            if let Err(error) = check_budget(started, cancel) {
                log(&format!(
                    "fixture=4c3e completed=0 gate={error:?} passed={passed} failed={failed}"
                ));
                return;
            }
            match run_case(&case, length, started, cancel) {
                Ok(()) => {
                    passed += 1;
                    log(&format!(
                        "fixture=4c3e case={} length={} lifecycle=pass",
                        case.id,
                        u8::from(length)
                    ));
                }
                Err(error) => {
                    failed += 1;
                    log(&format!(
                        "fixture=4c3e case={} length={} gate={error:?} lifecycle=fail",
                        case.id,
                        u8::from(length)
                    ));
                }
            }
        }
    }
    log(&format!(
        "fixture=4c3e completed=1 passed={passed} failed={failed} elapsed_us={}",
        started.elapsed().as_micros()
    ));
}

#[cfg(target_env = "ohos")]
fn submit(
    case: &Case,
    config: &VideoStreamConfig,
    decoder: &mut dyn VideoDecoder,
    index: usize,
    generation: u64,
) -> Result<DecodedVideoFrame> {
    let au = access(config, case.units[index], generation, index)?;
    let start = Instant::now();
    let result = decoder.submit(au);
    let elapsed = start.elapsed().as_micros();
    let count = match &result {
        Ok(DecodeOutcome::Frames(f)) => f.len(),
        _ => 0,
    };
    log(&format!("fixture=4c3e case={} generation={generation} au={index} before_eos_frames={count} decode_us={elapsed} wait_us=0", case.id));
    let frame = one_frame(result?)?;
    verify_frame(case, config, &frame, index, generation)?;
    let f = frame.as_input();
    log(&format!("fixture=4c3e case={} au={index} pixels=pass coded={}x{} visible={}x{} chroma={} strides={},{},{}", case.id, f.coded_size.width, f.coded_size.height, f.visible_rect.width, f.visible_rect.height, if case.y420 {420} else {444}, f.planes[0].stride_bytes(), f.planes[1].stride_bytes(), f.planes[2].stride_bytes()));
    Ok(frame)
}
#[cfg(target_env = "ohos")]
fn run_case(case: &Case, length: bool, start: Instant, cancel: &AtomicBool) -> Result<()> {
    let config = config(case, length)?;
    let backend = FfmpegBackend::load()?;
    log(&format!("fixture=4c3e case={} load=pass", case.id));
    let mut decoder = backend.create(&config)?;
    for index in 0..case.units.len() {
        check_budget(start, cancel)?;
        submit(case, &config, decoder.as_mut(), index, 1)?;
    }
    check_budget(start, cancel)?;
    decoder.reset(2)?;
    let stale = decoder.submit(access(&config, case.units[0], 1, 0)?);
    if !matches!(stale, Err(ref e) if e.code() == VideoDecodeErrorCode::StaleStreamOrGeneration) {
        return Err(Failure::StaleAccepted);
    }
    let frame = submit(case, &config, decoder.as_mut(), 0, 2)?;
    drop(decoder);
    // 同一 backend 反复创建/销毁，第三个 decoder 在 backend 释放后仍持有真实插件租约。
    for _ in 0..2 {
        check_budget(start, cancel)?;
        let mut next = backend.create(&config)?;
        submit(case, &config, next.as_mut(), 0, 1)?;
    }
    let mut retained = backend.create(&config)?;
    drop(backend);
    submit(case, &config, retained.as_mut(), 0, 1)?;
    drop(retained);
    verify_frame(case, &config, &frame, 0, 2)?;
    log(&format!(
        "fixture=4c3e case={} reset=pass stale=pass repeat=pass owned_after_drop=pass",
        case.id
    ));
    check_budget(start, cancel)?;
    // 长度前缀畸形输入在生产 framing 层确定拒绝；AnnexB 路径喂入无效 VCL。
    let backend = FfmpegBackend::load()?;
    if case.hevc {
        // FFmpeg 可跳过无效 VCL；单独记录原候选结果，不将 NeedMoreData 当作畸形拒绝通过。
        let mut probe = backend.create(&config)?;
        let probe_start = Instant::now();
        let outcome = probe.submit(raw_access(&config, vec![0, 0, 0, 1, 0x26, 1, 0xff], 1, 0)?);
        log_outcome(
            case.id,
            "truncated_vcl_observation",
            &outcome,
            probe_start.elapsed(),
        );
        drop(probe);
        check_budget(start, cancel)?;
    }
    let mut broken = backend.create(&config)?;
    let bad = if length {
        vec![0, 0, 0, 9, 0x65]
    } else if case.hevc {
        // 已接收 FFmpeg 8.1.2 h2645_parse 明确拒绝没有任何 start code 的完整输入。
        vec![0xff; 8]
    } else {
        vec![0, 0, 0, 1, 0x65, 0xff]
    };
    let malformed_start = Instant::now();
    let result = broken.submit(raw_access(&config, bad, 1, 0)?);
    log_outcome(
        case.id,
        "malformed_rejection",
        &result,
        malformed_start.elapsed(),
    );
    if !matches!(result, Err(ref e) if matches!(e.code(), VideoDecodeErrorCode::MalformedOrOverBudgetAccessUnit | VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame))
    {
        return Err(Failure::MalformedAccepted);
    }
    drop(broken);
    check_budget(start, cancel)?;
    let mut restored = backend.create(&config)?;
    submit(case, &config, restored.as_mut(), 0, 1)?;
    log(&format!(
        "fixture=4c3e case={} malformed=pass recreate=pass",
        case.id
    ));
    check_budget(start, cancel)
}
#[cfg(target_env = "ohos")]
fn log_outcome(
    id: &str,
    probe: &str,
    result: &std::result::Result<DecodeOutcome, VideoDecodeError>,
    elapsed: Duration,
) {
    match result {
        Ok(DecodeOutcome::NeedMoreData) => log(&format!(
            "fixture=4c3e case={id} probe={probe} outcome=NeedMoreData elapsed_us={}",
            elapsed.as_micros()
        )),
        Ok(DecodeOutcome::Frames(frames)) => log(&format!(
            "fixture=4c3e case={id} probe={probe} outcome=Frames frames={} elapsed_us={}",
            frames.len(),
            elapsed.as_micros()
        )),
        Err(error) => log(&format!(
            "fixture=4c3e case={id} probe={probe} outcome=Error code={:?} elapsed_us={}",
            error.code(),
            elapsed.as_micros()
        )),
    }
}
#[cfg(target_env = "ohos")]
fn log(text: &str) {
    #[cfg(target_env = "ohos")]
    {
        #[link(name = "hilog_ndk.z")]
        extern "C" {
            fn OH_LOG_Print(
                t: u32,
                l: u32,
                d: u32,
                tag: *const std::ffi::c_char,
                fmt: *const std::ffi::c_char,
                ...
            ) -> i32;
        }
        if let Ok(text) = std::ffi::CString::new(text) {
            // 固定字段/枚举及合成夹具标识，不记录路径、原始错误或远程像素。
            unsafe {
                OH_LOG_Print(
                    0,
                    4,
                    0xFF00,
                    c"FrdCodecProbe".as_ptr(),
                    c"%{public}s".as_ptr(),
                    text.as_ptr(),
                );
            }
        }
    }
    #[cfg(not(target_env = "ohos"))]
    {
        let _ = text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inputs_have_parameter_sets_and_exact_static_provenance() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../tests/codec-fixtures/provenance.json")).unwrap();
        for case in cases() {
            assert!(config(&case, false).is_ok());
            if !case.hevc {
                assert!(config(&case, true).is_ok());
            }
            for (index, reference) in case.references.iter().enumerate() {
                assert_eq!(
                    format!("{:x}", Sha256::digest(reference)),
                    case.hashes[index]
                );
            }
            for (index, bytes) in case.units.iter().enumerate() {
                let expected = if let Some(small) = manifest["small"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["file"] == case.id)
                {
                    small["sha256"].as_str().unwrap()
                } else {
                    manifest["large"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|v| v["name"] == case.id)
                        .unwrap()["access_units"][index]["sha256"]
                        .as_str()
                        .unwrap()
                };
                assert_eq!(format!("{:x}", Sha256::digest(bytes)), expected);
            }
        }
    }
    #[test]
    fn tiny_avc_uses_sps_coded_macroblock_and_separate_visible_crop() {
        let case = cases().remove(0);
        let config = config(&case, false).unwrap();
        assert_eq!(
            config.as_input().coded_size,
            PixelSize::new(16, 16).unwrap()
        );
        assert_eq!(
            config.as_input().visible_rect,
            PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2
            }
        );
        assert_eq!(case.references[0].len(), 6);
    }
    #[test]
    fn no_frame_before_eos_is_a_failure() {
        assert!(matches!(
            one_frame(DecodeOutcome::NeedMoreData),
            Err(Failure::BeforeEos)
        ));
        assert!(matches!(
            one_frame(DecodeOutcome::Frames(Box::default())),
            Err(Failure::BeforeEos)
        ));
    }
    #[test]
    fn annexb_fixture_splitter_rejects_missing_empty_or_trailing_nal() {
        for bytes in [&[][..], &[7, 8, 9], &[0, 0, 1], &[0, 0, 1, 7, 0, 0, 1]] {
            assert!(nals(bytes).is_err());
        }
        assert_eq!(
            nals(&[0, 0, 1, 7, 8, 0, 0, 0, 1, 9]).unwrap(),
            vec![&[7, 8][..], &[9][..]]
        );
    }
    fn padded_frame(case: &Case, config: &VideoStreamConfig) -> DecodedVideoFrame {
        let mut offset = 0;
        let mut planes = Vec::new();
        for i in 0..3 {
            let d = if i > 0 && case.y420 { 2 } else { 1 };
            let width = case.width.div_ceil(d);
            let height = case.height.div_ceil(d);
            let visible_width = case.visible_width.div_ceil(d) as usize;
            let visible_height = case.visible_height.div_ceil(d) as usize;
            let stride = width + 7;
            let mut data = vec![0xa5; (stride * height) as usize];
            for row in 0..visible_height {
                data[row * stride as usize..row * stride as usize + visible_width]
                    .copy_from_slice(&case.references[0][offset..offset + visible_width]);
                offset += visible_width;
            }
            planes
                .push(VideoPlane::try_new(width, height, stride, data.into_boxed_slice()).unwrap());
        }
        DecodedVideoFrame::try_new(DecodedVideoFrameInput {
            identity: config.as_input().identity,
            generation: 1,
            timestamp: VideoTimestamp {
                ticks: 0,
                timescale: config.as_input().time_base.ticks_per_second(),
            },
            coded_size: config.as_input().coded_size,
            visible_rect: config.as_input().visible_rect,
            format: if case.y420 {
                VideoPixelFormat::Yuv420P8
            } else {
                VideoPixelFormat::Yuv444P8
            },
            colorimetry: VideoColorimetry::Bt709,
            range: VideoRange::Limited,
            planes: planes.into_boxed_slice(),
        })
        .unwrap()
    }
    #[test]
    fn visible_parity_ignores_stride_and_cropped_allocation_padding() {
        for case in cases().into_iter().filter(|c| !c.references.is_empty()) {
            let config = config(&case, false).unwrap();
            let frame = padded_frame(&case, &config);
            assert_eq!(verify_frame(&case, &config, &frame, 0, 1), Ok(()));
            assert_eq!(
                verify_frame(&case, &config, &frame, 0, 2),
                Err(Failure::Geometry)
            );
            let mut changed = frame.as_input().clone();
            let p = &changed.planes[0];
            let mut bytes = p.bytes().to_vec();
            bytes[0] ^= 1;
            changed.planes[0] = VideoPlane::try_new(
                p.width(),
                p.height(),
                p.stride_bytes(),
                bytes.into_boxed_slice(),
            )
            .unwrap();
            assert_eq!(
                verify_frame(
                    &case,
                    &config,
                    &DecodedVideoFrame::try_new(changed).unwrap(),
                    0,
                    1
                ),
                Err(Failure::Pixels)
            );
        }
    }
    #[test]
    fn length_prefixed_fixture_input_preserves_each_nal_and_temporal_identity() {
        let case = cases().remove(0);
        let config = config(&case, true).unwrap();
        let au = access(&config, case.units[0], 2, 1).unwrap();
        assert_eq!(au.generation(), 2);
        assert_eq!(au.timestamp().ticks, 3000);
        assert!(!au.random_access());
        let mut bytes = au.bytes();
        for nal in nals(case.units[0]).unwrap() {
            let size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
            assert_eq!(&bytes[4..4 + size], nal);
            bytes = &bytes[4 + size..];
        }
        assert!(bytes.is_empty());
    }
    #[test]
    fn cancellation_and_deadline_are_separate_failures() {
        assert_eq!(
            check_budget(Instant::now(), &AtomicBool::new(true)),
            Err(Failure::Cancelled)
        );
        assert_eq!(
            check_budget(
                Instant::now() - Duration::from_secs(46),
                &AtomicBool::new(false)
            ),
            Err(Failure::Deadline)
        );
    }
}
