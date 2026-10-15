use super::*;
#[cfg(feature = "normal-hap-rdp-validation")]
use frd_core::ProtocolId;
use frd_core::SessionId;
use frd_media_api::VideoStreamIdentity;

#[test]
fn actual_root_has_only_explicit_rdp_on_ohos_arm64() {
    let factories = factories_for_platform(true);
    #[cfg(feature = "normal-hap-rdp-validation")]
    assert_eq!(
        factories
            .iter()
            .map(|f| f.descriptor().id)
            .collect::<Vec<_>>(),
        vec![ProtocolId::rdp()]
    );
    #[cfg(not(any(
        feature = "normal-hap-rdp-validation",
        all(debug_assertions, feature = "native-presentation-fixture")
    )))]
    assert!(factories.is_empty());
    #[cfg(all(debug_assertions, feature = "native-presentation-fixture"))]
    assert_eq!(factories.len(), 1);
}
#[test]
fn non_ohos_never_gains_rdp_from_validation_flag() {
    #[cfg(not(all(debug_assertions, feature = "native-presentation-fixture")))]
    assert!(factories_for_platform(false).is_empty());
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
fn c_abi_normal_create_keeps_form_idle_and_selects_only_target_rdp() {
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
    for target in [1, 2, 3] {
        let edit = serde_json::to_vec(&serde_json::json!({"target":target,"address":"example.invalid","port":3389,"username":"unused","protocol":"auto","remember":false})).unwrap();
        assert_eq!(
            unsafe { frd_ohos_app_edit(id, edit.as_ptr(), edit.len()) },
            crate::OK
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let value = loop {
            let v = snapshot();
            if v["initialized"] == true && v["target"] == target {
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
        assert_eq!(value["available"], rdp_enabled && target == 1);
        assert_eq!(value["statusCode"], "connection_form");
        assert_eq!(value["canConnect"], false);
        assert_eq!(value["inputReady"], false);
        assert!(value.get("syntheticFixture").is_none());
    }
    std::thread::sleep(Duration::from_millis(50));
    let value = snapshot();
    assert_eq!(value["statusCode"], "connection_form");
    assert_eq!(value["address"], "example.invalid");
    assert_eq!(value["session"], "");
}
