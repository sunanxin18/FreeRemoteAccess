use frd_shell_gtk::input_ownership::{
    FilterResult, KeyDecision, KeyOwner, KeyOwnershipState, KeyPhase, PhysicalPressPermit,
};

const KEY_A: u32 = 38;
const KEY_B: u32 = 56;
const PASS: FilterResult = FilterResult {
    consumed: false,
    has_commit: false,
};
const IM: FilterResult = FilterResult {
    consumed: true,
    has_commit: false,
};
const COMMIT: FilterResult = FilterResult {
    consumed: true,
    has_commit: true,
};

fn key(
    state: &mut KeyOwnershipState,
    code: u32,
    phase: KeyPhase,
    repeat: bool,
    eligible: bool,
    filter: FilterResult,
) -> KeyDecision {
    let event = state
        .begin_key(Some(code), phase, repeat, eligible)
        .unwrap();
    state.finish_key(event, filter)
}

fn permit(decision: KeyDecision, code: u32, expected_repeat: bool) -> PhysicalPressPermit {
    match decision {
        KeyDecision::RemotePress {
            hardware_keycode,
            repeat,
            permit,
        } => {
            assert_eq!(hardware_keycode, code);
            assert_eq!(repeat, expected_repeat);
            permit
        }
        other => panic!("expected physical press, got {other:?}"),
    }
}

fn send_press(state: &mut KeyOwnershipState, code: u32) {
    let p = permit(
        key(state, code, KeyPhase::Press, false, true, PASS),
        code,
        false,
    );
    assert!(state.confirm_physical_press(p, true));
}

#[test]
fn sent_press_release_bypasses_later_im_filter_and_drops_its_commit() {
    let mut state = KeyOwnershipState::new();
    state.bind_im_context();
    send_press(&mut state, KEY_A);
    assert_eq!(state.owner(KEY_A), Some(KeyOwner::RemotePhysical));
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Release, false, true, COMMIT),
        KeyDecision::RemoteRelease {
            hardware_keycode: KEY_A
        }
    ));
    assert_eq!(state.owner(KEY_A), None);
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Release, false, true, PASS),
        KeyDecision::Ignore
    ));
}

#[test]
fn failed_or_discarded_send_never_creates_remote_release() {
    for forwarded_failure in [false, true] {
        let mut state = KeyOwnershipState::new();
        let p = permit(
            key(&mut state, KEY_A, KeyPhase::Press, false, true, PASS),
            KEY_A,
            false,
        );
        if forwarded_failure {
            assert!(state.confirm_physical_press(p, false));
        } else {
            drop(p);
        }
        assert_eq!(state.owner(KEY_A), None);
        assert!(matches!(
            key(&mut state, KEY_A, KeyPhase::Release, false, true, PASS),
            KeyDecision::Ignore
        ));
    }
}

#[test]
fn confirmation_is_bound_to_exact_state_event_and_epoch() {
    let mut first = KeyOwnershipState::new();
    let mut second = KeyOwnershipState::new();
    let foreign = permit(
        key(&mut first, KEY_A, KeyPhase::Press, false, true, PASS),
        KEY_A,
        false,
    );
    let _second_decision = key(&mut second, KEY_A, KeyPhase::Press, false, true, PASS);
    assert!(!second.confirm_physical_press(foreign, true));
    let stale = permit(
        key(&mut first, KEY_A, KeyPhase::Press, false, true, PASS),
        KEY_A,
        false,
    );
    let newer = permit(
        key(&mut first, KEY_B, KeyPhase::Press, false, true, PASS),
        KEY_B,
        false,
    );
    assert!(!first.confirm_physical_press(stale, true));
    assert!(first.confirm_physical_press(newer, true));
    assert_eq!(first.owner(KEY_A), None);
    let stale_epoch = permit(
        key(&mut first, KEY_A, KeyPhase::Press, false, true, PASS),
        KEY_A,
        false,
    );
    first.reset_epoch();
    assert!(!first.confirm_physical_press(stale_epoch, true));
}

#[test]
fn synchronous_commit_and_physical_decision_are_mutually_exclusive() {
    for consumed in [false, true] {
        let mut state = KeyOwnershipState::new();
        let token = state.bind_im_context();
        let decision = key(
            &mut state,
            KEY_A,
            KeyPhase::Press,
            false,
            true,
            FilterResult {
                consumed,
                has_commit: true,
            },
        );
        assert!(matches!(
            decision,
            KeyDecision::InputMethod {
                accept_sync_commit: true
            }
        ));
        assert_eq!(state.owner(KEY_A), Some(KeyOwner::InputMethod));
        assert!(!state.accept_async_commit(&token));
        assert!(matches!(
            key(&mut state, KEY_A, KeyPhase::Release, false, true, PASS),
            KeyDecision::InputMethod {
                accept_sync_commit: false
            }
        ));
    }
}

#[test]
fn asynchronous_commit_survives_im_key_release_but_consumes_authorization_once() {
    let mut state = KeyOwnershipState::new();
    let token = state.bind_im_context();
    assert!(!state.accept_async_commit(&token));
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Press, false, true, IM),
        KeyDecision::InputMethod {
            accept_sync_commit: false
        }
    ));
    let _ = key(&mut state, KEY_A, KeyPhase::Release, false, true, PASS);
    assert!(state.accept_async_commit(&token));
    assert!(!state.accept_async_commit(&token));
}

#[test]
fn old_context_and_focus_epoch_tokens_cannot_acquire_new_authorization() {
    let mut state = KeyOwnershipState::new();
    let old_context = state.bind_im_context();
    let current_context = state.bind_im_context();
    let _ = key(&mut state, KEY_A, KeyPhase::Press, false, true, IM);
    assert!(!state.accept_async_commit(&old_context));
    assert!(state.accept_async_commit(&current_context));
    state.reset_epoch();
    let current_epoch = state.bind_im_context();
    let _ = key(&mut state, KEY_A, KeyPhase::Press, false, true, IM);
    assert!(!state.accept_async_commit(&current_context));
    assert!(state.accept_async_commit(&current_epoch));
}

#[test]
fn filter_ticket_can_cross_callback_without_borrow_but_not_new_event_or_context() {
    let mut state = KeyOwnershipState::new();
    let token = state.bind_im_context();
    let event = state
        .begin_key(Some(KEY_A), KeyPhase::Press, false, true)
        .unwrap();
    // 模拟同步 filter 回调：可再次访问账本，不允许当成异步提交发送。
    assert!(!state.accept_async_commit(&token));
    assert!(matches!(
        state.finish_key(event, COMMIT),
        KeyDecision::InputMethod {
            accept_sync_commit: true
        }
    ));
    let stale = state
        .begin_key(Some(KEY_A), KeyPhase::Press, false, true)
        .unwrap();
    let current = state
        .begin_key(Some(KEY_B), KeyPhase::Press, false, true)
        .unwrap();
    assert!(matches!(
        state.finish_key(stale, COMMIT),
        KeyDecision::Ignore
    ));
    assert!(matches!(
        state.finish_key(current, COMMIT),
        KeyDecision::InputMethod {
            accept_sync_commit: true
        }
    ));
    let stale = state
        .begin_key(Some(KEY_A), KeyPhase::Press, false, true)
        .unwrap();
    state.bind_im_context();
    assert!(matches!(
        state.finish_key(stale, COMMIT),
        KeyDecision::Ignore
    ));
}

#[test]
fn repeat_preserves_owner_and_never_bootstraps_a_missing_press() {
    let mut state = KeyOwnershipState::new();
    state.bind_im_context();
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Press, true, true, COMMIT),
        KeyDecision::Ignore
    ));
    send_press(&mut state, KEY_A);
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Press, false, true, PASS),
        KeyDecision::Ignore
    ));
    let repeat = permit(
        key(&mut state, KEY_A, KeyPhase::Press, true, true, COMMIT),
        KEY_A,
        true,
    );
    assert!(state.confirm_physical_press(repeat, false));
    assert_eq!(state.owner(KEY_A), Some(KeyOwner::RemotePhysical));
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Release, false, true, IM),
        KeyDecision::RemoteRelease { .. }
    ));
    let _ = key(&mut state, KEY_B, KeyPhase::Press, false, true, IM);
    assert!(matches!(
        key(&mut state, KEY_B, KeyPhase::Press, true, true, PASS),
        KeyDecision::InputMethod {
            accept_sync_commit: false
        }
    ));
}

#[test]
fn local_held_key_cannot_turn_remote_after_focus_change() {
    let mut state = KeyOwnershipState::new();
    state.bind_im_context();
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Press, false, false, COMMIT),
        KeyDecision::Local
    ));
    assert_eq!(state.owner(KEY_A), Some(KeyOwner::Local));
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Press, true, true, PASS),
        KeyDecision::Local
    ));
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Release, false, true, PASS),
        KeyDecision::Local
    ));
    send_press(&mut state, KEY_A);
}

#[test]
fn unknown_keys_and_missing_context_never_authorize_text() {
    let mut state = KeyOwnershipState::new();
    let token = state.bind_im_context();
    let event = state.begin_key(None, KeyPhase::Press, false, true).unwrap();
    assert!(matches!(
        state.finish_key(event, COMMIT),
        KeyDecision::Ignore
    ));
    assert!(!state.accept_async_commit(&token));
    state.reset_epoch();
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Press, false, true, COMMIT),
        KeyDecision::InputMethod {
            accept_sync_commit: false
        }
    ));
}

#[test]
fn local_or_physical_press_revokes_pending_async_text() {
    for local in [false, true] {
        let mut state = KeyOwnershipState::new();
        let token = state.bind_im_context();
        let _ = key(&mut state, KEY_A, KeyPhase::Press, false, true, IM);
        let _ = key(&mut state, KEY_B, KeyPhase::Press, false, !local, PASS);
        assert!(!state.accept_async_commit(&token));
    }
}

#[test]
fn reset_drops_owners_and_stale_filter_without_generating_second_release_policy() {
    let mut state = KeyOwnershipState::new();
    send_press(&mut state, KEY_A);
    let pending = state
        .begin_key(Some(KEY_B), KeyPhase::Press, false, true)
        .unwrap();
    state.reset_epoch();
    assert_eq!(state.owner(KEY_A), None);
    assert!(matches!(
        state.finish_key(pending, COMMIT),
        KeyDecision::Ignore
    ));
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Release, false, true, PASS),
        KeyDecision::Ignore
    ));
}

#[test]
fn context_rebind_preserves_sent_release_but_invalidates_old_im_ownership() {
    let mut state = KeyOwnershipState::new();
    let old = state.bind_im_context();
    send_press(&mut state, KEY_A);
    let _ = key(&mut state, KEY_B, KeyPhase::Press, false, true, IM);
    let current = state.bind_im_context();
    assert_eq!(state.owner(KEY_A), Some(KeyOwner::RemotePhysical));
    assert_eq!(state.owner(KEY_B), None);
    assert!(!state.accept_async_commit(&old));
    assert!(!state.accept_async_commit(&current));
    assert!(matches!(
        key(&mut state, KEY_B, KeyPhase::Release, false, true, COMMIT),
        KeyDecision::Ignore
    ));
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Release, false, true, IM),
        KeyDecision::RemoteRelease {
            hardware_keycode: KEY_A
        }
    ));
}

#[test]
fn consumed_release_cannot_reopen_authorization_after_sync_commit() {
    let mut state = KeyOwnershipState::new();
    let token = state.bind_im_context();
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Press, false, true, COMMIT),
        KeyDecision::InputMethod {
            accept_sync_commit: true
        }
    ));
    let _ = key(&mut state, KEY_A, KeyPhase::Release, false, true, IM);
    assert!(!state.accept_async_commit(&token));
}

#[test]
fn consumed_release_cannot_reopen_authorization_revoked_by_other_key() {
    for local in [false, true] {
        let mut state = KeyOwnershipState::new();
        let token = state.bind_im_context();
        let _ = key(&mut state, KEY_A, KeyPhase::Press, false, true, IM);
        let _ = key(&mut state, KEY_B, KeyPhase::Press, false, !local, PASS);
        let _ = key(&mut state, KEY_A, KeyPhase::Release, false, true, IM);
        assert!(!state.accept_async_commit(&token), "local={local}");
    }
}

#[test]
fn release_cannot_accept_sync_commit_after_authorization_was_consumed() {
    let mut state = KeyOwnershipState::new();
    let token = state.bind_im_context();
    let _ = key(&mut state, KEY_A, KeyPhase::Press, false, true, IM);
    assert!(state.accept_async_commit(&token));
    assert!(matches!(
        key(&mut state, KEY_A, KeyPhase::Release, false, true, COMMIT),
        KeyDecision::InputMethod {
            accept_sync_commit: false
        }
    ));
}
