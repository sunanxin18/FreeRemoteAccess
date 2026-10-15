use super::*;
use frd_core::ProtocolId;
use frd_core::SessionId;
use frd_media_api::VideoStreamIdentity;

#[test]
fn actual_root_accumulates_only_enabled_protocols_for_supported_platform() {
    let factories = factories_for_platform(true);
    #[allow(unused_mut)]
    let mut expected = vec![];
    #[cfg(feature = "normal-hap-rdp-validation")]
    expected.push(ProtocolId::rdp());
    #[cfg(all(
        feature = "normal-hap-apple-mvs-validation",
        target_env = "ohos",
        target_arch = "aarch64"
    ))]
    expected.push(ProtocolId::apple_hpss_mvs());
    #[cfg(all(debug_assertions, feature = "native-presentation-fixture"))]
    expected.push(factories[0].descriptor().id.clone());
    assert_eq!(
        factories
            .iter()
            .map(|f| f.descriptor().id)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(!expected.contains(&ProtocolId::apple_high_performance()));
}
#[test]
fn unsupported_platform_never_gains_ordinary_factories() {
    #[cfg(not(all(debug_assertions, feature = "native-presentation-fixture")))]
    assert!(factories_for_platform(false).is_empty());
}
#[test]
#[cfg(not(all(debug_assertions, feature = "native-presentation-fixture")))]
fn actual_catalog_keeps_mac_auto_hp_unregistered_and_requires_explicit_standard() {
    use frd_core::TargetSystem;
    use frd_protocol_api::{ProtocolCatalog, ProtocolError, ProtocolSelection};
    // 采用真实平台谓词，宿主不构造替身来证明目标注册。
    let factories = factories_for_platform(cfg!(all(target_env = "ohos", target_arch = "aarch64")));
    let catalog = ProtocolCatalog::new(factories.iter().map(|f| f.descriptor().id));
    assert_eq!(
        catalog.select(TargetSystem::MacOs, ProtocolSelection::Automatic),
        Err(ProtocolError::UnregisteredProtocol)
    );
    let expected = if cfg!(all(
        feature = "normal-hap-apple-mvs-validation",
        target_env = "ohos",
        target_arch = "aarch64"
    )) {
        Ok(ProtocolId::apple_hpss_mvs())
    } else {
        Err(ProtocolError::UnregisteredProtocol)
    };
    assert_eq!(
        catalog.select(
            TargetSystem::MacOs,
            ProtocolSelection::Explicit(ProtocolId::apple_hpss_mvs())
        ),
        expected
    );
    assert_eq!(
        catalog.select(
            TargetSystem::Windows,
            ProtocolSelection::Explicit(ProtocolId::apple_hpss_mvs())
        ),
        Err(ProtocolError::UnsupportedTargetProtocol)
    );
    assert!(catalog
        .descriptor(&ProtocolId::apple_high_performance())
        .is_none());
}
#[test]
fn ordinary_root_keeps_video_and_audio_unavailable() {
    let identity = VideoStreamIdentity {
        session_id: SessionId::allocate(),
        stream_id: 1,
    };
    assert_eq!(
        video_loader()(identity).err().unwrap().code(),
        frd_media_api::VideoDecodeErrorCode::BackendUnavailable
    );
    assert!(matches!(
        NoAudio.open(),
        Err(frd_media_api::AudioOutputError::Unavailable)
    ));
}

// 真实导出入口经过组合根和同一个 AppEngine；仅编辑，不提交或打开网络。
#[test]
#[cfg(not(all(debug_assertions, feature = "native-presentation-fixture")))]
fn c_abi_normal_create_keeps_form_idle_and_exact_protocol_selection() {
    use crate::app_abi::*;
    use std::time::{Duration, Instant};
    let root = std::env::temp_dir().join(format!(
        "frd-composition-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let path = root.to_str().unwrap().as_bytes();
    let mut id = 0;
    assert_eq!(
        unsafe { frd_ohos_app_create(path.as_ptr(), path.len(), &mut id) },
        crate::OK
    );
    assert_ne!(id, 0);
    struct App(u64);
    impl Drop for App {
        fn drop(&mut self) {
            assert_eq!(frd_ohos_app_destroy(self.0), crate::OK);
        }
    }
    let _app = App(id);
    let snapshot = || {
        let mut p = std::ptr::null_mut();
        let mut n = 0;
        assert_eq!(
            unsafe { frd_ohos_app_snapshot(id, &mut p, &mut n) },
            crate::OK
        );
        let value: serde_json::Value =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(p, n) }).unwrap();
        unsafe { frd_ohos_app_snapshot_free(p, n) };
        value
    };
    for (target, protocol) in [
        (1, "auto"),
        (2, "auto"),
        (3, "auto"),
        (2, "apple-hpss-mvs"),
        (1, "apple-hpss-mvs"),
        (2, "apple-high-performance"),
        (1, "rdp"),
    ] {
        let edit = serde_json::to_vec(&serde_json::json!({"target":target,"address":"example.invalid","port":3389,"username":"unused","protocol":protocol,"remember":false})).unwrap();
        assert_eq!(
            unsafe { frd_ohos_app_edit(id, edit.as_ptr(), edit.len()) },
            crate::OK
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let value = loop {
            let v = snapshot();
            if v["initialized"] == true && v["target"] == target && v["protocol"] == protocol {
                break v;
            }
            assert!(Instant::now() < deadline, "应用 owner 未处理 edit");
            std::thread::sleep(Duration::from_millis(5));
        };
        let rdp_enabled = cfg!(all(
            feature = "normal-hap-rdp-validation",
            target_env = "ohos",
            target_arch = "aarch64"
        ));
        let apple_enabled = cfg!(all(
            feature = "normal-hap-apple-mvs-validation",
            target_env = "ohos",
            target_arch = "aarch64"
        ));
        assert_eq!(
            value["available"],
            (rdp_enabled && target == 1 && (protocol == "auto" || protocol == "rdp"))
                || (apple_enabled && target == 2 && protocol == "apple-hpss-mvs")
        );
        assert_eq!(value["statusCode"], "connection_form");
        assert_eq!(value["canConnect"], false);
        assert_eq!(value["inputReady"], false);
        assert_eq!(value["physicalKeyInput"], false, "表单没有协议输入能力");
        assert_eq!(value["textInput"], false);
        assert_eq!(
            value["ime"]["installed"],
            cfg!(all(target_env = "ohos", target_arch = "aarch64"))
        );
        assert_eq!(
            value["ime"]["state"],
            if cfg!(all(target_env = "ohos", target_arch = "aarch64")) {
                "detached"
            } else {
                "unavailable"
            }
        );
        assert_eq!(value["ime"]["ticket"], serde_json::Value::Null);
        assert_eq!(
            value["keyboardBackend"],
            if cfg!(all(target_env = "ohos", target_arch = "aarch64")) {
                "arkui_limited"
            } else {
                "unavailable"
            }
        );
        assert!(value["keyboardBackendReason"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
        assert!(value.get("syntheticFixture").is_none());
    }
    std::thread::sleep(Duration::from_millis(50));
    let value = snapshot();
    assert_eq!(value["statusCode"], "connection_form");
    assert_eq!(value["address"], "example.invalid");
    assert_eq!(value["session"], "");
}

#[test]
fn system_ime_installation_requires_explicit_ohos_arm64_composition() {
    assert_eq!(ime_for_platform(true).name(), "system_ime_v1");
    assert_eq!(ime_for_platform(false).name(), "unavailable");
}
