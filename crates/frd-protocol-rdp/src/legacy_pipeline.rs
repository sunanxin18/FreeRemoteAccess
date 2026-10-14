// A2.10A test-only：真实 adapter 调用，独立原版冻结数据与持续 canvas。
use super::*;
use sha2::{Digest, Sha256};
use std::time::Instant;

fn bundle_bytes() -> Vec<u8> {
    let p = std::env::var_os("FRD_PIPELINE_DATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/pipeline-workloads-v3/workloads.bin")
        });
    std::fs::read(&p).unwrap_or_else(|e| panic!("独立oracle bundle未stage {}: {e}", p.display()))
}
const FORMATS: [IronPixelFormat; 8] = [
    IronPixelFormat::ARgb32,
    IronPixelFormat::XRgb32,
    IronPixelFormat::ABgr32,
    IronPixelFormat::XBgr32,
    IronPixelFormat::BgrA32,
    IronPixelFormat::BgrX32,
    IronPixelFormat::RgbA32,
    IronPixelFormat::RgbX32,
];
#[derive(Clone)]
struct Expected {
    status: u8,
    image: [u8; 32],
    canvas: [u8; 32],
    outputs: String,
    anchors: [u8; 12],
    projection: Vec<u8>,
}
#[derive(Clone)]
struct Step {
    kind: u8,
    drain: bool,
    domain: u8,
    data: Vec<u8>,
    rects: Vec<[u16; 4]>,
    expected: Expected,
    source_pixels: u64,
}
struct Work {
    name: String,
    width: u16,
    height: u16,
    format: u8,
    pointer: bool,
    full: bool,
    batches: [Vec<Step>; 3],
}
struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        a
    }
    fn byte(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take(2).try_into().unwrap())
    }
    fn u32(&mut self) -> usize {
        u32::from_le_bytes(self.take(4).try_into().unwrap()) as usize
    }
    fn field(&mut self) -> &'a [u8] {
        let n = self.u32();
        self.take(n)
    }
}
fn workloads() -> Vec<Work> {
    let bytes = bundle_bytes();
    let mut c = Cursor(&bytes);
    assert_eq!(c.take(8), b"FRDBND01");
    let count = c.u32();
    let mut out = Vec::new();
    for _ in 0..count {
        let mut w = Cursor(c.field());
        let mut e = Cursor(c.field());
        assert_eq!(w.take(8), b"FRDPIPE1");
        assert_eq!(e.take(8), b"FRDEXP01");
        let name = String::from_utf8(w.field().to_vec()).unwrap();
        let (width, height, format, pointer, full) =
            (w.u16(), w.u16(), w.byte(), w.byte() != 0, w.byte() != 0);
        let batches = std::array::from_fn(|_| {
            let n = w.u32();
            (0..n)
                .map(|_| {
                    let (kind, drain, domain) = (w.byte(), w.byte() != 0, w.byte());
                    let data = w.field().to_vec();
                    let count = w.u32();
                    let rects = (0..count)
                        .map(|_| [w.u16(), w.u16(), w.u16(), w.u16()])
                        .collect();
                    let expected = Expected {
                        status: e.byte(),
                        image: e.take(32).try_into().unwrap(),
                        canvas: e.take(32).try_into().unwrap(),
                        outputs: String::from_utf8(e.field().to_vec()).unwrap(),
                        anchors: e.take(12).try_into().unwrap(),
                        projection: e.field().to_vec(),
                    };
                    let source_pixels = source_pixels(kind, domain, &data);
                    Step {
                        kind,
                        drain,
                        domain,
                        data,
                        rects,
                        source_pixels,
                        expected,
                    }
                })
                .collect()
        });
        assert!(w.0.is_empty() && e.0.is_empty());
        out.push(Work {
            name,
            width,
            height,
            format,
            pointer,
            full,
            batches,
        });
    }
    assert!(c.0.is_empty());
    out
}
fn hash(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}
// 仅在 fixture load 时解析成本；不以这些数据替代 Stage 或 receipt 正确性。
fn source_pixels(kind: u8, domain: u8, data: &[u8]) -> u64 {
    if domain == 2 || kind >= 2 {
        return 0;
    }
    use ironrdp::core::{decode, decode_cursor, ReadCursor};
    use ironrdp::pdu::{
        bitmap::BitmapUpdateData,
        codecs::rfx::{Block, CodecChannel},
        fast_path::{FastPathHeader, FastPathUpdatePdu, UpdateCode},
        surface_commands::SurfaceCommand,
    };
    let bitmap_count = |data: &[u8]| -> u64 {
        let b: BitmapUpdateData<'_> = decode(data).unwrap();
        b.rectangles
            .iter()
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum()
    };
    if kind == 1 {
        let p: ironrdp::pdu::x224::X224<ironrdp::pdu::mcs::SendDataIndication<'_>> =
            decode(data).unwrap();
        return bitmap_count(&p.0.user_data[18..]);
    }
    let mut c = ReadCursor::new(data);
    let header: FastPathHeader = decode_cursor(&mut c).unwrap();
    assert_eq!(header.data_length, c.len());
    assert!(data.len() <= 32767);
    let p: FastPathUpdatePdu<'_> = decode_cursor(&mut c).unwrap();
    match p.update_code {
        UpdateCode::Bitmap => bitmap_count(p.data),
        UpdateCode::SurfaceCommands => {
            let mut c = ReadCursor::new(p.data);
            let mut n = 0;
            while !c.is_empty() {
                let cmd: SurfaceCommand<'_> = decode_cursor(&mut c).unwrap();
                if let SurfaceCommand::SetSurfaceBits(b) | SurfaceCommand::StreamSurfaceBits(b) =
                    cmd
                {
                    if b.extended_bitmap_data.codec_id == 3 {
                        let mut r = ReadCursor::new(b.extended_bitmap_data.data);
                        while !r.is_empty() {
                            let block: Block<'_> = decode_cursor(&mut r).unwrap();
                            if let Block::CodecChannel(CodecChannel::TileSet(t)) = block {
                                n += t.tiles.len() as u64 * 4096;
                            }
                        }
                    }
                }
            }
            n
        }
        _ => 0,
    }
}
fn events(data: &[u8]) -> [ironrdp::pdu::input::fast_path::FastPathInputEvent; 1] {
    use ironrdp::pdu::input::{
        fast_path::FastPathInputEvent,
        mouse::{MousePdu, PointerFlags},
    };
    [FastPathInputEvent::MouseEvent(MousePdu {
        flags: PointerFlags::MOVE,
        number_of_wheel_rotation_units: 0,
        x_position: u16::from_le_bytes(data[..2].try_into().unwrap()),
        y_position: u16::from_le_bytes(data[2..].try_into().unwrap()),
    })]
}
struct Pipeline {
    h: CoverageHarness,
    canvas: Vec<u8>,
    known: Vec<bool>,
    generation: u64,
    started: bool,
    last_revision: u64,
    starts: usize,
    patches: u64,
    published_bytes: u64,
    queued_entries_before_drain: u64,
    decoded_pixels: u64,
    last_ns: u128,
    setup_ns: u128,
    input_bytes: u64,
}
impl Pipeline {
    fn new(w: &Work, native: bool) -> Self {
        let setup_start = Instant::now();
        let mut h = CoverageHarness::with_entry_limit(w.width, w.height, native, w.pointer, 3);
        if w.format != 6 {
            h.image = DecodedImage::new(FORMATS[w.format as usize], w.width, w.height);
        }
        let setup_ns = setup_start.elapsed().as_nanos();
        #[cfg(frd_pipeline_alloc)]
        let _ = allocations::end();
        Self {
            h,
            canvas: vec![0; w.width as usize * w.height as usize * 4],
            known: vec![false; w.width as usize * w.height as usize],
            generation: 1,
            started: false,
            last_revision: 0,
            starts: 0,
            patches: 0,
            published_bytes: 0,
            queued_entries_before_drain: 0,
            decoded_pixels: 0,
            last_ns: 0,
            setup_ns,
            input_bytes: 0,
        }
    }
    fn replace(&mut self, w: &Work) {
        let start = Instant::now();
        self.generation = commit_reactivated_surface(
            &mut self.h.runtime,
            &mut self.h.baseline,
            &mut self.h.image,
            self.generation,
            DesktopSize {
                width: w.width,
                height: w.height,
            },
        )
        .unwrap();
        self.h.stage.set_fastpath_processor(
            ironrdp::session::fast_path::ProcessorBuilder {
                io_channel_id: 1003,
                user_channel_id: 1001,
                share_id: 2,
                enable_server_pointer: w.pointer,
                pointer_software_rendering: w.pointer,
                bulk_decompressor: None,
            }
            .build(),
        );
        self.last_ns = start.elapsed().as_nanos();
        self.known.fill(false);
        self.canvas.fill(0);
        self.started = false;
        self.last_revision = 0;
    }
    fn step(&mut self, w: &Work, s: &Step, check: bool) -> Vec<frd_frame::FrameTransaction> {
        if s.kind == 2 {
            self.replace(w);
            return vec![];
        }
        if s.kind == 4 {
            let start = Instant::now();
            self.h.stage.update_mouse_pos(
                u16::from_le_bytes(s.data[..2].try_into().unwrap()),
                u16::from_le_bytes(s.data[2..].try_into().unwrap()),
            );
            self.last_ns = start.elapsed().as_nanos();
            return vec![];
        }
        let before = if check && !w.full {
            Some(self.h.image.data().to_vec())
        } else {
            None
        };
        let start = Instant::now();
        let batch = if s.kind == 3 {
            super::super::process_input_batch(
                &mut self.h.stage,
                &mut self.h.image,
                self.generation,
                &events(&s.data),
            )
        } else {
            super::super::process_server_batch(
                &mut self.h.stage,
                &mut self.h.image,
                self.generation,
                if s.kind == 1 {
                    ironrdp::pdu::Action::X224
                } else {
                    ironrdp::pdu::Action::FastPath
                },
                &s.data,
            )
        }
        .unwrap_or_else(|e| panic!("{} domain={} actual batch failed: {e:?}", w.name, s.domain));
        if check && s.domain == 0 {
            assert_eq!(s.expected.status, 0, "共同合法域必须有原版成功");
            assert_eq!(
                format!("{:?}", batch.result.outputs),
                s.expected.outputs,
                "{} Stage outputs",
                w.name
            );
        }
        // stage receipts 单独核对 fixture geometry，不使用 receipts 生成 expected coverage。
        if check {
            let received: Vec<_> = batch
                .result
                .decoded_coverage
                .iter()
                .flat_map(|r| {
                    r.rectangles
                        .iter()
                        .map(|r| [r.left, r.top, r.right, r.bottom])
                })
                .collect();
            let declared = s.rects.clone();
            if check {
                assert_eq!(received, declared, "{} decoded geometry", w.name);
            }
        }
        super::super::route_active_outputs(
            batch,
            &mut self.h.writer,
            &mut self.h.runtime,
            &mut self.h.baseline,
            w.full,
        )
        .unwrap();
        let decode_route_ns = start.elapsed().as_nanos();
        // 诊断采样的额外锁不进入最终 release 总计时；不是 push outcome。
        self.queued_entries_before_drain += self.h.mailbox.lock().unwrap().len() as u64;
        let compile_start = Instant::now();
        let transactions = if s.drain && w.full {
            self.h.compile()
        } else {
            vec![]
        };
        self.last_ns = decode_route_ns + compile_start.elapsed().as_nanos();
        if let Some(before) = before {
            let mut untouched = 0;
            for y in 0..w.height {
                for x in 0..w.width {
                    if !s
                        .rects
                        .iter()
                        .any(|r| x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3])
                    {
                        let i = (y as usize * w.width as usize + x as usize) * 4;
                        assert_eq!(
                            &self.h.image.data()[i..i + 4],
                            &before[i..i + 4],
                            "{} untouched x={} y={} all alpha/X bytes",
                            w.name,
                            x,
                            y
                        );
                        untouched += 1;
                    }
                }
            }
            if s.source_pixels == 8192 {
                assert!(
                    untouched > 0,
                    "vendor region fixture must retain untouched pixels"
                );
            }
        }
        for r in &s.rects {
            for y in r[1]..=r[3] {
                for x in r[0]..=r[2] {
                    self.known[y as usize * w.width as usize + x as usize] = true;
                }
            }
        }
        self.decoded_pixels += s.source_pixels;
        if s.kind <= 1 {
            self.input_bytes += s.data.len() as u64;
        }
        if check && w.full {
            assert_eq!(
                self.h.baseline.known_pixels(),
                self.known.iter().filter(|&&v| v).count() as u64,
                "{} exact content",
                w.name
            );
            assert_eq!(
                self.h.baseline.baseline_established(),
                self.known.iter().all(|&v| v),
                "{} content-known",
                w.name
            );
        }

        transactions
    }
    fn consume(&mut self, w: &Work, ts: Vec<frd_frame::FrameTransaction>, check: bool) {
        for t in ts {
            let startup = matches!(t, frd_frame::FrameTransaction::Startup { .. });
            let revision = match t {
                frd_frame::FrameTransaction::Startup { revision, .. }
                | frd_frame::FrameTransaction::Revision { revision, .. } => revision,
            };
            if check {
                assert_eq!(startup, !self.started, "{} unique Startup", w.name);
                assert!(
                    self.known.iter().all(|&v| v),
                    "Startup/Revision 不能跨未知内容"
                );
                assert_eq!(revision.generation, self.generation);
                assert!(revision.revision > self.last_revision);
                assert_eq!(
                    revision.completeness,
                    if startup {
                        FrameCompleteness::FullBaseline
                    } else {
                        FrameCompleteness::Incremental
                    }
                );
            }
            if startup {
                self.starts += 1;
                self.canvas.fill(0);
            }
            self.started = true;
            self.last_revision = revision.revision;
            for p in revision.patches {
                self.patches += 1;
                self.published_bytes += p.pixels.as_bytes().len() as u64;
                for y in 0..p.rect.height as usize {
                    let dst = ((p.rect.y as usize + y) * w.width as usize + p.rect.x as usize) * 4;
                    let src = y * p.stride_bytes as usize;
                    let n = p.rect.width as usize * 4;
                    self.canvas[dst..dst + n].copy_from_slice(&p.pixels.as_bytes()[src..src + n]);
                }
            }
        }
    }
    fn check_image(&self, w: &Work, s: &Step) {
        if w.full && s.drain && self.started {
            let expected = if s.domain == 1 {
                &s.expected.projection[32..64]
            } else {
                &s.expected.canvas[..]
            };
            assert_eq!(
                hash(&self.canvas).as_slice(),
                expected,
                "{} persistent compiler canvas",
                w.name
            );
        }
        let (expected, anchors) = if s.domain == 1 {
            assert_eq!(s.expected.projection.len(), 108);
            (&s.expected.projection[..32], &s.expected.projection[64..76])
        } else {
            (&s.expected.image[..], &s.expected.anchors[..])
        };
        assert_eq!(
            hash(self.h.image.data()).as_slice(),
            expected,
            "{} image",
            w.name
        );
        let mut actual: Vec<u8> = Vec::new();
        for i in [
            0,
            self.h.image.data().len() / 8 * 4,
            self.h.image.data().len() - 4,
        ] {
            actual.extend(&self.h.image.data()[i..i + 4]);
        }
        assert_eq!(actual, anchors, "{} literal alpha/X/edge anchors", w.name);
    }
    fn batch(&mut self, w: &Work, batch: &[Step], check: bool) -> u128 {
        let mut ns = 0;
        for s in batch {
            let ts = self.step(w, s, check);
            ns += self.last_ns;
            self.consume(w, ts, check);
            if check {
                self.check_image(w, s);
            }
        }
        ns
    }
}
fn check_pipeline_workloads(quick: bool) {
    let mut ws = workloads();
    if quick {
        ws.retain(|w| w.width <= 128 && w.height <= 64);
    }
    for w in &ws {
        for native in [false, true] {
            if w.batches.iter().flatten().any(|s| s.domain == 2) {
                failure_lane(w, native);
                continue;
            }
            let mut p = Pipeline::new(w, native);
            for b in &w.batches {
                p.batch(w, b, true);
            }
            if w.full {
                assert!(p.started, "{} 必须实际 Startup", w.name);
            }
        }
    }
    // 针对原版/native差异的 pending-pointer 路径，沿相同实际工厂/builder。
    legacy_native_factory_stage_binding_preserves_pending_pointer_on_real_failure();
    // 同包端点形成跨未知中部的完整 damage bbox，仍须真实中部内容才能 Startup。
    legacy_native_factory_reactivation_starts_empty_and_requires_real_full_content();
    decoded_coverage_batch_rejects_bad_associations_and_late_regions_before_effects();
    decoded_coverage_failed_and_underfilled_updates_do_not_overwrite_known_pixels();
    decoded_coverage_real_queue_retry_failure_keeps_knowledge_then_compiles_full_retry();
    decoded_coverage_stage_error_retains_prior_image_writes_without_publishing_metadata();
    println!(
        "PIPELINE_CORRECTNESS subset={} workloads={} backends=2 frozen_sha={:x}",
        quick,
        ws.len(),
        Sha256::digest(bundle_bytes())
    );
}
#[test]
#[ignore = "complete independent original checkpoint gate"]
fn pipeline_original_checkpoints_real_factory_to_canvas() {
    check_pipeline_workloads(false);
}
#[test]
#[ignore = "explicit bounded small/RFX/vendor/pointer/failure/control subset only"]
fn pipeline_original_checkpoints_quick_subset() {
    check_pipeline_workloads(true);
}
fn failure_lane(w: &Work, native: bool) {
    let mut p = Pipeline::new(w, native);
    let s = &w.batches[0][0];
    let result = super::super::process_server_batch(
        &mut p.h.stage,
        &mut p.h.image,
        1,
        ironrdp::pdu::Action::FastPath,
        &s.data,
    );
    if w.name.ends_with("second-color") && !native {
        let batch = result.expect(
            "Compatibility checked-color difference must retain original successful update",
        );
        assert_eq!(s.expected.status, 0);
        assert_eq!(format!("{:?}", batch.result.outputs), s.expected.outputs);
        assert!(!batch.result.decoded_coverage.is_empty());
        drop(batch);
        assert_eq!(
            hash(p.h.image.data()),
            s.expected.image,
            "Compatibility successful partial image must equal original"
        );
        assert!(p.h.image.data().iter().any(|&b| b != 0));
        assert_eq!(p.h.baseline.known_pixels(), 0);
        assert_eq!(p.h.mailbox.lock().unwrap().len(), 1);
        println!("PIPELINE_DOMAIN {} Compatibility asserted_original_success=true original_status={} image={:x} (checked-color boundary; no throughput)",w.name,s.expected.status,Sha256::digest(p.h.image.data()));
        return;
    }
    assert!(
        result.is_err(),
        "{} native={} 必须拒绝，不得 scalar retry",
        w.name,
        native
    );
    assert_eq!(p.h.baseline.known_pixels(), 0);
    assert!(!p.h.baseline.baseline_established());
    assert_eq!(p.h.mailbox.lock().unwrap().len(), 1);
    assert!(p.h.compile().is_empty());
    if w.name.ends_with("second-empty-y") {
        assert_eq!(
            hash(p.h.image.data()),
            s.expected.image,
            "{} earlier real tile side effect",
            w.name
        );
        assert!(p.h.image.data().iter().any(|&b| b != 0));
    }
    if w.name.ends_with("header") || w.name.ends_with("strict-underfill") {
        assert!(p.h.image.data().iter().all(|&b| b == 0));
    }
    println!("PIPELINE_FAILURE {} native={} original_status={} original_partial={:?} actual_partial={:x}",w.name,native,s.expected.status,s.expected.image,Sha256::digest(p.h.image.data()));
}
struct WarmStep {
    expected: Expected,
    rgba: Vec<u8>,
    bgrx: Vec<u8>,
}
type WarmReferences = std::collections::HashMap<String, [Vec<WarmStep>; 2]>;
fn warm_references() -> WarmReferences {
    let path = std::env::var_os("FRD_PIPELINE_WARM_REF")
        .expect("stateful warm sidecar must be explicitly staged");
    let bytes = std::fs::read(path).unwrap();
    let mut c = Cursor(&bytes);
    assert_eq!(c.take(8), b"FRDWRM01");
    assert_eq!(c.u32(), 64);
    assert_eq!(c.u32(), 4096);
    let count = c.u32();
    assert_eq!(count, 3);
    let mut out = std::collections::HashMap::new();
    for _ in 0..count {
        let name = String::from_utf8(c.field().to_vec()).unwrap();
        let phases = std::array::from_fn(|_| {
            let n = c.u32();
            (0..n)
                .map(|_| {
                    let mut e = Cursor(c.field());
                    assert_eq!(e.byte(), 0);
                    let expected = Expected {
                        status: 0,
                        image: e.take(32).try_into().unwrap(),
                        canvas: e.take(32).try_into().unwrap(),
                        outputs: String::from_utf8(e.field().to_vec()).unwrap(),
                        anchors: e.take(12).try_into().unwrap(),
                        projection: e.field().to_vec(),
                    };
                    assert!(e.0.is_empty());
                    let mut rows = Cursor(c.field());
                    let row_len = rows.u32();
                    let height = rows.u32();
                    let size = rows.u32();
                    assert_eq!(row_len, 1920 * 4);
                    assert_eq!(height, 1080);
                    assert!(size <= height);
                    let dictionary: Vec<_> = (0..size)
                        .map(|_| {
                            let r = rows.field();
                            assert_eq!(r.len(), row_len);
                            r
                        })
                        .collect();
                    let mut rgba = Vec::with_capacity(row_len * height);
                    for _ in 0..height {
                        rgba.extend(dictionary[rows.u32()]);
                    }
                    assert!(rows.0.is_empty());
                    assert_eq!(hash(&rgba), expected.image);
                    let bgrx: Vec<u8> = rgba
                        .chunks_exact(4)
                        .flat_map(|p| [p[2], p[1], p[0], 255])
                        .collect();
                    assert_eq!(hash(&bgrx), expected.canvas);
                    WarmStep {
                        expected,
                        rgba,
                        bgrx,
                    }
                })
                .collect()
        });
        assert!(out.insert(name, phases).is_none());
    }
    assert!(c.0.is_empty());
    out
}
fn measurement_workloads() -> (Vec<Work>, WarmReferences) {
    let mut ws = workloads();
    let reference = warm_references();
    for w in &mut ws {
        if let Some(phases) = reference.get(&w.name) {
            for phase in 0..2 {
                assert_eq!(w.batches[phase + 1].len(), phases[phase].len());
                for (s, r) in w.batches[phase + 1].iter_mut().zip(&phases[phase]) {
                    s.expected = r.expected.clone();
                }
            }
        }
    }
    (ws, reference)
}
fn check_warm_step(p: &Pipeline, r: &WarmStep) {
    assert!(
        p.h.image.data() == r.rgba.as_slice(),
        "complete steady original RGBA checkpoint"
    );
    if p.started {
        assert!(
            p.canvas == r.bgrx,
            "complete steady original BGRX compiler canvas checkpoint"
        );
    }
}
fn percentile(times: &[u128], p: usize) -> u128 {
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    sorted[((sorted.len() * p).div_ceil(100))
        .saturating_sub(1)
        .min(sorted.len() - 1)]
}
#[test]
#[ignore = "bounded release local pipeline measurements; independent oracle must pass first"]
fn pipeline_release_measurements() {
    let (ws, warm) = measurement_workloads();
    for window in 0..3 {
        for w in &ws {
            if !w.full
                || w.batches
                    .iter()
                    .flatten()
                    .any(|s| s.domain != 0 || s.expected.status != 0)
            {
                continue;
            }
            let mut pair = if window % 2 == 0 {
                [Pipeline::new(w, false), Pipeline::new(w, true)]
            } else {
                let native = Pipeline::new(w, true);
                let compat = Pipeline::new(w, false);
                [compat, native]
            };
            let mut setup = [0u128; 2];
            for i in if window % 2 == 0 { [0, 1] } else { [1, 0] } {
                setup[i] = pair[i].setup_ns;
                let mut seed_total = 0;
                let mut first_startup = None;
                for s in &w.batches[0] {
                    let ts = pair[i].step(w, s, false);
                    seed_total += pair[i].last_ns;
                    if first_startup.is_none()
                        && ts
                            .iter()
                            .any(|t| matches!(t, frd_frame::FrameTransaction::Startup { .. }))
                    {
                        first_startup = Some(seed_total);
                    }
                    pair[i].consume(w, ts, true);
                }
                let cold = first_startup.expect("actual first Startup boundary required");
                println!(
                    "PIPELINE_SEED_TOTAL\t{}\t{}\t{}\t{}",
                    w.name, window, i, seed_total
                );
                assert!(pair[i].started);
                pair[i].check_image(w, w.batches[0].last().unwrap());
                println!(
                    "PIPELINE_COLD\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    w.name,
                    window,
                    i,
                    setup[i],
                    cold,
                    setup[i] + cold,
                    250_000_000u128,
                    setup[i] + cold <= 250_000_000
                );
                for k in 0..64 {
                    pair[i].batch(w, &w.batches[1 + k % 2], false);
                }
            }
            for p in &mut pair {
                p.decoded_pixels = 0;
                p.published_bytes = 0;
                p.patches = 0;
                p.queued_entries_before_drain = 0;
                p.input_bytes = 0;
            }
            let mut times = [Vec::new(), Vec::new()];
            let mut total = [0u128; 2];
            let mut count = 0;
            loop {
                let order = if window % 2 == 0 { [0, 1] } else { [1, 0] };
                for i in order {
                    for k in count..count + 256 {
                        let batch = &w.batches[1 + k % 2];
                        let mut elapsed = 0;
                        let mut transactions = Vec::new();
                        for (index, s) in batch.iter().enumerate() {
                            let ts = pair[i].step(w, s, false);
                            elapsed += pair[i].last_ns;
                            if let Some(reference) = warm.get(&w.name) {
                                pair[i].consume(w, ts, true);
                                check_warm_step(&pair[i], &reference[k % 2][index]);
                            } else {
                                transactions.push(ts);
                            }
                        }
                        times[i].push(elapsed);
                        total[i] += elapsed;
                        for ts in transactions {
                            pair[i].consume(w, ts, true);
                        }
                        if !warm.contains_key(&w.name) {
                            pair[i].check_image(w, batch.last().unwrap());
                        }
                    }
                }
                count += 256;
                if total.iter().all(|&n| n >= 100_000_000) || count == 4096 {
                    break;
                }
            }
            for i in 0..2 {
                println!(
                    "PIPELINE_GRANULARITY\t{}\t{}\t{}\t{}\t{}",
                    w.name,
                    window,
                    i,
                    total[i],
                    total[i] >= 100_000_000
                );
                println!(
                    "PIPELINE_INPUT_BYTES\t{}\t{}\t{}\t{}",
                    w.name, window, i, pair[i].input_bytes
                );
                println!(
                    "PIPELINE_ACTUAL_CALLS\t{}\t{}\t{}\t{}\t{}",
                    w.name,
                    window,
                    i,
                    64,
                    count / 2 * (w.batches[1].len() + w.batches[2].len())
                );
                let p95 = percentile(&times[i], 95);
                let desktop = u64::from(w.width) * u64::from(w.height);
                let touched: u64 = w.batches[1]
                    .iter()
                    .flat_map(|s| &s.rects)
                    .map(|r| (u64::from(r[2] - r[0]) + 1) * (u64::from(r[3] - r[1]) + 1))
                    .sum();
                let budget = if w.width == 1920 && w.height == 1080 {
                    if touched * 10 <= desktop {
                        16_670_000
                    } else {
                        33_340_000
                    }
                } else {
                    33_340_000
                };
                println!(
                    "PIPELINE_WARM_BUDGET\t{}\t{}\t{}\t{}\t{}\t{}",
                    w.name, window, i, budget, touched, desktop
                );
                println!(
                    "PIPELINE_WARM\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:x}",
                    w.name,
                    window,
                    i,
                    count,
                    percentile(&times[i], 50),
                    p95,
                    percentile(&times[i], 99),
                    times[i].iter().max().unwrap(),
                    total[i],
                    pair[i].decoded_pixels,
                    pair[i].published_bytes,
                    pair[i].patches,
                    pair[i].queued_entries_before_drain,
                    p95 <= budget,
                    Sha256::digest(&pair[i].canvas)
                );
            }
            let ratio = percentile(&times[1], 95) as f64 / percentile(&times[0], 95) as f64;
            println!(
                "PIPELINE_RATIO\t{}\t{}\t{}\t{}",
                w.name,
                window,
                ratio,
                ratio <= 1.25
            );
        }
    }
}
// Stage重新协调只指定不足100ms的work/window，C/N与O始终相同wholebatch次数。
fn stage_request() -> Option<Vec<(String, usize, usize)>> {
    std::env::var_os("FRD_PIPELINE_STAGE_REQUEST").map(|p| {
        std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .map(|line| {
                let f: Vec<_> = line.split('\t').collect();
                assert_eq!(f.len(), 3);
                let count: usize = f[2].parse().unwrap();
                assert!((256..=4096).contains(&count) && count % 256 == 0);
                (f[0].to_owned(), f[1].parse().unwrap(), count)
            })
            .collect()
    })
}
struct StageLane {
    stage: ironrdp::session::ActiveStage,
    image: DecodedImage,
    setup_ns: u128,
}
impl StageLane {
    fn new(w: &Work, native: bool) -> Self {
        let start = Instant::now();
        let stage = stage(native, w.pointer);
        let image = DecodedImage::new(FORMATS[w.format as usize], w.width, w.height);
        let setup_ns = start.elapsed().as_nanos();
        Self {
            stage,
            image,
            setup_ns,
        }
    }
    fn step(&mut self, w: &Work, s: &Step) -> Vec<ironrdp::session::ActiveStageOutput> {
        match s.kind {
            2 => {
                self.image = DecodedImage::new(self.image.pixel_format(), w.width, w.height);
                self.stage.set_fastpath_processor(
                    ironrdp::session::fast_path::ProcessorBuilder {
                        io_channel_id: 1003,
                        user_channel_id: 1001,
                        share_id: 2,
                        enable_server_pointer: w.pointer,
                        pointer_software_rendering: w.pointer,
                        bulk_decompressor: None,
                    }
                    .build(),
                );
                vec![]
            }
            3 => {
                let result = self
                    .stage
                    .process_fastpath_input(&mut self.image, &events(&s.data))
                    .unwrap();
                result
            }
            4 => {
                self.stage.update_mouse_pos(
                    u16::from_le_bytes(s.data[..2].try_into().unwrap()),
                    u16::from_le_bytes(s.data[2..].try_into().unwrap()),
                );
                vec![]
            }
            _ => {
                let result = self
                    .stage
                    .process(
                        &mut self.image,
                        if s.kind == 1 {
                            ironrdp::pdu::Action::X224
                        } else {
                            ironrdp::pdu::Action::FastPath
                        },
                        &s.data,
                    )
                    .unwrap();
                result
            }
        }
    }
    fn batch(&mut self, w: &Work, b: &[Step]) {
        for s in b {
            std::hint::black_box(self.step(w, s));
        }
    }
}
#[test]
#[ignore = "bounded release Stage comparison against independent original"]
fn pipeline_release_stage_measurements() {
    let (ws, warm) = measurement_workloads();
    let request = stage_request();
    for window in 0..3 {
        for w in &ws {
            let minimum = if let Some(request) = &request {
                match request
                    .iter()
                    .find(|(name, win, _)| name == &w.name && *win == window)
                {
                    Some((_, _, n)) => *n,
                    None => continue,
                }
            } else {
                256
            };
            if w.batches
                .iter()
                .flatten()
                .any(|s| s.domain != 0 || s.expected.status != 0)
            {
                continue;
            }
            let mut pair = if window % 2 == 0 {
                [StageLane::new(w, false), StageLane::new(w, true)]
            } else {
                let native = StageLane::new(w, true);
                let compat = StageLane::new(w, false);
                [compat, native]
            };
            for i in if window % 2 == 0 { [0, 1] } else { [1, 0] } {
                let lane = &mut pair[i];
                let cold_start = Instant::now();
                lane.batch(w, &w.batches[0]);
                let cold = cold_start.elapsed().as_nanos();
                assert_eq!(
                    hash(lane.image.data()),
                    w.batches[0].last().unwrap().expected.image
                );
                println!(
                    "PIPELINE_STAGE_COLD\t{}\t{}\t{}\t{}\t{}\t{}",
                    w.name,
                    window,
                    i,
                    lane.setup_ns,
                    cold,
                    lane.setup_ns + cold
                );
                for k in 0..64 {
                    lane.batch(w, &w.batches[1 + k % 2]);
                }
            }
            let mut times = [Vec::new(), Vec::new()];
            let mut total = [0u128; 2];
            let mut count = 0;
            loop {
                for i in if window % 2 == 0 { [0, 1] } else { [1, 0] } {
                    for k in count..count + 256 {
                        let b = &w.batches[1 + k % 2];
                        let ns = if let Some(reference) = warm.get(&w.name) {
                            let mut ns = 0;
                            for (index, s) in b.iter().enumerate() {
                                let start = Instant::now();
                                let result = pair[i].step(w, s);
                                ns += start.elapsed().as_nanos();
                                let r = &reference[k % 2][index];
                                assert_eq!(
                                    format!("{result:?}"),
                                    r.expected.outputs,
                                    "{} steady Stage output step{index}",
                                    w.name
                                );
                                assert!(
                                    pair[i].image.data() == r.rgba.as_slice(),
                                    "{} complete steady Stage image step{index}",
                                    w.name
                                );
                            }
                            ns
                        } else {
                            let start = Instant::now();
                            pair[i].batch(w, b);
                            let ns = start.elapsed().as_nanos();
                            assert_eq!(
                                hash(pair[i].image.data()),
                                b.last().unwrap().expected.image,
                                "{} Stage same work",
                                w.name
                            );
                            ns
                        };
                        times[i].push(ns);
                        total[i] += ns;
                    }
                }
                count += 256;
                if (count >= minimum && total.iter().all(|&n| n >= 100_000_000)) || count == 4096 {
                    break;
                }
            }
            for i in 0..2 {
                let source: u64 = (0..count)
                    .map(|k| {
                        w.batches[1 + k % 2]
                            .iter()
                            .map(|s| s.source_pixels)
                            .sum::<u64>()
                    })
                    .sum();
                let input: usize = (0..count)
                    .map(|k| {
                        w.batches[1 + k % 2]
                            .iter()
                            .filter(|s| s.kind <= 1)
                            .map(|s| s.data.len())
                            .sum::<usize>()
                    })
                    .sum();
                let applied: u64 = (0..count)
                    .map(|k| {
                        w.batches[1 + k % 2]
                            .iter()
                            .flat_map(|s| &s.rects)
                            .map(|r| u64::from(r[2] - r[0] + 1) * u64::from(r[3] - r[1] + 1))
                            .sum::<u64>()
                    })
                    .sum();
                println!(
                    "PIPELINE_STAGE_COST\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    w.name,
                    window,
                    i,
                    source,
                    input,
                    applied,
                    count / 2 * (w.batches[1].len() + w.batches[2].len()),
                    total[i] >= 100_000_000,
                    count == 4096
                );
                println!(
                    "PIPELINE_STAGE\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:x}",
                    w.name,
                    window,
                    i,
                    count,
                    percentile(&times[i], 50),
                    percentile(&times[i], 95),
                    percentile(&times[i], 99),
                    times[i].iter().max().unwrap(),
                    total[i],
                    Sha256::digest(pair[i].image.data())
                );
            }
        }
    }
}
// 独立 observed 构建才替换 allocator；正常 release 完全没有该 wrapper。
#[allow(unexpected_cfgs)]
#[cfg(frd_pipeline_alloc)]
mod allocations {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    pub static ENABLED: AtomicBool = AtomicBool::new(false);
    pub static COUNT: AtomicU64 = AtomicU64::new(0);
    pub static BYTES: AtomicU64 = AtomicU64::new(0);
    pub static LIVE: AtomicU64 = AtomicU64::new(0);
    pub static PEAK: AtomicU64 = AtomicU64::new(0);
    static START_LIVE: AtomicU64 = AtomicU64::new(0);
    static LAST: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
    struct Observed;
    unsafe impl GlobalAlloc for Observed {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(l) };
            if !p.is_null() {
                let n = LIVE.fetch_add(l.size() as u64, Ordering::Relaxed) + l.size() as u64;
                if ENABLED.load(Ordering::Relaxed) {
                    COUNT.fetch_add(1, Ordering::Relaxed);
                    BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
                    PEAK.fetch_max(n, Ordering::Relaxed);
                }
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            {
                let _ = LIVE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    Some(n.saturating_sub(l.size() as u64))
                });
            }
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            let out = unsafe { System.realloc(p, l, n) };
            if !out.is_null() {
                let _ = LIVE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(l.size() as u64) + n as u64)
                });
                if ENABLED.load(Ordering::Relaxed) {
                    COUNT.fetch_add(1, Ordering::Relaxed);
                    BYTES.fetch_add(n as u64, Ordering::Relaxed);
                    PEAK.fetch_max(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
                }
            }
            out
        }
    }
    #[global_allocator]
    static ALLOC: Observed = Observed;
    pub fn begin() {
        for c in [&COUNT, &BYTES] {
            c.store(0, Ordering::Relaxed);
        }
        let live = LIVE.load(Ordering::Relaxed);
        START_LIVE.store(live, Ordering::Relaxed);
        PEAK.store(live, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
    }
    pub fn pause() {
        ENABLED.store(false, Ordering::Relaxed);
    }
    pub fn resume() {
        ENABLED.store(true, Ordering::Relaxed);
    }
    // 全进程live含预载fixture与audit buffers；这里只观测enabled调用的alloc。
    // 返回count,bytes,startLive,endLive,absolutePeak,relativeAddedPeak；峰值不可求和。
    pub fn end() -> [u64; 6] {
        if !ENABLED.swap(false, Ordering::Relaxed) {
            return std::array::from_fn(|i| LAST[i].load(Ordering::Relaxed));
        }
        let start = START_LIVE.load(Ordering::Relaxed);
        let peak = PEAK.load(Ordering::Relaxed);
        let result = [
            COUNT.load(Ordering::Relaxed),
            BYTES.load(Ordering::Relaxed),
            start,
            LIVE.load(Ordering::Relaxed),
            peak,
            peak.saturating_sub(start),
        ];
        for (cell, value) in LAST.iter().zip(result) {
            cell.store(value, Ordering::Relaxed);
        }
        result
    }
}
#[allow(unexpected_cfgs)]
#[cfg(frd_pipeline_alloc)]
#[test]
#[ignore = "separate observed allocation lane; never use its timings as final release totals"]
fn pipeline_observed_allocations() {
    let (ws, warm) = measurement_workloads();
    for w in ws {
        if !w.full
            || w.batches
                .iter()
                .flatten()
                .any(|s| s.domain != 0 || s.expected.status != 0)
        {
            continue;
        }
        for native in [false, true] {
            allocations::begin();
            let mut p = Pipeline::new(&w, native);
            let setup = allocations::end();
            allocations::begin();
            let mut cold_ns = 0;
            for s in &w.batches[0] {
                let ts = p.step(&w, s, false);
                cold_ns += p.last_ns;
                allocations::pause();
                p.consume(&w, ts, true);
                allocations::resume();
            }
            let cold = allocations::end();
            for k in 0..64 {
                p.batch(&w, &w.batches[1 + k % 2], false);
            }
            let mut ns = 0;
            allocations::begin();
            for k in 0..256 {
                for (index, s) in w.batches[1 + k % 2].iter().enumerate() {
                    let ts = p.step(&w, s, false);
                    ns += p.last_ns;
                    allocations::pause();
                    p.consume(&w, ts, false);
                    if let Some(reference) = warm.get(&w.name) {
                        check_warm_step(&p, &reference[k % 2][index]);
                    }
                    allocations::resume();
                }
            }
            let counts = allocations::end();
            p.check_image(&w, w.batches[2].last().unwrap());
            println!(
                "PIPELINE_OBSERVED\t{}\t{}\t{:?}\t{:?}\t{:?}\t{}\t{}",
                w.name, native, setup, cold, counts, cold_ns, ns
            );
        }
    }
}
#[test]
#[ignore = "separate cold/replacement/overflow diagnostic phases"]
fn pipeline_release_control_measurements() {
    let ws = workloads();
    let w = ws
        .iter()
        .find(|w| w.name == "control-holes-replace-overflow")
        .unwrap();
    for window in 0..3 {
        for native in if window % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let mut p = Pipeline::new(w, native);
            let mut elapsed = p.setup_ns;
            let mut first = false;
            let mut replacement = 0;
            for s in &w.batches[0] {
                let ts = p.step(w, s, false);
                elapsed += p.last_ns;
                if s.kind == 2 {
                    replacement = p.last_ns;
                    elapsed = p.last_ns;
                }
                let is_start = ts
                    .iter()
                    .any(|t| matches!(t, frd_frame::FrameTransaction::Startup { .. }));
                p.consume(w, ts, true);
                p.check_image(w, s);
                if is_start {
                    println!(
                        "PIPELINE_CONTROL\t{}\t{}\t{}\t{}\t{}\t{}",
                        window,
                        native,
                        if first {
                            "replacement-recovery"
                        } else {
                            "first-startup"
                        },
                        elapsed,
                        replacement,
                        elapsed <= 250_000_000
                    );
                    first = true;
                }
                if std::ptr::eq(s, w.batches[0].last().unwrap()) {
                    println!(
                        "PIPELINE_CONTROL\t{}\t{}\toverflow-recovery\t{}",
                        window, native, p.last_ns
                    );
                }
            }
            assert_eq!(p.starts, 2, "fixture计划仅两个世代Startup");
        }
    }
}

#[test]
#[ignore = "small independently frozen image/canvas/Startup regression discriminators"]
fn pipeline_regression_discriminators() {
    // 同一管线回归入口必须区分真实factory/Stage绑定丢失，复用既有fixture。
    legacy_native_factory_stage_binding_preserves_pending_pointer_on_real_failure();
    let ws = workloads();
    let w = ws.iter().find(|w| w.name == "bitmap-24-raw-65").unwrap();
    for native in [false, true] {
        let mut p = Pipeline::new(w, native);
        p.batch(w, &w.batches[0], false);
        assert!(p.started);
        p.check_image(w, w.batches[0].last().unwrap());
        for batch in [&w.batches[1], &w.batches[2]] {
            for s in batch {
                let ts = p.step(w, s, false);
                p.consume(w, ts, true);
                p.check_image(w, s);
            }
        }
        assert_eq!(p.starts, 1);
    }
}

#[test]
#[ignore = "external independent stock4096-batch per-step steady reference"]
fn pipeline_steady_pointer_reference_gate() {
    let (ws, warm) = measurement_workloads();
    for w in ws.iter().filter(|w| warm.contains_key(&w.name)) {
        for native in [false, true] {
            let mut p = Pipeline::new(w, native);
            p.batch(w, &w.batches[0], true);
            for k in 0..64 {
                p.batch(w, &w.batches[1 + k % 2], false);
            }
            for k in 0..256 {
                for (index, s) in w.batches[1 + k % 2].iter().enumerate() {
                    let ts = p.step(w, s, true);
                    p.consume(w, ts, true);
                    check_warm_step(&p, &warm[&w.name][k % 2][index]);
                }
            }
            assert_eq!(p.starts, 1);
        }
    }
    println!("STEADY_GATE three stateful workloads x C/N:64warm+256wholebatch per-step original outputs/RGBA/BGRX+Startup; stock reference proved4096");
}

#[test]
#[ignore = "direct factory-selected Stage C/N against independent per-step steady reference"]
fn pipeline_stage_steady_pointer_reference_gate() {
    let (ws, warm) = measurement_workloads();
    for w in ws.iter().filter(|w| warm.contains_key(&w.name)) {
        for native in [false, true] {
            let mut p = StageLane::new(w, native);
            p.batch(w, &w.batches[0]);
            for k in 0..64 {
                p.batch(w, &w.batches[1 + k % 2]);
            }
            for k in 0..256 {
                for (index, s) in w.batches[1 + k % 2].iter().enumerate() {
                    let result = p.step(w, s);
                    let r = &warm[&w.name][k % 2][index];
                    assert_eq!(format!("{result:?}"), r.expected.outputs);
                    assert!(
                        p.image.data() == r.rgba.as_slice(),
                        "direct Stage complete original steady image"
                    );
                }
            }
        }
    }
    println!("STAGE_STEADY_GATE three stateful workloads x C/N64warm+256; all steps original outputs/fullRGBA");
}
