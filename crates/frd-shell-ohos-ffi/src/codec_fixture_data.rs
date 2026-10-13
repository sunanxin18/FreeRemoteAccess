// 固定合成输入及独立宿主像素参考；身份见 tests/codec-fixtures/provenance.json。
pub(super) fn cases() -> Vec<Case> {
    vec![
        Case {
            id: "synthetic-avc420-2x2.h264",
            width: 16,
            height: 16,
            visible_width: 2,
            visible_height: 2,
            hevc: false,
            y420: true,
            units: &[include_bytes!(
                "../tests/codec-fixtures/synthetic-avc420-2x2.h264"
            )],
            hashes: &["b28c1c9104f55fc9f04aa9079088c74ae5bfaf5ec86c7ec3982e0fb403f52668"],
            references: &[include_bytes!(
                "../tests/codec-fixtures/synthetic-avc420-2x2.h264.yuv420p"
            )],
        },
        Case {
            id: "synthetic-avc444-16x16.h264",
            width: 16,
            height: 16,
            visible_width: 16,
            visible_height: 16,
            hevc: false,
            y420: false,
            units: &[include_bytes!(
                "../tests/codec-fixtures/synthetic-avc444-16x16.h264"
            )],
            hashes: &["2cbc50f8faeb4a668e16a920021d8ab5a5c046789c8ee294114bac585c6212d6"],
            references: &[include_bytes!(
                "../tests/codec-fixtures/synthetic-avc444-16x16.h264.yuv444p"
            )],
        },
        Case {
            id: "synthetic-main444-cropped-16x8.hevc",
            width: 16,
            height: 16,
            visible_width: 16,
            visible_height: 8,
            hevc: true,
            y420: false,
            units: &[include_bytes!(
                "../tests/codec-fixtures/synthetic-main444-cropped-16x8.hevc"
            )],
            hashes: &["bf515c65f3f50db2eaa3639554f4af11ad24e744c73236a479e1d1f8251d0f5c"],
            references: &[include_bytes!(
                "../tests/codec-fixtures/synthetic-main444-cropped-16x8.hevc.yuv444p"
            )],
        },
        Case {
            id: "synthetic-avc420-256x144-2frames",
            width: 256,
            height: 144,
            visible_width: 256,
            visible_height: 144,
            hevc: false,
            y420: true,
            units: &[
                include_bytes!("../tests/codec-fixtures/synthetic-avc420-256x144-2frames-au0.h264"),
                include_bytes!("../tests/codec-fixtures/synthetic-avc420-256x144-2frames-au1.h264"),
            ],
            hashes: &[
                "c4e92b1f89263b7cb3ce3ce6906f5fb6c3c9f8916773d3bbcce862386a9a1967",
                "7c3889527ff8d3d973aaa8c0d98079f6e2085fc5d8a9fc0c770c08d363e71862",
            ],
            references: &[
                include_bytes!(
                    "../tests/codec-fixtures/synthetic-avc420-256x144-2frames-frame0.yuv420p"
                ),
                include_bytes!(
                    "../tests/codec-fixtures/synthetic-avc420-256x144-2frames-frame1.yuv420p"
                ),
            ],
        },
        Case {
            id: "synthetic-avc444-256x144-2frames",
            width: 256,
            height: 144,
            visible_width: 256,
            visible_height: 144,
            hevc: false,
            y420: false,
            units: &[
                include_bytes!("../tests/codec-fixtures/synthetic-avc444-256x144-2frames-au0.h264"),
                include_bytes!("../tests/codec-fixtures/synthetic-avc444-256x144-2frames-au1.h264"),
            ],
            hashes: &[
                "1bdea25b997c252cac0ecfc4137c953a20c4e28c5f0942b5360c98bc89d9a88e",
                "0dcb703c862f9d6cbf03d651af89597096cdedd3436a162224322e80deca0b9e",
            ],
            references: &[
                include_bytes!(
                    "../tests/codec-fixtures/synthetic-avc444-256x144-2frames-frame0.yuv444p"
                ),
                include_bytes!(
                    "../tests/codec-fixtures/synthetic-avc444-256x144-2frames-frame1.yuv444p"
                ),
            ],
        },
        Case {
            id: "synthetic-avc420-2560x1440-2frames",
            width: 2560,
            height: 1440,
            visible_width: 2560,
            visible_height: 1440,
            hevc: false,
            y420: true,
            units: &[
                include_bytes!(
                    "../tests/codec-fixtures/synthetic-avc420-2560x1440-2frames-au0.h264"
                ),
                include_bytes!(
                    "../tests/codec-fixtures/synthetic-avc420-2560x1440-2frames-au1.h264"
                ),
            ],
            hashes: &[
                "9daf1aef059630d6d1dd0417dcaced5bd16dd2f5d6d9f19172d52672af5ce445",
                "79303f29730a5e207f68df999a89d42726edf2ad1cf5361ed222a4c1a81c59e0",
            ],
            references: &[],
        },
        Case {
            id: "synthetic-avc420-3840x2160-2frames",
            width: 3840,
            height: 2160,
            visible_width: 3840,
            visible_height: 2160,
            hevc: false,
            y420: true,
            units: &[
                include_bytes!(
                    "../tests/codec-fixtures/synthetic-avc420-3840x2160-2frames-au0.h264"
                ),
                include_bytes!(
                    "../tests/codec-fixtures/synthetic-avc420-3840x2160-2frames-au1.h264"
                ),
            ],
            hashes: &[
                "20c1fb8cace6bf6dc5067d2ff96fab78db7a900c127146faa218f793ce86e96a",
                "0a46a9f1bc80cd78412bde200917b9ca45a0031e7a6b0311402f50ebac64bce2",
            ],
            references: &[],
        },
    ]
}
