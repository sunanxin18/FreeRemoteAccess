fn emit_patch(label: &str, patch: &PixelPatch) {
    assert_eq!(
        patch.pixels.as_bytes().len(),
        patch.stride_bytes as usize * patch.rect.height as usize
    );
    println!(
        "MVS_PIPELINE_V1|publication/{label}|rect={:?};stride={};bytes={:?}",
        patch.rect,
        patch.stride_bytes,
        patch.pixels.as_bytes()
    );
}

#[test]
fn pipeline_differential_real_record_publication() {
    let surface = native_surface(10, 9);
    let dynamic = native_runtime(10, 9);
    let mut receiver = MvsReceiveState::new(1);
    receiver.install_tables(&type_two_tables_fixture()).unwrap();
    let rect = MvsRect {
        x: 8,
        y: 8,
        width: 2,
        height: 1,
    };
    let seed = MvsRecord {
        rect,
        payload: native_mode_five_seed_payload(),
    };
    let MvsRecordOutcome::FullApplied {
        publication,
        complete_surface,
    } = apply_native_mvs_frame(&mut receiver, &seed, &surface, &dynamic).unwrap()
    else {
        panic!("预期 full publication");
    };
    assert!(!complete_surface);
    assert!(publication.contains_nonblack);
    assert_eq!(
        publication.patch.pixels.as_bytes(),
        &[128, 128, 128, 0, 128, 128, 128, 0]
    );
    emit_patch("type-zero-clipped", &publication.patch);
    assert_eq!(
        surface.lock().unwrap().framebuffer.pixels()[88..90],
        [0x00808080; 2]
    );
    let partial = MvsRecord {
        rect,
        payload: native_opcode_one_partial_payload(),
    };
    let MvsRecordOutcome::PartialApplied { patches } =
        apply_native_mvs_frame(&mut receiver, &partial, &surface, &dynamic).unwrap()
    else {
        panic!("预期 partial publication");
    };
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].rect, publication.patch.rect);
    assert_eq!(
        patches[0].pixels.as_bytes(),
        publication.patch.pixels.as_bytes()
    );
    emit_patch("type-one-clipped", &patches[0]);
    println!(
        "MVS_PIPELINE_V1|publication/real-framebuffer|{:?}",
        surface.lock().unwrap().framebuffer.pixels()
    );

    // 真实转换入口亦检查全黑、尾像素 nonblack，以及完整高字节与重叠 copy 的顺序。
    let mut rgb = vec![0; 27];
    let decoded = crate::mvs_full::DecodedMvsRect {
        width: 9,
        height: 1,
        rgb: rgb.clone(),
    };
    let rect = MvsRect {
        x: 1,
        y: 2,
        width: 9,
        height: 1,
    };
    let black = prepare_type_zero_patch(&decoded, rect).unwrap();
    assert!(!black.contains_nonblack);
    emit_patch("black", &black.patch);
    rgb[26] = 7;
    let decoded = crate::mvs_full::DecodedMvsRect {
        width: 9,
        height: 1,
        rgb,
    };
    let tail = prepare_type_zero_patch(&decoded, rect).unwrap();
    assert!(tail.contains_nonblack);
    assert_eq!(&tail.patch.pixels.as_bytes()[32..], &[7, 0, 0, 0]);
    emit_patch("tail-nonblack", &tail.patch);

    use crate::mvs_full::{PartialPixelOperation, PreparedPartialPixels};
    let mut framebuffer = crate::surface_publisher::CpuFramebuffer::new(5, 2).unwrap();
    let initial = [
        0x81112233, 0x92445566, 0xa3aabbcc, 0xb4ddeeff, 0xc5010203, 0xd6040506, 0xe7070809,
        0xf80a0b0c, 0x190d0e0f, 0x2a101112,
    ];
    framebuffer.pixels_mut().copy_from_slice(&initial);
    let pixels = PreparedPartialPixels {
        operations: vec![
            PartialPixelOperation::Copy {
                source_x: 0,
                source_y: 0,
                destination_x: 1,
                destination_y: 0,
                width: 4,
                height: 2,
            },
            PartialPixelOperation::Copy {
                source_x: 1,
                source_y: 0,
                destination_x: 0,
                destination_y: 0,
                width: 4,
                height: 2,
            },
            PartialPixelOperation::Replace {
                x: 4,
                y: 1,
                width: 1,
                height: 1,
                rgb: vec![3, 2, 1],
            },
        ],
    };
    let plan = plan_type_one_patches(&framebuffer, &pixels).unwrap();
    apply_validated_partial_pixels_to_framebuffer(&mut framebuffer, &pixels);
    assert_eq!(
        framebuffer.pixels(),
        &[
            initial[0], initial[1], initial[2], initial[3], initial[3], initial[5], initial[6],
            initial[7], initial[8], 0x00030201
        ]
    );
    let patches = fill_type_one_patches_from_committed_surface(&framebuffer, plan);
    assert_eq!(patches.len(), 2);
    assert_eq!(
        &patches[0].pixels.as_bytes()[0..4],
        &[0x33, 0x22, 0x11, 0x81]
    );
    emit_patch("overlap-high-byte-0", &patches[0]);
    emit_patch("overlap-high-byte-1", &patches[1]);
    println!(
        "MVS_PIPELINE_V1|publication/overlap-framebuffer|{:?}",
        framebuffer.pixels()
    );
    println!(
        "MVS_PIPELINE_V1|publication/DONE|real_type0=1;real_type1=1;conversion=2;copy=2;replace=1"
    );
}
