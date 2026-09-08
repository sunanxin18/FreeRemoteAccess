use super::FrameGate;

#[test]
fn bootstrap_and_missing_baseline_never_confirm() {
    let mut gate = FrameGate::default();
    gate.begin(1, false);
    gate.paint(1, true);
    assert!(gate.draw(1, true));
    assert!(!gate.finish(1, true, true));
}

#[test]
fn exact_clean_frame_is_consumed_once() {
    let mut gate = FrameGate::default();
    gate.begin(2, true);
    gate.paint(2, true);
    assert!(gate.draw(2, true));
    assert!(gate.finish(2, true, true));
    assert!(!gate.finish(2, true, true));
}

#[test]
fn wrong_counter_offscreen_missing_region_duplicate_and_fault_reject() {
    for scenario in 0..7 {
        let mut gate = FrameGate::default();
        gate.begin(4, true);
        gate.paint(if scenario == 0 { 3 } else { 4 }, scenario != 1);
        gate.draw(4, scenario != 2);
        if scenario == 3 {
            gate.draw(4, true);
        }
        if scenario == 4 {
            gate.paint(4, true);
        }
        assert!(!gate.finish(if scenario == 5 { 5 } else { 4 }, scenario != 6, true));
    }
}

#[test]
fn resize_unrealize_invalidates_inflight_and_issued_epoch() {
    let mut gate = FrameGate::default();
    gate.begin(7, true);
    gate.paint(7, true);
    gate.draw(7, true);
    let token_epoch = gate.epoch.get();
    let retained_epoch = gate.epoch.clone();
    gate.invalidate();
    assert_ne!(retained_epoch.get(), token_epoch);
    assert!(!gate.finish(7, true, true));
    gate.begin(8, true);
    gate.paint(8, true);
    gate.draw(8, true);
    assert!(gate.finish(8, true, true));
}

#[test]
fn exhausted_epoch_permanently_rejects_new_frames() {
    let mut gate = FrameGate::default();
    gate.epoch.set(u64::MAX);
    gate.invalidate();
    assert!(gate.exhausted.get());
    gate.begin(9, true);
    gate.paint(9, true);
    gate.draw(9, true);
    assert!(!gate.finish(9, true, true));
}

#[test]
fn clean_after_paint_without_window_context_transition_cannot_confirm() {
    let mut gate = FrameGate::default();
    gate.begin(10, true);
    // Surface::render的region存在即可关联，是否为空不是最终GSK damage。
    gate.paint(10, true);
    gate.draw(10, true);
    assert!(!gate.finish(10, true, false));
    gate.begin(11, true);
    gate.paint(11, true);
    gate.draw(11, true);
    assert!(gate.finish(11, true, true));
}

#[test]
fn egl_error_is_captured_before_successful_identity_queries_overwrite_it() {
    let error = std::cell::Cell::new(0x300d_u32); // EGL_BAD_SURFACE
    let (observed, identity) = super::capture_egl_before_identity(
        || error.replace(0x3000),
        || {
            error.set(0x3000);
            true
        },
    );
    assert!(identity);
    assert_eq!(observed, 0x300d);
    assert_eq!(error.get(), 0x3000);
}

#[test]
fn initial_attach_and_layout_gaps_never_confirm_or_fabricate_baseline() {
    for layout in [false, true] {
        let mut gate = FrameGate::initial_attach(20);
        if layout {
            gate.begin(20, true);
        }
        gate.paint(20, true);
        if layout {
            gate.invalidate_layout(20);
        }
        assert!(gate.draw(20, true));
        assert!(!gate.finish(20, true, true));
        gate.begin(21, true);
        gate.paint(21, true);
        assert!(gate.draw(21, true));
        assert!(gate.finish(21, true, true));
    }
}

#[test]
fn explicit_gaps_still_reject_wrong_signal_counter_and_duplicate_draw() {
    for scenario in 0..4 {
        let mut gate = FrameGate::initial_attach(22);
        gate.paint(22, true);
        if scenario == 3 {
            gate.paint(22, true);
            gate.invalidate_layout(22);
        }
        let accepted = gate.draw(if scenario == 0 { 23 } else { 22 }, scenario != 1);
        if scenario == 2 {
            assert!(accepted);
            assert!(!gate.draw(22, true));
        } else {
            assert!(!accepted);
        }
        assert!(!gate.finish(22, true, true));
    }
    let mut gate = FrameGate::default();
    gate.invalidate();
    gate.paint(22, true);
    assert!(!gate.draw(22, true)); // 任意失效不能冒充显式 gap。
}
