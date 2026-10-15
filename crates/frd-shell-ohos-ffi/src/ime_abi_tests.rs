fn ime_abi_fixture(
    physical: bool,
) -> (
    Arc<AppEngine>,
    u64,
    Arc<crate::presentation::Link>,
    crate::presentation::Epoch,
    std::sync::mpsc::Receiver<frd_core::SessionInput>,
) {
    use crate::app_adapter::Command;
    use crate::ime_policy::Installation;
    use crate::keyboard_policy::OhosKeyboardQualification;
    let (inputs, received) = std::sync::mpsc::channel();
    let engine = Arc::new(
        AppEngine::spawn_with_input(
            isolated_abi_stores(),
            vec![Arc::new(CaptureFactory(inputs))],
            Arc::new(|_| {
                Err(frd_media_api::VideoDecodeError::new(
                    frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
                ))
            }),
            Arc::new(NoAudio),
            if physical {
                OhosKeyboardQualification::ArkUiLimited
            } else {
                OhosKeyboardQualification::Unavailable
            },
            Installation::SystemImeV1,
        )
        .unwrap(),
    );
    let id = {
        let mut registry = apps().lock().unwrap();
        registry.0 += 1;
        let id = registry.0;
        registry.1.insert(id, engine.clone());
        id
    };
    let link = Arc::new(crate::presentation::Link::default());
    link.surface(crate::PocStatus {
        surface_generation: 1,
        width: 100,
        height: 100,
        attached: 1,
        foreground: 1,
        ..Default::default()
    });
    engine.bind(link.clone()).unwrap();
    engine
        .send(Command::Geometry(ViewGeometry {
            drawable: PixelSize {
                width: 100,
                height: 100,
            },
            bounds: PixelRect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            scale: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }))
        .unwrap();
    engine
        .send(Command::Edit(crate::app_adapter::Edit {
            target: 1,
            address: "fixture.invalid".into(),
            port: 3389,
            username: "synthetic".into(),
            protocol: "rdp".into(),
            remember: false,
        }))
        .unwrap();
    engine
        .send(Command::Password(SecretBuffer::from_text(
            "in-memory-keyboard-fixture".into(),
        )))
        .unwrap();
    wait_snapshot(&engine, |s| s["canConnect"] == true);
    engine
        .send(Command::Submit {
            event: 1,
            composition: false,
            repeat: false,
        })
        .unwrap();
    let waiting = wait_snapshot(&engine, |s| {
        s["page"] == 2 && s["acceptedSurface"]["generation"] == "1"
    });
    assert_eq!(waiting["physicalKeyInput"], physical);
    assert_eq!(waiting["textInput"], false);
    assert_eq!(waiting["inputReady"], false);
    let deadline = Instant::now() + Duration::from_secs(3);
    let epoch = loop {
        if let Some(epoch) = link.0.lock().unwrap().epoch() {
            break epoch;
        }
        assert!(Instant::now() < deadline, "真实 geometry 未完成接线");
        std::thread::sleep(Duration::from_millis(1));
    };
    link.0.lock().unwrap().feedback = Some(crate::presentation::Feedback {
        epoch,
        event: Some(frd_protocol_api::PresentationEvent::FramePresented {
            session_id: epoch.session,
            generation: 1,
            revision: 1,
            completeness: frd_frame::FrameCompleteness::FullBaseline,
        }),
    });
    wait_snapshot(&engine, |s| s["inputReady"] == true);
    engine
        .send(Command::Focus {
            foreground: true,
            focused: true,
            remote: true,
        })
        .unwrap();
    let local = serde_json::to_vec(&crate::local_display::sample_message(1, 1, 11, true)).unwrap();
    assert_eq!(
        unsafe { frd_ohos_app_local_display(id, local.as_ptr(), local.len()) },
        OK
    );
    wait_snapshot(&engine, |s| {
        s["ime"]["sourceAvailable"] == true && s["localDisplayEvidence"]["windowId"] == 11
    });
    (engine, id, link, epoch, received)
}
struct ImeAbiGuard(u64);
impl Drop for ImeAbiGuard {
    fn drop(&mut self) {
        if let Ok(mut registry) = apps().lock() {
            if let Some(engine) = registry.1.remove(&self.0) {
                engine.stop();
            }
        }
    }
}
fn ime_abi_source(id: u64, ticket: &serde_json::Value, state: &str) -> i32 {
    let bytes =
        serde_json::to_vec(&serde_json::json!({"abiVersion":1,"state":state,"ticket":ticket}))
            .unwrap();
    unsafe { frd_ohos_app_ime_source(id, bytes.as_ptr(), bytes.len()) }
}
fn ime_abi_operation(id: u64, ticket: &serde_json::Value, serial: u64, text: &str) -> i32 {
    let bytes=serde_json::to_vec(&serde_json::json!({"abiVersion":1,"ticket":ticket,"serial":serial,"kind":"text","text":text})).unwrap();
    unsafe { frd_ohos_app_ime_operation(id, bytes.as_ptr(), bytes.len()) }
}
#[test]
fn ime_abi_actual_fifo_keeps_identical_commits_and_rejects_unticketed_channel() {
    for physical in [false, true] {
        let (engine, id, _, epoch, received) = ime_abi_fixture(physical);
        let _guard = ImeAbiGuard(id);
        let ticket = serde_json::json!({"windowStage":1,"windowId":11,"editorSerial":1,
            "session":epoch.session.get().to_string(),"generation":"1"});
        assert_eq!(ime_abi_source(id, &ticket, "attached"), OK);
        wait_snapshot(&engine, |s| s["ime"]["state"] == "attached");
        assert_eq!(
            unsafe {
                frd_ohos_app_text(
                    id,
                    epoch.session.get(),
                    1,
                    epoch.geometry,
                    b"unticketed".as_ptr(),
                    10,
                )
            },
            UNSUPPORTED_PLATFORM
        );
        assert_eq!(ime_abi_operation(id, &ticket, 1, "committed示例"), OK);
        assert_eq!(ime_abi_operation(id, &ticket, 2, "committed示例"), OK);
        let mut events = Vec::new();
        while events.len() < 2 {
            let input = received.recv_timeout(Duration::from_secs(2)).unwrap();
            if input.event != InputEvent::ReleaseAll {
                events.push(input.event);
            }
        }
        assert_eq!(
            events,
            vec![
                InputEvent::Text {
                    utf8: "committed示例".into()
                },
                InputEvent::Text {
                    utf8: "committed示例".into()
                }
            ]
        );
        let pair=serde_json::to_vec(&serde_json::json!({"abiVersion":1,"ticket":ticket,"serial":3,"kind":"keyPair","key":"Tab"})).unwrap();
        assert_eq!(
            unsafe { frd_ohos_app_ime_operation(id, pair.as_ptr(), pair.len()) },
            if physical { OK } else { UNSUPPORTED_PLATFORM }
        );
        if physical {
            for state in [KeyState::Pressed, KeyState::Released] {
                let input = received.recv_timeout(Duration::from_secs(2)).unwrap();
                assert_eq!(
                    input.event,
                    InputEvent::PhysicalKey {
                        code: PhysicalKeyCode::from_usb_hid_usage(0x2b),
                        state,
                        modifiers: Modifiers::default()
                    }
                );
            }
        }
        let serial = if physical { 3 } else { 2 };
        let snapshot = wait_snapshot(&engine, |s| s["ime"]["lastDispatchedSerial"] == serial);
        assert_eq!(snapshot["ime"]["lastAcceptedSerial"], serial);
        assert!(!snapshot.to_string().contains("committed示例"));
        assert_eq!(ime_abi_source(id, &ticket, "detached"), OK);
        wait_snapshot(&engine, |s| s["ime"]["state"] == "detached");
        assert_eq!(ime_abi_operation(id, &ticket, 3, "late"), INVALID_ARGUMENT);
    }
}
#[test]
fn ime_abi_strict_schema_rejects_versions_duplicates_types_lengths_keys_and_unsafe_ids() {
    let ticket = serde_json::json!({"windowStage":1,"windowId":11,"editorSerial":1,"session":"1","generation":"1"});
    let source = serde_json::json!({"abiVersion":1,"state":"attached","ticket":ticket});
    let op =
        serde_json::json!({"abiVersion":1,"ticket":ticket,"serial":1,"kind":"text","text":"valid"});
    let mut invalid_source = Vec::new();
    for (field, value) in [
        ("abiVersion", serde_json::json!(0)),
        ("abiVersion", serde_json::json!("1")),
        ("state", serde_json::json!("preedit")),
        ("capabilities", serde_json::json!(true)),
    ] {
        let mut v = source.clone();
        v[field] = value;
        invalid_source.push(serde_json::to_vec(&v).unwrap());
    }
    for key in ["session", "generation"] {
        for bad in [
            serde_json::json!(1),
            serde_json::json!("0"),
            serde_json::json!("01"),
            serde_json::json!("+1"),
            serde_json::json!(" 1"),
            serde_json::json!("1.0"),
            serde_json::json!("18446744073709551616"),
        ] {
            let mut v = source.clone();
            v["ticket"][key] = bad;
            invalid_source.push(serde_json::to_vec(&v).unwrap());
        }
    }
    for key in ["windowStage", "windowId", "editorSerial"] {
        for bad in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("1"),
            serde_json::json!(9007199254740992_u64),
            serde_json::Value::Null,
        ] {
            let mut v = source.clone();
            v["ticket"][key] = bad;
            invalid_source.push(serde_json::to_vec(&v).unwrap());
        }
    }
    let mut v = source.clone();
    v["ticket"]["extra"] = true.into();
    invalid_source.push(serde_json::to_vec(&v).unwrap());
    invalid_source.extend([
        b"{\"abiVersion\":1,\"abiVersion\":1}".to_vec(),
        b"\xff".to_vec(),
        vec![b' '; 4097],
    ]);
    for bytes in invalid_source {
        assert_eq!(
            unsafe { frd_ohos_app_ime_source(0, bytes.as_ptr(), bytes.len()) },
            INVALID_ARGUMENT
        );
    }
    let mut invalid_ops = Vec::new();
    for (field, value) in [
        ("abiVersion", serde_json::json!(2)),
        ("serial", serde_json::json!(0)),
        ("serial", serde_json::json!(9007199254740992_u64)),
        ("serial", serde_json::json!(1.0)),
        ("kind", serde_json::json!("preedit")),
        ("text", serde_json::json!("")),
        ("text", serde_json::json!(42)),
        ("text", serde_json::Value::Null),
        ("text", serde_json::json!("a".repeat(16385))),
        ("key", serde_json::json!("Enter")),
        ("key", serde_json::Value::Null),
        ("revision", serde_json::json!(1)),
    ] {
        let mut v = op.clone();
        v[field] = value;
        invalid_ops.push(serde_json::to_vec(&v).unwrap());
    }
    for bad in ["A", "Escape", "ControlLeft", "0x28", "Arrowleft", ""] {
        let mut v = op.clone();
        v.as_object_mut().unwrap().remove("text");
        v["kind"] = "keyPair".into();
        v["key"] = bad.into();
        invalid_ops.push(serde_json::to_vec(&v).unwrap());
    }
    let duplicate = serde_json::to_string(&op).unwrap().replacen(
        "\"serial\":1",
        "\"serial\":1,\"serial\":2",
        1,
    );
    invalid_ops.push(duplicate.into_bytes());
    let duplicate_text = serde_json::to_string(&op).unwrap().replacen(
        "\"text\":\"valid\"",
        "\"text\":\"valid\",\"text\":\"other\"",
        1,
    );
    invalid_ops.push(duplicate_text.into_bytes());
    invalid_ops.extend([b"\xff".to_vec(), vec![b' '; 32769]]);
    for bytes in invalid_ops {
        assert_eq!(
            unsafe { frd_ohos_app_ime_operation(0, bytes.as_ptr(), bytes.len()) },
            INVALID_ARGUMENT
        );
    }
    let source = serde_json::to_vec(&source).unwrap();
    let op = serde_json::to_vec(&op).unwrap();
    assert_eq!(
        unsafe { frd_ohos_app_ime_source(0, source.as_ptr(), source.len()) },
        INVALID_HANDLE
    );
    assert_eq!(
        unsafe { frd_ohos_app_ime_operation(0, op.as_ptr(), op.len()) },
        INVALID_HANDLE
    );
    for f in [frd_ohos_app_ime_source, frd_ohos_app_ime_operation] {
        assert_eq!(unsafe { f(0, std::ptr::null(), 1) }, INVALID_ARGUMENT);
        assert_eq!(unsafe { f(0, std::ptr::null(), 0) }, INVALID_ARGUMENT);
    }
    assert_eq!(std::mem::size_of::<AppGeometry>(), 44);
    assert_eq!(std::mem::size_of::<AppInput>(), 64);
}
#[test]
fn ime_abi_uninstalled_is_unavailable_and_malformed_stale_detach_cannot_poison_source() {
    let (engine, id, _, epoch, received) = ime_abi_fixture(true);
    let _guard = ImeAbiGuard(id);
    let ticket = serde_json::json!({"windowStage":1,"windowId":11,"editorSerial":1,"session":epoch.session.get().to_string(),"generation":"1"});
    assert_eq!(ime_abi_source(id, &ticket, "attached"), OK);
    wait_snapshot(&engine, |s| s["ime"]["state"] == "attached");
    let invalid =
        serde_json::to_vec(&serde_json::json!({"abiVersion":2,"state":"detached","ticket":ticket}))
            .unwrap();
    assert_eq!(
        unsafe { frd_ohos_app_ime_source(id, invalid.as_ptr(), invalid.len()) },
        INVALID_ARGUMENT
    );
    let mut stale = ticket.clone();
    stale["editorSerial"] = 2.into();
    assert_eq!(ime_abi_source(id, &stale, "detached"), OK);
    assert_eq!(ime_abi_operation(id, &ticket, 1, "still-current"), OK);
    loop {
        let input = received.recv_timeout(Duration::from_secs(2)).unwrap();
        if input.event != InputEvent::ReleaseAll {
            assert_eq!(
                input.event,
                InputEvent::Text {
                    utf8: "still-current".into()
                }
            );
            break;
        }
    }
    let (sender, _rx) = std::sync::mpsc::channel();
    let default = Arc::new(
        AppEngine::spawn_with_keyboard(
            isolated_abi_stores(),
            vec![Arc::new(CaptureFactory(sender))],
            Arc::new(|_| {
                Err(frd_media_api::VideoDecodeError::new(
                    frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
                ))
            }),
            Arc::new(NoAudio),
            crate::keyboard_policy::OhosKeyboardQualification::ArkUiLimited,
        )
        .unwrap(),
    );
    let default_id = {
        let mut a = apps().lock().unwrap();
        a.0 += 1;
        let id = a.0;
        a.1.insert(id, default.clone());
        id
    };
    let _guard = ImeAbiGuard(default_id);
    assert_eq!(
        ime_abi_source(default_id, &ticket, "attached"),
        UNSUPPORTED_PLATFORM
    );
    wait_snapshot(&default, |s| s["ime"]["installed"] == false);
}
// 正常 AppEngine 的存储依赖边界替身：避免本进程唯一沙盒 fd 与别的 ABI 测试根目录争用。
struct ImeAbiPins;
impl frd_platform_api::ServerIdentityStore for ImeAbiPins {
    fn load_pin(
        &self,
        _: &frd_core::ProtocolId,
        _: &frd_core::Endpoint,
    ) -> Result<Option<[u8; 32]>, frd_platform_api::PlatformError> {
        Ok(None)
    }
    fn store_pin(
        &self,
        _: &frd_core::ProtocolId,
        _: &frd_core::Endpoint,
        _: [u8; 32],
    ) -> Result<(), frd_platform_api::PlatformError> {
        Ok(())
    }
}
struct ImeAbiProfiles;
impl frd_platform_api::ConnectionProfileStore for ImeAbiProfiles {
    fn list(
        &self,
    ) -> Result<Vec<frd_platform_api::SavedConnectionProfile>, frd_platform_api::PlatformError>
    {
        Ok(vec![])
    }
    fn upsert(
        &self,
        _: &frd_platform_api::SavedConnectionProfile,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Ok(())
    }
    fn delete(
        &self,
        _: &frd_platform_api::ConnectionProfileKey,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Ok(())
    }
}
struct ImeAbiCredentials;
impl frd_platform_api::SecureCredentialStore for ImeAbiCredentials {
    fn load(
        &self,
        _: &frd_platform_api::ConnectionProfileKey,
    ) -> Result<Option<SecretBuffer>, frd_platform_api::PlatformError> {
        Ok(None)
    }
    fn stage(
        &self,
        _: frd_core::SessionId,
        _: &frd_platform_api::ConnectionProfileKey,
        _: &SecretBuffer,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }
    fn commit(
        &self,
        _: frd_core::SessionId,
        _: &frd_platform_api::ConnectionProfileKey,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }
    fn discard(&self, _: frd_core::SessionId) -> Result<(), frd_platform_api::PlatformError> {
        Ok(())
    }
    fn delete(
        &self,
        _: &frd_platform_api::ConnectionProfileKey,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Ok(())
    }
    fn purge_pending(&self) -> Result<(), frd_platform_api::PlatformError> {
        Ok(())
    }
}
fn isolated_abi_stores() -> crate::app_adapter::Stores {
    crate::app_adapter::Stores {
        context_root: None,
        identities: Arc::new(ImeAbiPins),
        profiles: Arc::new(ImeAbiProfiles),
        credentials: Arc::new(ImeAbiCredentials),
    }
}

#[test]
fn ime_fix1_source_state_accepts_strings_only() {
    let ticket = serde_json::json!({"windowStage":1,"windowId":11,"editorSerial":1,"session":"1","generation":"1"});
    for state in ["attached", "detached"] {
        assert_eq!(ime_abi_source(0, &ticket, state), INVALID_HANDLE);
    }
    for state in [
        serde_json::json!({"attached":null}),
        serde_json::json!({"detached":null}),
        serde_json::json!(true),
        serde_json::json!(1),
        serde_json::json!(null),
        serde_json::json!([]),
    ] {
        let bytes =
            serde_json::to_vec(&serde_json::json!({"abiVersion":1,"state":state,"ticket":ticket}))
                .unwrap();
        assert_eq!(
            unsafe { frd_ohos_app_ime_source(0, bytes.as_ptr(), bytes.len()) },
            INVALID_ARGUMENT
        );
    }
}
#[test]
fn ime_fix1_operation_kind_accepts_strings_only() {
    let ticket = serde_json::json!({"windowStage":1,"windowId":11,"editorSerial":1,"session":"1","generation":"1"});
    for kind in [
        serde_json::json!("text"),
        serde_json::json!({"text":null}),
        serde_json::json!(true),
        serde_json::json!(1),
        serde_json::json!(null),
        serde_json::json!([]),
    ] {
        let valid = kind == serde_json::json!("text");
        let bytes = serde_json::to_vec(&serde_json::json!({"abiVersion":1,"ticket":ticket,"serial":1,"kind":kind,"text":"synthetic"})).unwrap();
        assert_eq!(
            unsafe { frd_ohos_app_ime_operation(0, bytes.as_ptr(), bytes.len()) },
            if valid {
                INVALID_HANDLE
            } else {
                INVALID_ARGUMENT
            }
        );
    }
    let bytes = serde_json::to_vec(&serde_json::json!({"abiVersion":1,"ticket":ticket,"serial":1,"kind":{"keyPair":null},"key":"Enter"})).unwrap();
    assert_eq!(
        unsafe { frd_ohos_app_ime_operation(0, bytes.as_ptr(), bytes.len()) },
        INVALID_ARGUMENT
    );
}
#[test]
fn ime_fix1_control_key_accepts_strings_only() {
    let ticket = serde_json::json!({"windowStage":1,"windowId":11,"editorSerial":1,"session":"1","generation":"1"});
    for key in [
        serde_json::json!("Enter"),
        serde_json::json!({"Enter":null}),
        serde_json::json!(true),
        serde_json::json!(1),
        serde_json::json!(null),
        serde_json::json!([]),
    ] {
        let valid = key == serde_json::json!("Enter");
        let bytes = serde_json::to_vec(&serde_json::json!({"abiVersion":1,"ticket":ticket,"serial":1,"kind":"keyPair","key":key})).unwrap();
        assert_eq!(
            unsafe { frd_ohos_app_ime_operation(0, bytes.as_ptr(), bytes.len()) },
            if valid {
                INVALID_HANDLE
            } else {
                INVALID_ARGUMENT
            }
        );
    }
}
