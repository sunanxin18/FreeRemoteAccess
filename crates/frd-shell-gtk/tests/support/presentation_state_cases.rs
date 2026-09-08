use super::FrameGate;

#[test]
fn bootstrap_and_missing_baseline_never_confirm() {
    let mut gate = FrameGate::default();
    gate.begin(1, false);
    gate.paint(1, true);
    assert!(gate.draw(1, true));
    assert!(!gate.finish(1, true));
}

#[test]
fn exact_clean_frame_is_consumed_once() {
    let mut gate = FrameGate::default();
    gate.begin(2, true);
    gate.paint(2, true);
    assert!(gate.draw(2, true));
    assert!(gate.finish(2, true));
    assert!(!gate.finish(2, true));
}

#[test]
fn wrong_counter_offscreen_empty_duplicate_and_fault_reject() {
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
        assert!(!gate.finish(if scenario == 5 { 5 } else { 4 }, scenario != 6));
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
    assert!(!gate.finish(7, true));
    gate.begin(8, true);
    gate.paint(8, true);
    gate.draw(8, true);
    assert!(gate.finish(8, true));
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
    assert!(!gate.finish(9, true));
}
