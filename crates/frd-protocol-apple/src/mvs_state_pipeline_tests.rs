fn trace(label: &str, state: &MvsDecodeState) {
    println!(
        "MVS_PIPELINE_V1|state/{label}|{}",
        state.pipeline_snapshot()
    );
}

fn prepared(state: &mut MvsDecodeState) -> PreparedGenerationMvs {
    let MvsDecodeDecision::Prepared(value) = state
        .prepare(7, &mode_five_seed_full_payload(), 8, 8)
        .unwrap()
    else {
        panic!("预期 type0 prepare");
    };
    value
}

#[test]
fn pipeline_differential_generation_ownership() {
    let mut state = MvsDecodeState::new(7);
    trace("initial", &state);
    assert!(matches!(
        state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap(),
        MvsDecodeDecision::RequestFull(MvsResyncReason::MissingTables)
    ));
    state.install_tables(7, &type_two_tables()).unwrap();
    assert!(state.awaiting_full());
    trace("tables", &state);
    let first = prepared(&mut state);
    let stale_sibling = prepared(&mut state);
    state.commit(first).unwrap();
    assert!(!state.awaiting_full());
    let before = state.pipeline_snapshot();
    assert!(state.commit(stale_sibling).is_err());
    assert_eq!(state.pipeline_snapshot(), before);
    trace("owner-sibling-rejected", &state);
    let mut foreign = MvsDecodeState::new(7);
    foreign.install_tables(7, &type_two_tables()).unwrap();
    let foreign_before = foreign.pipeline_snapshot();
    assert!(foreign.commit(prepared(&mut state)).is_err());
    assert_eq!(foreign.pipeline_snapshot(), foreign_before);
    assert_eq!(state.pipeline_snapshot(), before);
    trace("foreign-owner-rejected", &foreign);

    let partial = partial_payload(
        |bits| {
            bits.write_bits(0, 2);
        },
        0,
        0,
    );
    let MvsDecodeDecision::PreparedOpaque(opaque) = state.prepare(7, &partial, 8, 8).unwrap()
    else {
        panic!("预期 type1 prepare");
    };
    let old = prepared(&mut state);
    state.commit_opaque(opaque).unwrap();
    assert!(!state.awaiting_full());
    let before = state.pipeline_snapshot();
    assert!(state.commit(old).is_err());
    assert_eq!(state.pipeline_snapshot(), before);
    trace("opaque-owner-rotation", &state);

    let old = prepared(&mut state);
    let MvsDecodeDecision::PreparedOpaque(old_opaque) = state.prepare(7, &partial, 8, 8).unwrap()
    else {
        panic!("预期 type1 prepare");
    };
    let mut tables = type_two_tables();
    tables[1] = 64;
    tables[65] = 3;
    state.install_tables(7, &tables).unwrap();
    let before = state.pipeline_snapshot();
    assert!(state.commit(old).is_err());
    assert!(state.commit_opaque(old_opaque).is_err());
    assert_eq!(state.pipeline_snapshot(), before);
    assert!(state.awaiting_full());
    trace("table-replacement-rejects-both", &state);
    let foreign_before = foreign.pipeline_snapshot();
    assert!(state.commit(prepared(&mut foreign)).is_err());
    assert_eq!(state.pipeline_snapshot(), before);
    assert_eq!(foreign.pipeline_snapshot(), foreign_before);
    let full = prepared(&mut state);
    state.commit(full).unwrap();
    trace("replacement-full", &state);

    let old = prepared(&mut state);
    let MvsDecodeDecision::PreparedOpaque(old_opaque) = state.prepare(7, &partial, 8, 8).unwrap()
    else {
        panic!("预期 type1 prepare");
    };
    state.reset(8);
    let before = state.pipeline_snapshot();
    assert!(state.commit(old).is_err());
    assert!(state.commit_opaque(old_opaque).is_err());
    assert!(matches!(
        state.prepare(7, &mode_zero_full_payload(), 8, 8).unwrap(),
        MvsDecodeDecision::IgnoreStale
    ));
    assert!(state.install_tables(7, &tables).is_err());
    assert!(state.request_full(7).is_err());
    assert_eq!(state.pipeline_snapshot(), before);
    assert!(state.awaiting_full());
    trace("reset-stale-rejected", &state);
    state.install_tables(8, &tables).unwrap();
    assert!(matches!(
        state.prepare(8, &[0], 8, 8).unwrap(),
        MvsDecodeDecision::RequestFull(MvsResyncReason::MalformedPayload)
    ));
    trace("malformed-await", &state);
    println!("MVS_PIPELINE_V1|state/DONE|full_commits=2;opaque_commits=1;sibling_reject=1;foreign_reject=2;opaque_rotation_reject=1;table_reject=2;reset_commit_reject=2;stale_prepare=1;stale_tables=1;stale_request=1");
}
