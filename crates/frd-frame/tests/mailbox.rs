use frd_core::{PixelRect, PixelSize, SessionId};
use frd_frame::{
    FrameCompleteness, FrameMailbox, PixelBuffer, PixelFormat, PixelPatch, PushOutcome,
    SurfaceUpdate,
};

fn session() -> SessionId {
    SessionId::allocate()
}

fn size() -> PixelSize {
    PixelSize::new(4, 3).expect("测试表面尺寸有效")
}

fn reset(session_id: SessionId, generation: u64) -> SurfaceUpdate {
    SurfaceUpdate::Reset {
        session_id,
        generation,
        size: size(),
        format: PixelFormat::Bgra8UnormSrgb,
    }
}

fn patch(rect: PixelRect, stride_bytes: u32, pixel_len: usize) -> PixelPatch {
    PixelPatch {
        rect,
        stride_bytes,
        pixels: PixelBuffer::new(vec![0x5a; pixel_len]),
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

fn one_pixel_patch() -> PixelPatch {
    patch(
        PixelRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        4,
        4,
    )
}

#[test]
fn accepts_patch_with_exact_stride_length_and_surface_bounds() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    assert_eq!(mailbox.push(reset(session_id, 1)), PushOutcome::Queued);

    let valid_patch = patch(
        PixelRect {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        },
        12,
        24,
    );
    assert_eq!(
        mailbox.push(damage(session_id, 1, 1, vec![valid_patch])),
        PushOutcome::Queued
    );
    assert_eq!(mailbox.queued_pixel_bytes(), 24);
}

#[test]
fn rejects_patch_with_short_stride() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    mailbox.push(reset(session_id, 1));

    assert_eq!(
        mailbox.push(damage(
            session_id,
            1,
            1,
            vec![patch(
                PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                7,
                7,
            )],
        )),
        PushOutcome::Rejected
    );
}

#[test]
fn rejects_patch_with_non_exact_payload_length() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    mailbox.push(reset(session_id, 1));

    assert_eq!(
        mailbox.push(damage(
            session_id,
            1,
            1,
            vec![patch(
                PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                8,
                7,
            )],
        )),
        PushOutcome::Rejected
    );
}

#[test]
fn rejects_patch_outside_surface_or_with_overflowing_bounds() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    mailbox.push(reset(session_id, 1));

    assert_eq!(
        mailbox.push(damage(
            session_id,
            1,
            1,
            vec![patch(
                PixelRect {
                    x: 3,
                    y: 2,
                    width: 2,
                    height: 1,
                },
                8,
                8,
            )],
        )),
        PushOutcome::Rejected
    );
    assert_eq!(
        mailbox.push(damage(
            session_id,
            1,
            2,
            vec![patch(
                PixelRect {
                    x: u32::MAX,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                4,
                4,
            )],
        )),
        PushOutcome::Rejected
    );
}

#[test]
fn rejects_damage_and_boundaries_from_stale_session_or_generation() {
    let session_id = session();
    let stale_session = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    mailbox.push(reset(session_id, 2));

    assert_eq!(
        mailbox.push(damage(
            stale_session,
            2,
            1,
            vec![patch(
                PixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                4,
                4,
            )],
        )),
        PushOutcome::Rejected
    );
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Rejected
    );
}

#[test]
fn rejects_zero_generation_and_revision() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    assert_eq!(mailbox.push(reset(session_id, 0)), PushOutcome::Rejected);
    mailbox.push(reset(session_id, 1));

    assert_eq!(
        mailbox.push(damage(
            session_id,
            1,
            0,
            vec![patch(
                PixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                4,
                4,
            )],
        )),
        PushOutcome::Rejected
    );
}

#[test]
fn entry_budget_requests_snapshot_and_removes_current_damage_and_boundaries() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(3, 64);
    mailbox.push(reset(session_id, 1));
    mailbox.push(damage(
        session_id,
        1,
        1,
        vec![patch(
            PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            4,
            4,
        )],
    ));
    mailbox.push(SurfaceUpdate::FrameBoundary {
        session_id,
        generation: 1,
        revision: 1,
        completeness: FrameCompleteness::Incremental,
    });

    assert_eq!(
        mailbox.push(damage(
            session_id,
            1,
            2,
            vec![patch(
                PixelRect {
                    x: 1,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                4,
                4,
            )],
        )),
        PushOutcome::NeedsFullSnapshot
    );
    assert_eq!(mailbox.len(), 1);
    assert_eq!(mailbox.queued_pixel_bytes(), 0);
    assert!(matches!(mailbox.pop(), Some(SurfaceUpdate::Reset { .. })));
}

#[test]
fn byte_budget_requests_snapshot_without_enqueuing_oversized_damage() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 8);
    mailbox.push(reset(session_id, 1));
    assert_eq!(
        mailbox.push(damage(
            session_id,
            1,
            1,
            vec![patch(
                PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                8,
                16,
            )],
        )),
        PushOutcome::NeedsFullSnapshot
    );
    assert_eq!(mailbox.len(), 1);
    assert_eq!(mailbox.queued_pixel_bytes(), 0);
}

#[test]
fn drain_and_reset_keep_entry_and_byte_accounting_exact() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    mailbox.push(reset(session_id, 1));
    mailbox.push(damage(
        session_id,
        1,
        1,
        vec![patch(
            PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 2,
            },
            4,
            8,
        )],
    ));
    assert_eq!((mailbox.len(), mailbox.queued_pixel_bytes()), (2, 8));
    assert!(mailbox.pop().is_some());
    assert_eq!((mailbox.len(), mailbox.queued_pixel_bytes()), (1, 8));
    assert!(mailbox.pop().is_some());
    assert_eq!((mailbox.len(), mailbox.queued_pixel_bytes()), (0, 0));

    mailbox.push(reset(session_id, 2));
    assert_eq!((mailbox.len(), mailbox.queued_pixel_bytes()), (1, 0));
}

#[test]
fn pixel_buffer_is_moved_into_the_mailbox_without_copying() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    mailbox.push(reset(session_id, 1));
    let pixels = PixelBuffer::new(vec![0x44; 4]);
    let patch = PixelPatch {
        rect: PixelRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        stride_bytes: 4,
        pixels,
    };

    assert_eq!(
        mailbox.push(damage(session_id, 1, 1, vec![patch])),
        PushOutcome::Queued
    );
    assert!(matches!(mailbox.pop(), Some(SurfaceUpdate::Reset { .. })));
    let Some(SurfaceUpdate::Damage { patches, .. }) = mailbox.pop() else {
        panic!("应能按值取出已入队的损伤");
    };
    assert_eq!(patches[0].pixels.as_bytes(), &[0x44; 4]);
}

#[test]
fn rejects_stale_same_session_reset_without_mutating_the_current_mailbox() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    mailbox.push(reset(session_id, 2));
    mailbox.push(damage(session_id, 2, 1, vec![one_pixel_patch()]));

    assert_eq!(mailbox.push(reset(session_id, 2)), PushOutcome::Rejected);
    assert_eq!(mailbox.push(reset(session_id, 1)), PushOutcome::Rejected);
    assert_eq!((mailbox.len(), mailbox.queued_pixel_bytes()), (2, 4));
    assert_eq!(
        mailbox.push(damage(session_id, 2, 2, vec![one_pixel_patch()])),
        PushOutcome::Queued
    );
}

#[test]
fn rejects_older_session_reset_without_changing_current_revision_acceptance() {
    let older_session = session();
    let current_session = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    mailbox.push(reset(current_session, 1));
    mailbox.push(damage(current_session, 1, 1, vec![one_pixel_patch()]));

    assert_eq!(
        mailbox.push(reset(older_session, 99)),
        PushOutcome::Rejected
    );
    assert_eq!((mailbox.len(), mailbox.queued_pixel_bytes()), (2, 4));
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id: current_session,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Queued
    );
}

#[test]
fn accepts_newer_session_reset_and_resets_revision_lifecycle() {
    let older_session = session();
    let newer_session = session();
    let mut mailbox = FrameMailbox::new(4, 64);
    mailbox.push(reset(older_session, 9));
    mailbox.push(damage(older_session, 9, 1, vec![one_pixel_patch()]));
    mailbox.push(SurfaceUpdate::FrameBoundary {
        session_id: older_session,
        generation: 9,
        revision: 1,
        completeness: FrameCompleteness::FullBaseline,
    });

    assert_eq!(mailbox.push(reset(newer_session, 1)), PushOutcome::Queued);
    assert_eq!(mailbox.len(), 1);
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id: newer_session,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Rejected
    );
    assert_eq!(
        mailbox.push(damage(newer_session, 1, 1, vec![one_pixel_patch()])),
        PushOutcome::Queued
    );
}

#[test]
fn damage_revisions_must_strictly_increase() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(6, 64);
    mailbox.push(reset(session_id, 1));
    assert_eq!(
        mailbox.push(damage(session_id, 1, 2, vec![one_pixel_patch()])),
        PushOutcome::Queued
    );
    assert_eq!(
        mailbox.push(damage(session_id, 1, 2, vec![one_pixel_patch()])),
        PushOutcome::Rejected
    );
    assert_eq!(
        mailbox.push(damage(session_id, 1, 1, vec![one_pixel_patch()])),
        PushOutcome::Rejected
    );
    assert_eq!(
        mailbox.push(damage(session_id, 1, 3, vec![one_pixel_patch()])),
        PushOutcome::Queued
    );
}

#[test]
fn frame_boundaries_require_the_latest_damage_and_strictly_increase() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(8, 64);
    mailbox.push(reset(session_id, 1));
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Rejected
    );
    mailbox.push(damage(session_id, 1, 2, vec![one_pixel_patch()]));
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Rejected
    );
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 3,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Rejected
    );
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 2,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Queued
    );
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 2,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Rejected
    );
}

#[test]
fn overflow_does_not_advance_damage_revision() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(3, 64);
    mailbox.push(reset(session_id, 1));
    mailbox.push(damage(session_id, 1, 1, vec![one_pixel_patch()]));
    mailbox.push(SurfaceUpdate::FrameBoundary {
        session_id,
        generation: 1,
        revision: 1,
        completeness: FrameCompleteness::Incremental,
    });

    assert_eq!(
        mailbox.push(damage(session_id, 1, 2, vec![one_pixel_patch()])),
        PushOutcome::NeedsFullSnapshot
    );
    assert_eq!(
        mailbox.push(damage(session_id, 1, 2, vec![one_pixel_patch()])),
        PushOutcome::Queued
    );
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 2,
            completeness: FrameCompleteness::FullBaseline,
        }),
        PushOutcome::Queued
    );
}

#[test]
#[should_panic(expected = "entry_limit 必须大于零")]
fn mailbox_rejects_zero_entry_capacity() {
    let _ = FrameMailbox::new(0, 0);
}

#[test]
fn entry_overflow_invalidates_the_discarded_damage_boundary_eligibility() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(2, 64);
    mailbox.push(reset(session_id, 1));
    mailbox.push(damage(session_id, 1, 1, vec![one_pixel_patch()]));

    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::NeedsFullSnapshot
    );
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Rejected
    );
    assert_eq!((mailbox.len(), mailbox.queued_pixel_bytes()), (1, 0));
    assert!(matches!(mailbox.pop(), Some(SurfaceUpdate::Reset { .. })));
    assert_eq!(
        mailbox.push(damage(session_id, 1, 2, vec![one_pixel_patch()])),
        PushOutcome::Queued
    );
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 2,
            completeness: FrameCompleteness::FullBaseline,
        }),
        PushOutcome::Queued
    );
}

#[test]
fn byte_overflow_invalidates_the_discarded_damage_boundary_eligibility() {
    let session_id = session();
    let mut mailbox = FrameMailbox::new(4, 8);
    mailbox.push(reset(session_id, 1));
    mailbox.push(damage(session_id, 1, 1, vec![one_pixel_patch()]));

    // FrameBoundary 没有像素负载，不能触发字节预算溢出；此更大 Damage 是对应路径。
    assert_eq!(
        mailbox.push(damage(
            session_id,
            1,
            2,
            vec![patch(
                PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                8,
                8,
            )],
        )),
        PushOutcome::NeedsFullSnapshot
    );
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::Incremental,
        }),
        PushOutcome::Rejected
    );
    assert_eq!((mailbox.len(), mailbox.queued_pixel_bytes()), (1, 0));
    assert_eq!(
        mailbox.push(damage(session_id, 1, 2, vec![one_pixel_patch()])),
        PushOutcome::Queued
    );
    assert_eq!(
        mailbox.push(SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 2,
            completeness: FrameCompleteness::FullBaseline,
        }),
        PushOutcome::Queued
    );
}
