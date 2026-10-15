// 共享已证明的 wire fixtures；默认及 native 两种编译运行同一确定序列。
fn trace(label: &str, decoder: &MvsFullDecoder) {
    println!(
        "MVS_PIPELINE_V1|decoder/{label}|{}",
        decoder.pipeline_snapshot()
    );
}

fn commit_full(
    decoder: &mut MvsFullDecoder,
    modes: &[u8],
    data: &[u8],
    width: usize,
    height: usize,
) {
    let prepared = decoder
        .prepare(&record_with_thresholds(modes, data, 15, 31), width, height)
        .unwrap();
    assert_eq!(
        MvsFullDecoder::decoded(&prepared).rgb.len(),
        width * height * 3
    );
    decoder.commit(prepared);
}

#[test]
fn pipeline_differential_complete_record_state() {
    let mut decoder = decoder_with_tables(64, 1);
    trace("initial", &decoder);
    let modes = mode_stream(&[(5, 1)]);
    let data = mode_five_data(&["0_1_0_00_010_0010", "1"]);
    let before = decoder.pipeline_snapshot();
    let dropped = decoder
        .prepare(&record_with_thresholds(&modes, &data, 15, 31), 16, 8)
        .unwrap();
    assert_eq!(pixel(MvsFullDecoder::decoded(&dropped), 0, 0), [150; 3]);
    assert_eq!(
        complete_tile(MvsFullDecoder::decoded(&dropped), 0, 0),
        complete_tile(MvsFullDecoder::decoded(&dropped), 8, 0)
    );
    drop(dropped);
    assert_eq!(decoder.pipeline_snapshot(), before);
    trace("drop", &decoder);
    commit_full(&mut decoder, &modes, &data, 16, 8);
    assert_eq!(
        coefficient_seed_snapshot(&decoder, 0),
        coefficient_seed_snapshot(&decoder, 1)
    );
    trace("ac-copy", &decoder);

    // 非均匀量化与两个 tile 的 DC predictor：复用既有不对称种子。
    let mut luma = [1; 64];
    luma[1] = 64;
    decoder.replace_tables(MvsTables {
        luminance: luma,
        chrominance: [2; 64],
    });
    commit_full(
        &mut decoder,
        &mode_stream(&[(5, 1)]),
        &asymmetric_seed_mode_five_data(2),
        16,
        8,
    );
    assert_ne!(
        coefficient_seed_snapshot(&decoder, 0),
        coefficient_seed_snapshot(&decoder, 1)
    );
    trace("dc-predictors-tables", &decoder);

    let populate = partial_payload(
        1,
        1,
        &["01_000000_0_1010_0_1010", "01_000000_0_1010_0_1010"],
    );
    let before = decoder.pipeline_snapshot();
    let dropped = decoder
        .prepare_partial(&populate, 0, 0, 16, 8, 16, 8)
        .unwrap();
    drop(dropped);
    assert_eq!(decoder.pipeline_snapshot(), before);
    let prepared = decoder
        .prepare_partial(&populate, 0, 0, 16, 8, 16, 8)
        .unwrap();
    assert_eq!(prepared.partial_pixels.operations.len(), 2);
    decoder.commit_opaque(prepared);
    assert_eq!(cache_index_snapshot(&decoder), (0, 2, 2));
    assert_eq!(cache_entry_snapshot(&decoder, 1).unwrap()[0], 80);
    trace("populate", &decoder);
    // 非 EOB chroma AC 再走一次完整 prepare/commit，保留精确 cache 字节。
    let ac = partial_payload(19, 14, &["01_000000_0_1100_0_1010_0_1010"]);
    let prepared = decoder.prepare_partial(&ac, 0, 0, 8, 8, 16, 8).unwrap();
    decoder.commit_opaque(prepared);
    assert_eq!(cache_entry_snapshot(&decoder, 3).unwrap()[66], -1);
    trace("partial-ac-fixed-extents", &decoder);

    for (label, bits, expected) in [
        ("lookup-one", "11_0_00000000_00000001", 1),
        ("lookup-next", "11_1", 2),
        ("lookup-miss", "11_0_00000000_00000100", 2),
    ] {
        let partial = partial_payload(0, 0, &[bits]);
        let prepared = decoder
            .prepare_partial(&partial, 0, 0, 8, 8, 16, 8)
            .unwrap();
        if label == "lookup-miss" {
            assert!(prepared.partial_pixels.operations.is_empty());
        }
        decoder.commit_opaque(prepared);
        assert_eq!(cache_index_snapshot(&decoder), (expected, 3, 3));
        trace(label, &decoder);
    }
    // 边界状态注入仅加速已存在 64999 -> 1 cache 规则，无 wire 推测。
    decoder.cache_state.last_insert_index = 64_999;
    decoder.surface_state.as_mut().unwrap().tiles[0]
        .coefficients
        .as_mut()
        .unwrap()
        .cb_dc = 79;
    let partial = partial_payload(1, 1, &["01_000000_0_1010_0_1010"]);
    let prepared = decoder
        .prepare_partial(&partial, 0, 0, 8, 8, 16, 8)
        .unwrap();
    decoder.commit_opaque(prepared);
    assert_eq!(cache_index_snapshot(&decoder), (2, 1, 4));
    assert_eq!(cache_entry_snapshot(&decoder, 1).unwrap()[79], 79);
    trace("cache-wrap", &decoder);
    commit_full(
        &mut decoder,
        &mode_stream(&[(6, 0), (7, 0)]),
        &cache_mode_data(&[1]),
        16,
        8,
    );
    assert_eq!(cache_index_snapshot(&decoder).0, 2);
    trace("full-cache-six-seven", &decoder);

    decoder = decoder_with_tables(64, 1);
    // 完整 tile scan 的模式 0/1/2 与随后 opcode0/2 引用。
    commit_full(
        &mut decoder,
        &mode_stream(&[(0, 0), (1, 0), (2, 1)]),
        &terminal_only_data(),
        16,
        16,
    );
    assert_eq!(reference_snapshot(&decoder, 1), Some((0, 0)));
    let partial = partial_payload(0, 0, &["00", "10", "10", "10"]);
    let prepared = decoder
        .prepare_partial(&partial, 0, 0, 16, 16, 16, 16)
        .unwrap();
    assert_eq!(prepared.partial_pixels.operations.len(), 3);
    decoder.commit_opaque(prepared);
    assert!(decoder
        .surface_state
        .as_ref()
        .unwrap()
        .rgb
        .iter()
        .all(|v| *v == 255));
    trace("scan-references", &decoder);

    decoder = decoder_with_tables(64, 1);
    // mode3 位图及 mode4 色板，偏移子矩形，逻辑8x8写入裁剪到10x9。
    commit_full(
        &mut decoder,
        &mode_stream(&[(5, 3)]),
        &asymmetric_seed_mode_five_data(4),
        10,
        9,
    );
    trace("clipped-seeds", &decoder);
    let modes = mode_stream(&[(3, 0)]);
    let data = mode_three_zero_bitmap_data();
    let prepared = decoder
        .prepare_rect(&record(&modes, &data), 8, 8, 2, 1, 10, 9)
        .unwrap();
    decoder.commit(prepared);
    assert_eq!(
        &decoder.surface_state.as_ref().unwrap().rgb[(8 * 10 + 8) * 3..],
        &[255; 6]
    );
    trace("clipped-mode-three-offset", &decoder);
    let mut color = TestBitWriter::new();
    color.write_bits(0, 2);
    color.write_bits(100, 8);
    color.write_bits(16, 6);
    color.write_bits(48, 6);
    color.write_bits(0x6d, 8);
    let modes = mode_stream(&[(4, 0)]);
    let data = color.finish();
    let prepared = decoder
        .prepare_rect(&record(&modes, &data), 8, 8, 2, 1, 10, 9)
        .unwrap();
    assert_eq!(
        MvsFullDecoder::decoded(&prepared).rgb,
        [190, 76, 0, 190, 76, 0]
    );
    decoder.commit(prepared);
    trace("mode-four-offset", &decoder);

    for (label, bad) in [
        (
            "bad-terminal",
            partial_payload_with_terminals(1, 1, &["01_000000_0_1010_0_1010"], [0x6d, 0x76, 0x72]),
        ),
        ("truncated", vec![1, 1, 1, 0x40]),
    ] {
        let before = decoder.pipeline_snapshot();
        assert!(decoder.prepare_partial(&bad, 0, 0, 8, 8, 10, 9).is_err());
        assert_eq!(decoder.pipeline_snapshot(), before);
        trace(label, &decoder);
    }
    let before = decoder.pipeline_snapshot();
    let error = decoder
        .prepare_rect(&record(&mode_stream(&[(5, 0)]), &[0]), 0, 0, 8, 8, 10, 9)
        .unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        "MVS mode 5 Y DC 解码失败: MVS mode 5 DC q0 缺少 present 位: MVS 位流耗尽: bit offset 8 请求 1 位，仅余 0 位"
    );
    assert_eq!(decoder.pipeline_snapshot(), before);
    trace("full-truncated", &decoder);
    println!("MVS_PIPELINE_V1|decoder/DONE|full=7;partial=7;rejected=3;dropped=2;cache_capacity={MVS_CACHE_SLOT_COUNT}");
}
