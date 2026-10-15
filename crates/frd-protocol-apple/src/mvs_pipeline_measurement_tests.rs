// 有限真实 wire→CPU surface→协议邮箱测量；仅显式 ignored 测试可执行。
// 默认编译仅为测试 oracle，不是受支持平台的生产标量解码许可。
use frd_frame::{FrameCompleteness, FrameMailbox, SurfaceUpdate};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

const PIXEL_BUDGET: usize = 16 * 1024 * 1024;
const SAMPLE_CYCLES: usize = 3;
const RECORDS: usize = 4;

struct Workload {
    name: &'static str,
    width: u16,
    height: u16,
    tables: [u8; 129],
    setup: Vec<MvsRecord>,
    records: [MvsRecord; RECORDS],
}

fn rect(x: u16, y: u16, width: u16, height: u16) -> MvsRect {
    MvsRect {
        x,
        y,
        width,
        height,
    }
}

fn tile_count(r: MvsRect) -> usize {
    usize::from(r.width).div_ceil(8) * usize::from(r.height).div_ceil(8)
}

fn bits(writer: &mut TestBitWriter, literal: &str) {
    for bit in literal.bytes().filter(|b| *b != b'_') {
        assert!(matches!(bit, b'0' | b'1'));
        writer.write_bits(u32::from(bit - b'0'), 1);
    }
}

fn full_record(r: MvsRect, modes: &[u8], data: TestBitWriter, threshold: u8) -> MvsRecord {
    assert_eq!(modes.len(), tile_count(r));
    let mut writer = TestBitWriter::new();
    writer.write_bits(1, 1);
    let mut start = 0;
    while start < modes.len() {
        let mode = modes[start];
        assert!(mode < 8);
        let count = modes[start..]
            .iter()
            .take(16)
            .take_while(|m| **m == mode)
            .count();
        writer.write_bits(u32::from(mode), 3);
        if count == 1 {
            writer.write_bits(0, 1);
        } else {
            writer.write_bits(1, 1);
            writer.write_bits((count - 2) as u32, 4);
        }
        start += count;
    }
    writer.write_bits(0x6d, 8);
    let mode = writer.finish();
    let mut data = data;
    data.write_bits(0x6d, 8);
    let offset = 6 + mode.len();
    assert!(offset < 1 << 24);
    let mut payload = vec![
        0,
        threshold,
        31,
        (offset >> 16) as u8,
        (offset >> 8) as u8,
        offset as u8,
    ];
    payload.extend(mode);
    payload.extend(data.finish());
    MvsRecord { rect: r, payload }
}

fn table(luma: u8, dense: bool) -> [u8; 129] {
    let mut t = [1; 129];
    t[0] = 2;
    for (i, q) in t[1..65].iter_mut().enumerate() {
        *q = if dense { (i % 4 + 1) as u8 } else { luma };
    }
    t
}

// 与既有 mode3 对角线 fixture 相同的独立图案：每块方向随块坐标变化。
fn bitmap(r: MvsRect, phase: usize) -> MvsRecord {
    let mut data = TestBitWriter::new();
    for index in 0..tile_count(r) {
        data.write_bits(0, 8);
        for row in 0..8 {
            data.write_bits(0x80 >> ((row + index + phase) % 8), 8);
        }
    }
    full_record(r, &vec![3; tile_count(r)], data, 15)
}

fn copy_record(r: MvsRect, vertical: bool, clipped_row_start: bool) -> MvsRecord {
    let columns = usize::from(r.width).div_ceil(8);
    let modes = (0..tile_count(r))
        .map(|i| {
            if vertical || (clipped_row_start && i != 0 && i % columns == 0) {
                2
            } else {
                1
            }
        })
        .collect::<Vec<_>>();
    full_record(r, &modes, TestBitWriter::new(), 15)
}

fn clipped_overlay(r: MvsRect) -> MvsRecord {
    let mut data = TestBitWriter::new();
    data.write_bits(0, 8);
    for row in 0..8 {
        data.write_bits(0x80 >> row, 8);
    }
    full_record(r, &[3, 1, 1, 1], data, 15)
}

fn misses(r: MvsRect, explicit: bool) -> MvsRecord {
    let mut data = TestBitWriter::new();
    if explicit {
        for _ in 0..tile_count(r) {
            data.write_bits(60_000, 16);
        }
    }
    full_record(
        r,
        &vec![if explicit { 6 } else { 7 }; tile_count(r)],
        data,
        15,
    )
}

fn ac_seed(r: MvsRect, dense: bool, phase: usize) -> MvsRecord {
    let mut data = TestBitWriter::new();
    for tile in 0..tile_count(r) {
        bits(&mut data, "0_1_0_00");
        if dense {
            // scan1..63：全部非零Y AC；Cb/Cr为零，不称三通道dense。
            for scan in 1..64 {
                bits(
                    &mut data,
                    if (scan + tile + phase) % 2 == 0 {
                        "010"
                    } else {
                        "011"
                    },
                );
            }
        } else {
            bits(&mut data, "010_0010");
        }
    }
    full_record(r, &vec![5; tile_count(r)], data, 15)
}

fn partial(r: MvsRect, cr_extent: u8, cb_extent: u8, fields: &[&str]) -> MvsRecord {
    assert_eq!(fields.len(), tile_count(r));
    let mut data = TestBitWriter::new();
    for literal in fields {
        bits(&mut data, literal);
    }
    for terminal in [0x6d, 0x76, 0x73] {
        data.write_bits(terminal, 8);
    }
    let mut payload = vec![1, cr_extent, cb_extent];
    payload.extend(data.finish());
    MvsRecord { rect: r, payload }
}

fn workload(index: usize) -> Workload {
    let (width, height) = match index {
        0 | 6 => (1280, 720),
        1 | 2 => (1366, 769),
        _ => (1920, 1080),
    };
    let full = rect(0, 0, width, height);
    let local = rect(320, 240, 64, 64);
    let other = rect(1280, 640, 64, 64);
    let mut w = Workload {
        name: [
            "full-copy-720",
            "full-copy-tail",
            "clipped-overlay",
            "local-copy",
            "local-miss",
            "local-type1-copy",
            "full-dense-y",
            "local-type1-refine",
        ][index],
        width,
        height,
        tables: table(64, index == 6),
        setup: vec![bitmap(full, 0)],
        records: std::array::from_fn(|_| copy_record(full, false, index == 1)),
    };
    w.records = match index {
        0 | 1 => std::array::from_fn(|i| copy_record(full, i % 2 != 0, index == 1)),
        2 => {
            let r = rect(16, 16, 10, 16);
            [
                clipped_overlay(r),
                bitmap(r, 1),
                clipped_overlay(r),
                bitmap(r, 2),
            ]
        }
        3 => [
            copy_record(local, false, false),
            copy_record(other, true, false),
            copy_record(local, true, false),
            copy_record(other, false, false),
        ],
        4 => std::array::from_fn(|i| misses(local, i % 2 == 0)),
        5 => {
            let fields = (0..64)
                .map(|i| if i == 0 { "00" } else { "10" })
                .collect::<Vec<_>>();
            [
                copy_record(local, false, false),
                partial(local, 0, 0, &fields),
                copy_record(local, true, false),
                partial(local, 0, 0, &fields),
            ]
        }
        6 => std::array::from_fn(|i| ac_seed(full, true, i)),
        7 => {
            w.setup.push(ac_seed(local, false, 0));
            let populate = "01_000000_0_1100_0_1010_0_1010";
            [
                partial(local, 19, 14, &[populate; 64]),
                partial(local, 0, 0, &["11_0_00000000_00000001"; 64]),
                partial(local, 19, 14, &[populate; 64]),
                partial(local, 0, 0, &["11_1"; 64]),
            ]
        }
        _ => unreachable!(),
    };
    w
}

struct Context {
    receiver: MvsReceiveState,
    surface: Arc<Mutex<DisplaySurface>>,
    dynamic: Arc<Mutex<DynamicResolutionRuntime>>,
    runtime: ProtocolRuntime,
    publisher: AppleSurfacePublisher,
    mailbox: Arc<Mutex<FrameMailbox>>,
    session: frd_core::SessionId,
    framebuffer_pointer: usize,
    framebuffer_len: usize,
    surface_allocations: usize,
}

impl Context {
    fn new(w: &Workload) -> Self {
        reset_display_surface_allocation_count();
        let surface = native_surface(w.width as usize, w.height as usize);
        let dynamic = native_runtime(w.width, w.height);
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&w.tables).unwrap();
        let session = frd_core::SessionId::allocate();
        let (mut runtime, mailbox, _) = mailbox_protocol_runtime(session, PIXEL_BUDGET);
        let publisher = AppleSurfacePublisher::begin(
            &mut runtime,
            session,
            PixelSize::new(w.width as u32, w.height as u32).unwrap(),
        )
        .unwrap();
        let reset = mailbox.lock().unwrap().drain_committed_updates();
        assert_eq!(reset.len(), 1);
        assert!(matches!(reset[0].update, SurfaceUpdate::Reset { .. }));
        let locked = surface.lock().unwrap();
        let mut context = Self {
            framebuffer_pointer: locked.framebuffer.pixels().as_ptr() as usize,
            framebuffer_len: locked.framebuffer.pixels().len(),
            surface_allocations: display_surface_allocation_count(),
            receiver,
            surface: surface.clone(),
            dynamic,
            runtime,
            publisher,
            mailbox,
            session,
        };
        drop(locked);
        assert_eq!(context.surface_allocations, 1);
        for (index, record) in w.setup.iter().enumerate() {
            let outcome = context.apply(record);
            context.publish(outcome);
            context.drain();
            if index == 0 {
                // 独立literal：第一块是左上到右下的白色对角线。
                let s = context.surface.lock().unwrap();
                for y in 0..8 {
                    for x in 0..8 {
                        assert_eq!(
                            s.framebuffer.pixels()[y * w.width as usize + x],
                            if x == y { 0x00ff_ffff } else { 0 }
                        );
                    }
                }
            } else {
                // 已冻结单AC literal，不以default/native同错作为正确性依据。
                let expected = [150u32, 147, 141, 132, 124, 115, 109, 106];
                let s = context.surface.lock().unwrap();
                for y in 0..8 {
                    for (x, v) in expected.iter().enumerate() {
                        let offset = (record.rect.y as usize + y) * w.width as usize
                            + record.rect.x as usize
                            + x;
                        assert_eq!(s.framebuffer.pixels()[offset], v * 0x0001_0101);
                    }
                }
            }
        }
        context
    }

    fn apply(&mut self, record: &MvsRecord) -> MvsRecordOutcome {
        apply_native_mvs_frame(&mut self.receiver, record, &self.surface, &self.dynamic).unwrap()
    }

    fn publish(&mut self, outcome: MvsRecordOutcome) {
        let surface = self.surface.lock().unwrap();
        let result = match outcome {
            MvsRecordOutcome::FullApplied {
                complete_surface,
                publication,
            } => self.publisher.publish_committed_patch(
                &mut self.runtime,
                &surface,
                1,
                publication.patch,
                MvsFrameKind::TypeZero {
                    complete_surface,
                    initial_nonblack: publication.contains_nonblack,
                },
            ),
            MvsRecordOutcome::PartialApplied { patches } if !patches.is_empty() => self
                .publisher
                .publish_committed_type_one_patches(&mut self.runtime, &surface, 1, patches),
            _ => panic!("资格/测量记录必须实际产生像素并发布"),
        }
        .unwrap();
        assert_eq!(result, PublicationOutcome::Published);
    }

    fn drain(&self) -> (Vec<frd_frame::EnqueuedSurfaceUpdate>, usize) {
        let surface = self.surface.lock().unwrap();
        assert_eq!(
            surface.framebuffer.pixels().as_ptr() as usize,
            self.framebuffer_pointer
        );
        assert_eq!(surface.framebuffer.pixels().len(), self.framebuffer_len);
        assert_eq!(display_surface_allocation_count(), self.surface_allocations);
        drop(surface);
        let mut mailbox = self.mailbox.lock().unwrap();
        let queued = mailbox.queued_pixel_bytes();
        assert!(queued > 0 && queued <= PIXEL_BUDGET);
        let updates = mailbox.drain_committed_updates();
        assert_eq!(updates.len(), 2);
        assert!(matches!(updates[0].update, SurfaceUpdate::Damage { .. }));
        assert!(matches!(
            updates[1].update,
            SurfaceUpdate::FrameBoundary { .. }
        ));
        assert!(mailbox.is_empty());
        (updates, queued)
    }
}

fn number(out: &mut impl Write, value: u64) -> io::Result<()> {
    out.write_all(&value.to_le_bytes())
}
fn blob(out: &mut impl Write, value: &[u8]) -> io::Result<()> {
    number(out, value.len() as u64)?;
    out.write_all(value)
}
fn write_rect(out: &mut impl Write, r: frd_core::PixelRect) -> io::Result<()> {
    for v in [r.x, r.y, r.width, r.height] {
        number(out, u64::from(v))?;
    }
    Ok(())
}

// 完整二进制输出；session ID只校验归属，不把进程内分配号当像素差异。
fn write_record(
    out: &mut impl Write,
    w: &Workload,
    index: usize,
    c: &Context,
    updates: &[frd_frame::EnqueuedSurfaceUpdate],
) -> io::Result<()> {
    out.write_all(b"FRDMVP01")?;
    blob(out, w.name.as_bytes())?;
    for value in [index as u64, w.width as u64, w.height as u64] {
        number(out, value)?;
    }
    let r = w.records[index].rect;
    for value in [r.x, r.y, r.width, r.height] {
        number(out, value as u64)?;
    }
    let surface = c.surface.lock().unwrap();
    number(out, surface.framebuffer.pixels().len() as u64)?;
    let mut bytes = [0; 4096];
    for words in surface.framebuffer.pixels().chunks(1024) {
        for (word, target) in words.iter().zip(bytes.chunks_exact_mut(4)) {
            target.copy_from_slice(&word.to_le_bytes());
        }
        out.write_all(&bytes[..words.len() * 4])?;
    }
    drop(surface);
    number(out, updates.len() as u64)?;
    for entry in updates {
        match &entry.update {
            SurfaceUpdate::Damage {
                session_id,
                generation,
                revision,
                patches,
            } => {
                assert_eq!(*session_id, c.session);
                out.write_all(&[1])?;
                for value in [*generation, *revision, patches.len() as u64] {
                    number(out, value)?;
                }
                for patch in patches {
                    write_rect(out, patch.rect)?;
                    number(out, patch.stride_bytes as u64)?;
                    blob(out, patch.pixels.as_bytes())?;
                }
            }
            SurfaceUpdate::FrameBoundary {
                session_id,
                generation,
                revision,
                completeness,
            } => {
                assert_eq!(*session_id, c.session);
                out.write_all(&[2])?;
                number(out, *generation)?;
                number(out, *revision)?;
                out.write_all(&[match completeness {
                    FrameCompleteness::Incremental => 0,
                    FrameCompleteness::FullBaseline => 1,
                }])?;
            }
            SurfaceUpdate::Reset { .. } => panic!("记录内不允许重新分配generation"),
        }
    }
    Ok(())
}

fn directory(variable: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(variable).unwrap_or_else(|| panic!("需要显式设置{variable}")),
    );
    assert!(path.is_absolute(), "证据目录必须是绝对路径");
    path
}
fn record_path(root: &Path, w: &Workload, record: usize) -> PathBuf {
    root.join(format!("{}-{record}.bin", w.name))
}

// 流式逐字节比较，至多4096字节临时缓冲；不在计时内写磁盘或比较。
struct ExactReference {
    reader: BufReader<File>,
    offset: usize,
}
impl ExactReference {
    fn new(path: &Path) -> Self {
        Self {
            reader: BufReader::new(File::open(path).unwrap()),
            offset: 0,
        }
    }
    fn finish(mut self) {
        let mut tail = [0];
        assert_eq!(self.reader.read(&mut tail).unwrap(), 0, "参考含额外尾部");
    }
}
impl Write for ExactReference {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut expected = [0; 4096];
        for chunk in bytes.chunks(expected.len()) {
            self.reader.read_exact(&mut expected[..chunk.len()])?;
            if expected[..chunk.len()] != *chunk {
                return Err(io::Error::other(format!(
                    "完整输出在byte {}不一致",
                    self.offset
                )));
            }
            self.offset += chunk.len();
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn compare_record(
    reference: &Path,
    w: &Workload,
    index: usize,
    c: &Context,
    updates: &[frd_frame::EnqueuedSurfaceUpdate],
) {
    let mut comparison = ExactReference::new(&record_path(reference, w, index));
    write_record(&mut comparison, w, index, c, updates).unwrap();
    comparison.finish();
}

#[test]
#[ignore = "显式有限32记录wire资格；需要FRD_MVS_QUALIFICATION_DIR，不执行性能测量"]
fn mvs_pipeline_wire_qualification() {
    assert!(!cfg!(debug_assertions), "资格使用Release构建");
    assert!(!cfg!(feature = "mvs-profile"));
    let root = directory("FRD_MVS_QUALIFICATION_DIR");
    // 不覆盖旧输出，失败工件和部分输出必须保留。
    fs::create_dir(&root).unwrap();
    for index in 0..8 {
        let w = workload(index);
        let mut context = Context::new(&w);
        for (record_index, record) in w.records.iter().enumerate() {
            let outcome = context.apply(record);
            context.publish(outcome);
            let (updates, queued) = context.drain();
            let mut out =
                BufWriter::new(File::create(record_path(&root, &w, record_index)).unwrap());
            write_record(&mut out, &w, record_index, &context, &updates).unwrap();
            out.flush().unwrap();
            println!("MVS_QUALIFY_V1|{}|record={record_index}|queued_bytes={queued}|surface_allocations=1|framebuffer_stable=true", w.name);
        }
        let state = context.receiver.decoder.pipeline_snapshot();
        fs::write(root.join(format!("{}.state", w.name)), state).unwrap();
    }
    fs::write(
        root.join("DONE"),
        b"cases=8;records=32;full_values=true;timing=false\n",
    )
    .unwrap();
}

#[test]
#[ignore = "正式有限性能执行需root另行释放；参考和身份环境变量必须显式提供"]
fn mvs_pipeline_finite_measurement() {
    assert!(!cfg!(debug_assertions));
    assert!(!cfg!(feature = "mvs-profile"));
    let reference = directory("FRD_MVS_REFERENCE_DIR");
    let output = directory("FRD_MVS_MEASUREMENT_DIR");
    let identity = std::env::var("FRD_MVS_RUN_IDENTITY").expect("需显式设置工件及目标身份");
    assert!(!identity.is_empty() && !identity.contains(['\n', '\r', '|']));
    let reverse = match std::env::var("FRD_MVS_CASE_ORDER").as_deref() {
        Ok("forward") => false,
        Ok("reverse") => true,
        _ => panic!("FRD_MVS_CASE_ORDER必须forward或reverse"),
    };
    assert_eq!(
        fs::read(reference.join("DONE")).unwrap(),
        b"cases=8;records=32;full_values=true;timing=false\n"
    );
    fs::create_dir(&output).unwrap();
    let mut log = BufWriter::new(File::create(output.join("records.txt")).unwrap());
    writeln!(log, "MVS_MEASURE_V1|identity={identity}|native={}|samples={SAMPLE_CYCLES}|records={RECORDS}|warmup_cycles=1|order={}|allocation_scope=DisplaySurface-only|private_capacity=unmeasured|rss=unmeasured|libtest_type1_trace=included", cfg!(feature="mvs-native-arm64"), if reverse {"reverse"} else {"forward"}).unwrap();
    for index in (0..8).map(|i| if reverse { 7 - i } else { i }) {
        let w = workload(index);
        // 每cycle新建相同context，warmup不会改变sample的cache索引。
        for cycle in 0..=SAMPLE_CYCLES {
            let mut context = Context::new(&w);
            for (record_index, record) in w.records.iter().enumerate() {
                let started = Instant::now();
                let outcome = context.apply(record);
                let applied = Instant::now();
                context.publish(outcome);
                let published = Instant::now();
                let (updates, queued) = context.drain();
                compare_record(&reference, &w, record_index, &context, &updates);
                if cycle != 0 {
                    let patches = match &updates[0].update {
                        SurfaceUpdate::Damage { patches, .. } => patches,
                        _ => unreachable!(),
                    };
                    writeln!(log, "MVS_RECORD_V1|case={}|sample={}|record={record_index}|surface={}x{}|rect={},{},{},{}|wire_type={}|apply_ns={}|publish_ns={}|total_ns={}|queued_bytes={queued}|patches={}|patch_bytes={}|surface_allocations=1|framebuffer_stable=true", w.name, cycle-1, w.width,w.height,record.rect.x,record.rect.y,record.rect.width,record.rect.height,record.payload[0],applied.duration_since(started).as_nanos(),published.duration_since(applied).as_nanos(),published.duration_since(started).as_nanos(),patches.len(),patches.iter().map(|p|p.pixels.len()).sum::<usize>()).unwrap();
                    log.flush().unwrap();
                }
            }
            let state = context.receiver.decoder.pipeline_snapshot();
            assert_eq!(
                state.as_bytes(),
                fs::read(reference.join(format!("{}.state", w.name))).unwrap()
            );
        }
    }
    log.flush().unwrap();
    fs::write(
        output.join("DONE"),
        b"cases=8;samples=3;measured_records=96;warmup_records=32\n",
    )
    .unwrap();
}
