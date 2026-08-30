use std::time::{Duration, Instant};

use frd_core::{PixelRect, PixelSize, SessionId};
use frd_frame::{
    EnqueuedSurfaceUpdate, FrameCompleteness, FrameTransaction, FrameTransactionCompiler,
    FrameTransactionError, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate,
};

macro_rules! assert_error {
    ($result:expr, $expected:expr) => {
        assert_eq!(($result).expect_err("输入必须被拒绝"), $expected)
    };
}

fn session() -> SessionId {
    SessionId::allocate()
}

fn size() -> PixelSize {
    PixelSize::new(4, 3).expect("测试表面尺寸有效")
}

fn enqueued(enqueued_at: Instant, update: SurfaceUpdate) -> EnqueuedSurfaceUpdate {
    EnqueuedSurfaceUpdate {
        enqueued_at,
        update,
    }
}

fn reset(session_id: SessionId, generation: u64) -> SurfaceUpdate {
    SurfaceUpdate::Reset {
        session_id,
        generation,
        size: size(),
        format: PixelFormat::Bgra8UnormSrgb,
    }
}

fn one_pixel_patch(fill: u8) -> PixelPatch {
    PixelPatch {
        rect: PixelRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        stride_bytes: 4,
        pixels: PixelBuffer::new(vec![fill; 4]),
    }
}

fn damage(
    session_id: SessionId,
    generation: u64,
    revision: u64,
    patches: Vec<PixelPatch>,
) -> SurfaceUpdate {
    SurfaceUpdate::Damage {
        session_id,
        generation,
        revision,
        patches,
    }
}

fn boundary(
    session_id: SessionId,
    generation: u64,
    revision: u64,
    completeness: FrameCompleteness,
) -> SurfaceUpdate {
    SurfaceUpdate::FrameBoundary {
        session_id,
        generation,
        revision,
        completeness,
    }
}

fn start(compiler: &mut FrameTransactionCompiler, session_id: SessionId, at: Instant) {
    let transactions = compiler
        .compile([
            enqueued(at, reset(session_id, 1)),
            enqueued(
                at + Duration::from_millis(1),
                damage(session_id, 1, 1, vec![one_pixel_patch(0x10)]),
            ),
            enqueued(
                at + Duration::from_millis(2),
                boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
            ),
        ])
        .expect("完整基线应建立编译器");
    assert_eq!(transactions.len(), 1);
}

#[test]
fn reset_and_damage_without_full_boundary_emit_nothing() {
    let session_id = session();
    let at = Instant::now();
    let mut compiler = FrameTransactionCompiler::new(session_id);

    assert!(compiler
        .compile([enqueued(at, reset(session_id, 1))])
        .expect("Reset 本身有效")
        .is_empty());
    assert!(compiler.has_buffered_input());
    assert_eq!(compiler.buffered_source_update_count(), 1);

    assert!(compiler
        .compile([enqueued(
            at + Duration::from_millis(1),
            damage(session_id, 1, 1, vec![one_pixel_patch(0x11)]),
        )])
        .expect("首个损伤有效")
        .is_empty());
    assert!(compiler.has_buffered_input());
    assert_eq!(compiler.buffered_source_update_count(), 2);
}

#[test]
fn matching_full_baseline_emits_one_atomic_startup_without_copying_patches() {
    let session_id = session();
    let at = Instant::now();
    let mut compiler = FrameTransactionCompiler::new(session_id);
    let first = PixelBuffer::new(vec![0x21; 4]);
    let first_pointer = first.as_bytes().as_ptr();
    let second = PixelBuffer::new(vec![0x32; 4]);
    let second_pointer = second.as_bytes().as_ptr();
    let patches = vec![
        PixelPatch {
            rect: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            stride_bytes: 4,
            pixels: first,
        },
        PixelPatch {
            rect: PixelRect {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
            },
            stride_bytes: 4,
            pixels: second,
        },
    ];

    assert!(compiler
        .compile([
            enqueued(at, reset(session_id, 1)),
            enqueued(
                at + Duration::from_millis(1),
                damage(session_id, 1, 1, patches),
            ),
        ])
        .expect("首帧输入有效")
        .is_empty());
    let transactions = compiler
        .compile([enqueued(
            at + Duration::from_millis(2),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
        )])
        .expect("完整边界应关闭首帧");

    assert_eq!(transactions.len(), 1);
    let FrameTransaction::Startup {
        reset, revision, ..
    } = &transactions[0]
    else {
        panic!("必须产生 Startup 事务");
    };
    assert_eq!(reset.session_id, session_id);
    assert_eq!(revision.completeness, FrameCompleteness::FullBaseline);
    assert_eq!(revision.patches.len(), 2);
    assert_eq!(
        revision.patches[0].pixels.as_bytes().as_ptr(),
        first_pointer
    );
    assert_eq!(
        revision.patches[1].pixels.as_bytes().as_ptr(),
        second_pointer
    );
    assert_eq!(revision.patches[0].pixels.as_bytes(), &[0x21; 4]);
    assert_eq!(revision.patches[1].pixels.as_bytes(), &[0x32; 4]);
    assert_eq!(transactions[0].source_update_count(), 3);
}

#[test]
fn incremental_first_startup_boundary_is_fatal() {
    let session_id = session();
    let at = Instant::now();
    let mut compiler = FrameTransactionCompiler::new(session_id);

    let error = compiler
        .compile([
            enqueued(at, reset(session_id, 1)),
            enqueued(
                at + Duration::from_millis(1),
                damage(session_id, 1, 1, vec![one_pixel_patch(0x33)]),
            ),
            enqueued(
                at + Duration::from_millis(2),
                boundary(session_id, 1, 1, FrameCompleteness::Incremental),
            ),
        ])
        .expect_err("首帧的增量边界必须失败");

    assert_eq!(error, FrameTransactionError::StartupBoundaryNotFullBaseline);
}

#[test]
fn compiler_rejects_foreign_session_and_session_switch_uses_new_compiler() {
    let local_session = session();
    let other_session = session();
    let at = Instant::now();
    let mut compiler = FrameTransactionCompiler::new(local_session);

    for update in [
        reset(other_session, 1),
        damage(other_session, 1, 1, vec![one_pixel_patch(0x44)]),
        boundary(other_session, 1, 1, FrameCompleteness::FullBaseline),
    ] {
        assert_error!(
            compiler.compile([enqueued(at, update)]),
            FrameTransactionError::ForeignSession
        );
    }

    let mut switched = FrameTransactionCompiler::new(other_session);
    start(&mut switched, other_session, at + Duration::from_millis(10));
}

#[test]
fn newer_generation_discards_pending_startup_or_revision_but_stale_reset_fails() {
    let session_id = session();
    let foreign_session = session();
    let at = Instant::now();
    let mut compiler = FrameTransactionCompiler::new(session_id);

    compiler
        .compile([
            enqueued(at, reset(session_id, 1)),
            enqueued(
                at + Duration::from_millis(1),
                damage(session_id, 1, 1, vec![one_pixel_patch(0x51)]),
            ),
        ])
        .expect("首帧可等待完整边界");
    assert!(compiler.has_buffered_input());
    assert!(compiler
        .compile([enqueued(
            at + Duration::from_millis(2),
            reset(session_id, 2),
        )])
        .expect("更高世代可取代待启动输入")
        .is_empty());
    assert_eq!(compiler.buffered_source_update_count(), 1);
    assert_error!(
        compiler.compile([enqueued(
            at + Duration::from_millis(3),
            reset(session_id, 1),
        )]),
        FrameTransactionError::StaleReset
    );
    assert_error!(
        compiler.compile([enqueued(
            at + Duration::from_millis(4),
            reset(foreign_session, 99),
        )]),
        FrameTransactionError::ForeignSession
    );

    compiler
        .compile([
            enqueued(
                at + Duration::from_millis(5),
                damage(session_id, 2, 1, vec![one_pixel_patch(0x52)]),
            ),
            enqueued(
                at + Duration::from_millis(6),
                boundary(session_id, 2, 1, FrameCompleteness::FullBaseline),
            ),
        ])
        .expect("第二世代完整基线有效");
    compiler
        .compile([enqueued(
            at + Duration::from_millis(7),
            damage(session_id, 2, 2, vec![one_pixel_patch(0x53)]),
        )])
        .expect("稳态修订可等待边界");
    assert!(compiler
        .compile([enqueued(
            at + Duration::from_millis(8),
            reset(session_id, 3),
        )])
        .expect("更高世代可取代待修订输入")
        .is_empty());
    assert_eq!(compiler.buffered_source_update_count(), 1);
}

#[test]
fn steady_revision_errors_are_exact_and_never_emit() {
    let session_id = session();
    let at = Instant::now();
    let mut compiler = FrameTransactionCompiler::new(session_id);

    assert_error!(
        compiler.compile([enqueued(
            at,
            damage(session_id, 1, 1, vec![one_pixel_patch(0x61)]),
        )]),
        FrameTransactionError::UpdateBeforeReset
    );
    assert!(compiler
        .compile([enqueued(at, reset(session_id, 1))])
        .expect("Reset 有效")
        .is_empty());
    assert_error!(
        compiler.compile([enqueued(
            at + Duration::from_millis(1),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
        )]),
        FrameTransactionError::BoundaryWithoutDamage
    );
    assert!(compiler
        .compile([enqueued(
            at + Duration::from_millis(2),
            damage(session_id, 1, 1, vec![one_pixel_patch(0x62)]),
        )])
        .expect("首个损伤有效")
        .is_empty());
    assert_error!(
        compiler.compile([enqueued(
            at + Duration::from_millis(3),
            damage(session_id, 1, 1, vec![one_pixel_patch(0x63)]),
        )]),
        FrameTransactionError::DuplicateDamage
    );
    assert_error!(
        compiler.compile([enqueued(
            at + Duration::from_millis(4),
            damage(session_id, 1, 2, vec![one_pixel_patch(0x64)]),
        )]),
        FrameTransactionError::RevisionWhilePending
    );
    assert_error!(
        compiler.compile([enqueued(
            at + Duration::from_millis(5),
            boundary(session_id, 1, 2, FrameCompleteness::FullBaseline),
        )]),
        FrameTransactionError::BoundaryMismatch
    );
    assert!(matches!(
        compiler.compile([enqueued(
            at + Duration::from_millis(6),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
        )]),
        Ok(transactions) if transactions.is_empty() == false
    ));
    assert_error!(
        compiler.compile([enqueued(
            at + Duration::from_millis(7),
            damage(session_id, 1, 1, vec![one_pixel_patch(0x65)]),
        )]),
        FrameTransactionError::StaleUpdate
    );
}

#[test]
fn compiler_carries_earliest_constituent_enqueue_across_drains() {
    let session_id = session();
    let at = Instant::now();
    let mut compiler = FrameTransactionCompiler::new(session_id);
    let startup = compiler
        .compile([enqueued(at, reset(session_id, 1))])
        .expect("Reset 有效");
    assert!(startup.is_empty());
    assert_eq!(compiler.earliest_buffered_enqueue_at(), Some(at));
    assert!(compiler
        .compile([enqueued(
            at + Duration::from_millis(10),
            damage(session_id, 1, 1, vec![one_pixel_patch(0x71)]),
        )])
        .expect("损伤有效")
        .is_empty());
    let startup = compiler
        .compile([enqueued(
            at + Duration::from_millis(20),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
        )])
        .expect("完整边界有效");
    assert_eq!(startup[0].earliest_constituent_enqueue_at(), at);

    let first_revision = compiler
        .compile([
            enqueued(
                at + Duration::from_millis(30),
                damage(session_id, 1, 2, vec![one_pixel_patch(0x72)]),
            ),
            enqueued(
                at + Duration::from_millis(40),
                boundary(session_id, 1, 2, FrameCompleteness::Incremental),
            ),
        ])
        .expect("完整稳态修订有效");
    let second_prefix = compiler
        .compile([enqueued(
            at + Duration::from_millis(50),
            damage(session_id, 1, 3, vec![one_pixel_patch(0x73)]),
        )])
        .expect("跨 drain 修订前缀有效");
    assert!(second_prefix.is_empty());
    let second_revision = compiler
        .compile([enqueued(
            at + Duration::from_millis(60),
            boundary(session_id, 1, 3, FrameCompleteness::Incremental),
        )])
        .expect("跨 drain 修订边界有效");

    let all = startup
        .iter()
        .chain(first_revision.iter())
        .chain(second_revision.iter())
        .collect::<Vec<_>>();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].earliest_constituent_enqueue_at(), at);
    assert_eq!(
        all[1].earliest_constituent_enqueue_at(),
        at + Duration::from_millis(30)
    );
    assert_eq!(
        all[2].earliest_constituent_enqueue_at(),
        at + Duration::from_millis(50)
    );
    assert_eq!(
        all.iter()
            .map(|transaction| transaction.earliest_constituent_enqueue_at())
            .min(),
        Some(at)
    );
}
