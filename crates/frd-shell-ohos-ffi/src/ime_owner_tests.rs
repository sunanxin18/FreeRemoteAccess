// 在真实 Owner 和 SessionHost command 通道边界验证，不把 sent_inputs 测试账本当作发送成功。
struct ImeCaptureFactory {
    inputs: mpsc::Sender<frd_core::SessionInput>,
    text: bool,
    physical: bool,
}
impl ProtocolFactory for ImeCaptureFactory {
    fn descriptor(&self) -> frd_protocol_api::ProtocolDescriptor {
        frd_protocol_api::ProtocolDescriptor {
            id: ProtocolId::rdp(),
            display_name: "IME 合成会话".into(),
            default_port: 3389,
            credential_requirements: frd_protocol_api::CredentialRequirements::username_password(),
        }
    }
    fn create(
        &self,
        request: frd_protocol_api::ConnectRequest,
        runtime: frd_protocol_api::ProtocolRuntime,
    ) -> Result<Box<dyn frd_protocol_api::ProtocolSession>, ProtocolError> {
        Ok(Box::new(ImeCaptureSession {
            runtime,
            session: request.session_id,
            inputs: self.inputs.clone(),
            text: self.text,
            physical: self.physical,
        }))
    }
}
struct ImeCaptureSession {
    runtime: frd_protocol_api::ProtocolRuntime,
    session: frd_core::SessionId,
    inputs: mpsc::Sender<frd_core::SessionInput>,
    text: bool,
    physical: bool,
}
impl frd_protocol_api::ProtocolSession for ImeCaptureSession {
    fn run(mut self: Box<Self>) -> frd_protocol_api::ProtocolExit {
        let mut produce = || -> Result<(), ProtocolError> {
            self.runtime
                .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))?;
            self.runtime
                .publish_event(SessionEvent::CapabilitiesChanged(
                    frd_protocol_api::SessionCapabilities {
                        text_input: self.text,
                        physical_key_input: self.physical,
                        ..Default::default()
                    },
                ))?;
            self.runtime.begin_generation(
                self.session,
                1,
                frd_core::PixelSize {
                    width: 2,
                    height: 2,
                },
                frd_frame::PixelFormat::Bgrx8UnormSrgb,
            )
        };
        if let Err(e) = produce() {
            return frd_protocol_api::ProtocolExit::Failed(e);
        }
        while !self.runtime.requires_shutdown() {
            while let Some(c) = self.runtime.try_next_command() {
                match c {
                    SessionCommand::Input(input) => {
                        let _ = self.inputs.send(input);
                    }
                    SessionCommand::Disconnect => return frd_protocol_api::ProtocolExit::Closed,
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        frd_protocol_api::ProtocolExit::Closed
    }
}
fn ime_owner(
    physical: bool,
    protocol_text: bool,
) -> (
    Owner,
    AppEngine,
    mpsc::Receiver<Command>,
    mpsc::Receiver<frd_core::SessionInput>,
    Arc<crate::presentation::Link>,
    ImeTicket,
) {
    let mut o = owner();
    o.ime = ImeState::new(ImeInstallation::SystemImeV1);
    o.keyboard = if physical {
        OhosKeyboardQualification::ArkUiLimited
    } else {
        OhosKeyboardQualification::Unavailable
    };
    o.configure_ime();
    let (inputs, received) = mpsc::channel();
    o.host = SessionHost::new(
        vec![Arc::new(ImeCaptureFactory {
            inputs,
            text: protocol_text,
            physical,
        }) as Arc<dyn ProtocolFactory>],
        Arc::new(Wake),
        Arc::new(NoAudio),
        Arc::new(|_| {
            Err(frd_media_api::VideoDecodeError::new(
                frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
            ))
        }),
    );
    o.catalog = ProtocolCatalog::new([ProtocolId::rdp()]);
    let (tx, launches) = mpsc::sync_channel(64);
    o.tx = tx;
    o.command(Command::Edit(Edit {
        target: 1,
        address: "fixture.invalid".into(),
        port: 3389,
        username: "synthetic".into(),
        protocol: "rdp".into(),
        remember: false,
    }));
    o.command(Command::Password(SecretBuffer::from_text(
        "synthetic-in-memory".into(),
    )));
    o.command(Command::Submit {
        event: 1,
        composition: false,
        repeat: false,
    });
    o.command(launches.recv_timeout(Duration::from_secs(2)).unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    while o.remote_epoch.is_none() {
        o.poll();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let link = Arc::new(crate::presentation::Link::default());
    link.surface(crate::PocStatus {
        surface_generation: 1,
        width: 100,
        height: 100,
        attached: 1,
        foreground: 1,
        ..Default::default()
    });
    link.0.lock().unwrap().bound = true;
    *o.renderer.lock().unwrap() = Some(link.clone());
    o.command(Command::Geometry(ViewGeometry {
        drawable: frd_core::PixelSize {
            width: 100,
            height: 100,
        },
        bounds: frd_core::PixelRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        },
        scale: 1.0,
        pan_x: 0.0,
        pan_y: 0.0,
    }));
    o.command(Command::LocalDisplay(display_message(
        crate::local_display::sample_message(1, 1, 11, true),
    )));
    present_key_owner(&mut o, &link);
    o.focus(true, true, true);
    let ticket = ImeTicket {
        window_stage: 1,
        window_id: 11,
        editor_serial: 1,
        session: o.remote_epoch.unwrap().0.get(),
        generation: 1,
    };
    let (engine, queue) = key_engine(&o, 64);
    (o, engine, queue, received, link, ticket)
}
fn ime_source_message(t: ImeTicket, state: &str) -> ImeSource {
    ImeSource::parse(
        &serde_json::to_vec(&json!({"abiVersion":1,"state":state,"ticket":t.snapshot()})).unwrap(),
    )
    .unwrap()
}
fn ime_op(t: ImeTicket, serial: u64, text: &str) -> ImeOperation {
    ImeOperation::parse(&serde_json::to_vec(&json!({"abiVersion":1,"ticket":t.snapshot(),"serial":serial,"kind":"text","text":text})).unwrap()).unwrap()
}
fn ime_key(t: ImeTicket, serial: u64, key: &str) -> ImeOperation {
    ImeOperation::parse(&serde_json::to_vec(&json!({"abiVersion":1,"ticket":t.snapshot(),"serial":serial,"kind":"keyPair","key":key})).unwrap()).unwrap()
}
fn ime_attach(
    o: &mut Owner,
    engine: &AppEngine,
    queue: &mpsc::Receiver<Command>,
    ticket: ImeTicket,
) {
    engine
        .ime_source(ime_source_message(ticket, "attached"))
        .unwrap();
    o.command(queue.recv().unwrap());
}
fn ime_take(receiver: &mpsc::Receiver<frd_core::SessionInput>, count: usize) -> Vec<InputEvent> {
    let mut inputs = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while inputs.len() < count {
        let input = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if input.event != InputEvent::ReleaseAll {
            inputs.push(input.event);
        }
    }
    inputs
}
#[test]
fn ime_owner_same_source_rebuild_waits_full_present_and_dispatches_current_revision_once() {
    let (mut o, engine, queue, received, link, ticket) = ime_owner(true, true);
    ime_attach(&mut o, &engine, &queue, ticket);
    assert_eq!(o.snapshot()["textInput"], true);
    let old = o.input.revision;
    let mut geometry = o.input.geometry.unwrap();
    geometry.scale = 2.0;
    o.command(Command::Geometry(geometry));
    assert_eq!(o.input.revision, old + 1);
    for op in [
        ime_op(ticket, 1, "repeat中"),
        ime_key(ticket, 2, "Enter"),
        ime_op(ticket, 3, "repeat中"),
    ] {
        engine.ime_operation(op).unwrap();
        o.command(queue.recv().unwrap());
    }
    assert_eq!(o.ime.accepted, 3);
    assert_eq!(o.ime.dispatched, 0);
    assert_eq!(o.ime.pending.len(), 3);
    o.sync_presentation(false);
    let epoch = link.0.lock().unwrap().epoch().unwrap();
    feedback(&link, epoch, frd_frame::FrameCompleteness::Incremental);
    o.sync_presentation(false);
    assert_eq!(o.ime.dispatched, 0);
    feedback(&link, epoch, frd_frame::FrameCompleteness::FullBaseline);
    o.sync_presentation(false);
    assert_eq!(o.ime.dispatched, 3);
    assert_eq!(
        ime_take(&received, 4),
        vec![
            InputEvent::Text {
                utf8: "repeat中".into()
            },
            key_event(0x28, true),
            key_event(0x28, false),
            InputEvent::Text {
                utf8: "repeat中".into()
            }
        ]
    );
    o.sync_presentation(false);
    assert!(received.recv_timeout(Duration::from_millis(20)).is_err());
    let value = o.snapshot().to_string();
    assert!(
        !value.contains("repeat中")
            && !value.contains("pending_bytes")
            && !value.contains("textLength")
    );
    assert_eq!(o.snapshot()["ime"]["lastDispatchedSerial"], 3);
    let snapshot = o.snapshot();
    if let Some(log) = o.take_display_log(&snapshot) {
        assert!(!log.to_string().contains("repeat中"));
    }
}
#[test]
fn ime_owner_text_only_installation_preserves_capability_intersections_and_legacy_gate() {
    let (mut o, engine, queue, received, _, ticket) = ime_owner(false, true);
    assert_eq!(o.snapshot()["textInput"], false);
    ime_attach(&mut o, &engine, &queue, ticket);
    assert_eq!(o.snapshot()["physicalKeyInput"], false);
    engine
        .ime_operation(ime_op(ticket, 1, "text-only"))
        .unwrap();
    o.command(queue.recv().unwrap());
    assert_eq!(
        ime_take(&received, 1),
        vec![InputEvent::Text {
            utf8: "text-only".into()
        }]
    );
    assert_eq!(
        engine.ime_operation(ime_key(ticket, 2, "Tab")),
        Err(crate::UNSUPPORTED_PLATFORM)
    );
    let legacy = || {
        Command::Input(
            InputEpoch {
                session: ticket.session,
                generation: 1,
                revision: o.input.revision,
            },
            InputEvent::Text {
                utf8: "legacy-denied".into(),
            },
            false,
        )
    };
    assert_eq!(engine.send(legacy()), Err(crate::UNSUPPORTED_PLATFORM));
    o.command(legacy());
    assert!(received.recv_timeout(Duration::from_millis(20)).is_err());
    let (blocked, e, q, _, _, t) = ime_owner(true, false);
    assert_eq!(
        e.ime_source(ime_source_message(t, "attached")),
        Err(crate::INVALID_ARGUMENT)
    );
    assert!(q.try_recv().is_err());
    assert!(blocked.ime.ticket.is_none());
    assert!(!blocked.controller.negotiated_capabilities().text_input);
    assert_eq!(blocked.snapshot()["textInput"], false);
}
#[test]
fn ime_owner_serial_gap_duplicate_and_old_ticket_cannot_replay_or_reopen() {
    for rejected in [1, 3] {
        let (mut o, e, q, received, _, t) = ime_owner(true, true);
        ime_attach(&mut o, &e, &q, t);
        e.ime_operation(ime_op(t, 1, "same")).unwrap();
        o.command(q.recv().unwrap());
        assert_eq!(
            ime_take(&received, 1),
            vec![InputEvent::Text {
                utf8: "same".into()
            }]
        );
        e.ime_operation(ime_op(t, rejected, "never")).unwrap();
        o.command(q.recv().unwrap());
        assert_eq!(o.ime.reason, "invalid_serial");
        assert!(o.ime.ticket.is_none());
        assert_eq!(
            e.ime_operation(ime_op(t, 2, "late")),
            Err(crate::INVALID_ARGUMENT)
        );
        assert_eq!(
            e.ime_source(ime_source_message(t, "attached")),
            Err(crate::INVALID_ARGUMENT)
        );
        let next = ImeTicket {
            editor_serial: 2,
            ..t
        };
        ime_attach(&mut o, &e, &q, next);
        e.ime_source(ime_source_message(t, "detached")).unwrap();
        assert!(q.try_recv().is_err());
        assert_eq!(o.ime.ticket, Some(next));
        while let Ok(input) = received.recv_timeout(Duration::from_millis(20)) {
            assert_eq!(input.event, InputEvent::ReleaseAll);
        }
    }
}
#[test]
fn ime_owner_queue_full_detach_barrier_discards_old_attach_and_recovers_fresh_editor() {
    for detach in [false, true] {
        let (mut o, _original_engine, _original_queue, received, _, t) = ime_owner(true, true);
        let (e, q) = key_engine(&o, 64);
        e.ime_source(ime_source_message(t, "attached")).unwrap();
        for _ in 0..63 {
            e.send(Command::Focus {
                foreground: true,
                focused: true,
                remote: true,
            })
            .unwrap();
        }
        let result = if detach {
            e.ime_source(ime_source_message(t, "detached"))
        } else {
            e.ime_operation(ime_op(t, 1, "overflow"))
        };
        assert_eq!(result, Err(crate::QUEUE_FULL));
        o.command(q.recv().unwrap());
        for _ in 0..63 {
            o.command(q.recv().unwrap());
        }
        assert!(o.ime.ticket.is_none());
        assert!(
            o.input.owns_remote_focus(),
            "IME 屏障不能撤销实体键/指针焦点"
        );
        let fresh = ImeTicket {
            editor_serial: 2,
            ..t
        };
        ime_attach(&mut o, &e, &q, fresh);
        e.ime_operation(ime_op(fresh, 1, "fresh")).unwrap();
        o.command(q.recv().unwrap());
        assert_eq!(
            ime_take(&received, 1),
            vec![InputEvent::Text {
                utf8: "fresh".into()
            }]
        );
    }
}
#[test]
fn ime_owner_real_lifecycle_invalidation_wipes_pending_and_tombstones_identity() {
    for invalidation in 0..8 {
        let (mut o, e, q, received, link, t) = ime_owner(true, true);
        ime_attach(&mut o, &e, &q, t);
        let mut geometry = o.input.geometry.unwrap();
        geometry.scale = 2.0;
        o.command(Command::Geometry(geometry));
        e.ime_operation(ime_op(t, 1, "pending-never-dispatched"))
            .unwrap();
        o.command(q.recv().unwrap());
        match invalidation {
            0 => o.focus(false, false, false),
            1 => {
                e.ime_source(ime_source_message(t, "detached")).unwrap();
                o.command(q.recv().unwrap());
            }
            2 => o.clear_remote(),
            3 => o.command(Command::LocalDisplay(display_message(
                crate::local_display::sample_message(2, 2, 12, true),
            ))),
            4 => o.runtime_event(
                o.remote_epoch.unwrap().0,
                SessionEvent::CapabilitiesChanged(Default::default()),
            ),
            5 => {
                e.cancel();
                o.apply_control();
            }
            6 => {
                let mut state = link.0.lock().unwrap();
                state.surface.foreground = false;
                drop(state);
                o.sync_presentation(false);
            }
            _ => o.runtime_event(
                o.remote_epoch.unwrap().0,
                SessionEvent::SurfaceGenerationChanged {
                    session_id: o.remote_epoch.unwrap().0,
                    generation: 2,
                    size: frd_core::PixelSize {
                        width: 2,
                        height: 2,
                    },
                },
            ),
        }
        assert!(o.ime.ticket.is_none(), "case {invalidation}");
        assert!(o.ime.pending.is_empty());
        assert_eq!(o.ime.pending_bytes, 0);
        assert_eq!(
            e.ime_source(ime_source_message(t, "attached")),
            Err(crate::INVALID_ARGUMENT)
        );
        assert_eq!(
            e.ime_operation(ime_op(t, 2, "late")),
            Err(crate::INVALID_ARGUMENT)
        );
        if invalidation != 5 {
            o.sync_presentation(false);
        }
        let _ = link;
        assert!(received
            .try_iter()
            .all(|i| i.event == InputEvent::ReleaseAll));
    }
}
#[test]
fn ime_owner_wrong_window_epoch_or_focus_never_attaches_and_old_caps_cannot_raise_protocol() {
    for field in 0..5 {
        let (mut o, e, q, _, _, mut t) = ime_owner(true, true);
        match field {
            0 => t.window_stage += 1,
            1 => t.window_id += 1,
            2 => t.session += 1,
            3 => t.generation += 1,
            _ => o.focus(true, true, false),
        }
        if field == 2 || field == 3 {
            ime_attach(&mut o, &e, &q, t);
        } else {
            assert_eq!(
                e.ime_source(ime_source_message(t, "attached")),
                Err(crate::INVALID_ARGUMENT)
            );
        }
        assert!(o.ime.ticket.is_none());
        assert_eq!(
            e.ime_source(ime_source_message(t, "attached")),
            Err(crate::INVALID_ARGUMENT)
        );
    }
    let (mut active, ae, aq, _, _, at) = ime_owner(true, true);
    ime_attach(&mut active, &ae, &aq, at);
    active.runtime_event(
        frd_core::SessionId::allocate(),
        SessionEvent::CapabilitiesChanged(Default::default()),
    );
    assert!(active.controller.negotiated_capabilities().text_input);
    assert_eq!(active.ime.ticket, Some(at));
    let (mut o, e, q, _, _, t) = ime_owner(true, false);
    o.runtime_event(
        frd_core::SessionId::allocate(),
        SessionEvent::CapabilitiesChanged(frd_protocol_api::SessionCapabilities {
            text_input: true,
            ..Default::default()
        }),
    );
    assert!(!o.controller.negotiated_capabilities().text_input);
    assert_eq!(
        e.ime_source(ime_source_message(t, "attached")),
        Err(crate::INVALID_ARGUMENT)
    );
    assert!(q.try_recv().is_err());
    assert!(o.ime.ticket.is_none());
}
#[test]
fn ime_owner_pending_is_bounded_by_count_and_bytes_and_command_failure_has_no_ack() {
    for bytes_limit in [false, true] {
        let (mut o, e, q, _, _, t) = ime_owner(true, true);
        ime_attach(&mut o, &e, &q, t);
        let mut geometry = o.input.geometry.unwrap();
        geometry.scale = 2.0;
        o.command(Command::Geometry(geometry));
        let text = if bytes_limit {
            "z".repeat(8192)
        } else {
            "z".into()
        };
        let count = if bytes_limit { 3 } else { 33 };
        for serial in 1..=count {
            e.ime_operation(ime_op(t, serial, &text)).unwrap();
            o.command(q.recv().unwrap());
        }
        assert!(o.ime.ticket.is_none());
        assert!(o.ime.pending.is_empty());
        assert_eq!(o.ime.reason, "pending_full");
    }
    for key in [false, true] {
        let (mut o, e, q, _, _, t) = ime_owner(true, true);
        ime_attach(&mut o, &e, &q, t);
        o.host.attach_frame_test_mailbox(
            o.remote_epoch.unwrap().0,
            Arc::new(Mutex::new(frd_frame::FrameMailbox::new(8, 4096))),
        );
        e.ime_operation(if key {
            ime_key(t, 1, "Enter")
        } else {
            ime_op(t, 1, "not-acknowledged")
        })
        .unwrap();
        o.command(q.recv().unwrap());
        assert_eq!(o.ime.accepted, 1);
        assert_eq!(o.ime.dispatched, 0);
        assert_eq!(o.ime.reason, "dispatch_failed");
        assert!(o.ime.pending.is_empty());
    }
}
#[test]
fn ime_owner_queued_detach_attach_and_revision_exhaustion_cannot_retag_old_callbacks() {
    let (mut o, e, q, received, _, t) = ime_owner(true, true);
    ime_attach(&mut o, &e, &q, t);
    e.ime_operation(ime_op(t, 1, "stale-queued")).unwrap();
    e.ime_source(ime_source_message(t, "detached")).unwrap();
    let fresh = ImeTicket {
        editor_serial: 2,
        ..t
    };
    e.ime_source(ime_source_message(fresh, "attached")).unwrap();
    for _ in 0..3 {
        o.command(q.recv().unwrap());
    }
    assert_eq!(o.ime.ticket, Some(fresh));
    assert_eq!(o.ime.accepted, 0);
    e.ime_source(ime_source_message(t, "detached")).unwrap();
    e.ime_operation(ime_op(fresh, 1, "current")).unwrap();
    o.command(q.recv().unwrap());
    assert_eq!(
        ime_take(&received, 1),
        vec![InputEvent::Text {
            utf8: "current".into()
        }]
    );
    {
        let mut ingress = o.ime_ingress.lock().unwrap();
        ingress.revision = u64::MAX - 1;
    }
    e.ime_source(ime_source_message(fresh, "detached")).unwrap();
    o.command(q.recv().unwrap());
    assert!(o.ime.ticket.is_none());
    assert_eq!(
        e.ime_source(ime_source_message(
            ImeTicket {
                editor_serial: 3,
                ..fresh
            },
            "attached"
        )),
        Err(crate::EXHAUSTED)
    );
}
#[test]
fn ime_owner_control_pair_never_steals_held_key_or_synthesizes_modifiers() {
    for hid in [0x28, 0xe0, 0xe1, 0xe2] {
        for before_attach in [false, true] {
            let (mut o, e, q, received, _, t) = ime_owner(true, true);
            let epoch = InputEpoch {
                session: t.session,
                generation: 1,
                revision: o.input.revision,
            };
            if before_attach {
                o.command(key_command(epoch, hid, true));
                assert_eq!(ime_take(&received, 1), vec![key_event(hid, true)]);
            }
            ime_attach(&mut o, &e, &q, t);
            if !before_attach {
                o.command(key_command(epoch, hid, true));
                assert_eq!(ime_take(&received, 1), vec![key_event(hid, true)]);
            }
            e.ime_operation(ime_key(t, 1, "Enter")).unwrap();
            o.command(q.recv().unwrap());
            assert!(o.ime.ticket.is_none());
            assert_eq!(o.ime.dispatched, 0);
            assert_eq!(o.ime.reason, "physical_source_busy");
            o.command(key_command(epoch, hid, false));
            if !before_attach {
                assert_eq!(ime_take(&received, 1), vec![key_event(hid, false)]);
            }
            assert!(received.try_iter().all(|input| matches!(
                input.event,
                InputEvent::ReleaseAll
                    | InputEvent::PhysicalKey {
                        state: frd_core::KeyState::Released,
                        ..
                    }
            )));
            let fresh = ImeTicket {
                editor_serial: 2,
                ..t
            };
            ime_attach(&mut o, &e, &q, fresh);
            for (serial, name, hid) in [
                (1, "Enter", 0x28),
                (2, "Tab", 0x2b),
                (3, "Backspace", 0x2a),
                (4, "Delete", 0x4c),
                (5, "ArrowLeft", 0x50),
                (6, "ArrowRight", 0x4f),
                (7, "ArrowUp", 0x52),
                (8, "ArrowDown", 0x51),
                (9, "Home", 0x4a),
                (10, "End", 0x4d),
                (11, "PageUp", 0x4b),
                (12, "PageDown", 0x4e),
            ] {
                e.ime_operation(ime_key(fresh, serial, name)).unwrap();
                o.command(q.recv().unwrap());
                assert_eq!(
                    ime_take(&received, 2),
                    vec![key_event(hid, true), key_event(hid, false)]
                );
            }
        }
    }
}
#[test]
fn ime_owner_authoritative_window_slot_rejects_forgery_without_poisoning_current_source() {
    let (mut o, e, q, received, _, t) = ime_owner(true, true);
    ime_attach(&mut o, &e, &q, t);
    for identity in 2..=1001 {
        let forged = ImeTicket {
            window_stage: identity,
            window_id: identity + 100,
            editor_serial: identity,
            ..t
        };
        assert_eq!(
            e.ime_source(ime_source_message(forged, "attached")),
            Err(crate::INVALID_ARGUMENT)
        );
        assert_eq!(o.ime.ticket, Some(t));
    }
    assert!(q.try_recv().is_err());
    e.ime_operation(ime_op(t, 1, "still-authoritative"))
        .unwrap();
    o.command(q.recv().unwrap());
    assert_eq!(
        ime_take(&received, 1),
        vec![InputEvent::Text {
            utf8: "still-authoritative".into()
        }]
    );
}
#[test]
fn ime_owner_single_window_tombstone_survives_background_and_unavailable_then_new_stage_resets() {
    let (mut o, e, q, received, _, t) = ime_owner(true, true);
    ime_attach(&mut o, &e, &q, t);
    o.focus(false, false, false);
    assert_eq!(
        e.ime_source(ime_source_message(
            ImeTicket {
                editor_serial: 2,
                ..t
            },
            "attached"
        )),
        Err(crate::INVALID_ARGUMENT)
    );
    o.focus(true, true, true);
    assert_eq!(
        e.ime_source(ime_source_message(t, "attached")),
        Err(crate::INVALID_ARGUMENT)
    );
    let fresh = ImeTicket {
        editor_serial: 2,
        ..t
    };
    ime_attach(&mut o, &e, &q, fresh);
    o.command(Command::LocalDisplay(display_message(
        json!({"abiVersion":1,"stage":1,"revision":2,
        "windowId":11,"phone":true,"geometry":null,"reason":"window_destroyed"}),
    )));
    assert_eq!(
        e.ime_source(ime_source_message(
            ImeTicket {
                editor_serial: 3,
                ..t
            },
            "attached"
        )),
        Err(crate::INVALID_ARGUMENT)
    );
    o.command(Command::LocalDisplay(display_message(
        crate::local_display::sample_message(1, 3, 11, true),
    )));
    assert_eq!(
        e.ime_source(ime_source_message(fresh, "attached")),
        Err(crate::INVALID_ARGUMENT)
    );
    o.command(Command::LocalDisplay(display_message(
        crate::local_display::sample_message(2, 4, 12, true),
    )));
    let next = ImeTicket {
        window_stage: 2,
        window_id: 12,
        editor_serial: 1,
        ..t
    };
    ime_attach(&mut o, &e, &q, next);
    assert_eq!(
        e.ime_source(ime_source_message(
            ImeTicket {
                editor_serial: 1000,
                ..fresh
            },
            "attached"
        )),
        Err(crate::INVALID_ARGUMENT)
    );
    assert_eq!(o.ime.ticket, Some(next));
    e.ime_operation(ime_op(next, 1, "new-window")).unwrap();
    o.command(q.recv().unwrap());
    assert_eq!(
        ime_take(&received, 1),
        vec![InputEvent::Text {
            utf8: "new-window".into()
        }]
    );
}
#[test]
fn ime_owner_public_ordinary_sender_cannot_bypass_ticket_ingress() {
    let (mut o, e, q, _, _, t) = ime_owner(true, true);
    assert_eq!(
        e.send(Command::ImeSource(0, ime_source_message(t, "attached"))),
        Err(crate::INVALID_ARGUMENT)
    );
    assert_eq!(
        e.send(Command::ImeOperation(0, ime_op(t, 1, "unticketed-bypass"))),
        Err(crate::INVALID_ARGUMENT)
    );
    assert!(q.try_recv().is_err());
    assert!(o.ime.ticket.is_none());
    ime_attach(&mut o, &e, &q, t);
    assert_eq!(o.ime.ticket, Some(t));
}

#[test]
fn ime_fix1_display_queue_overflow_published_before_admission_cannot_dispatch() {
    for key_pair in [false, true] {
        let (mut o, engine, queue, received, link, ticket) = ime_owner(true, true);
        let engine = Arc::new(engine);
        ime_attach(&mut o, &engine, &queue, ticket);
        let mut geometry = o.input.geometry.unwrap();
        geometry.scale = 2.0;
        o.command(Command::Geometry(geometry));
        engine
            .ime_operation(if key_pair {
                ime_key(ticket, 1, "Enter")
            } else {
                ime_op(ticket, 1, "must-not-cross-window-barrier")
            })
            .unwrap();
        o.command(queue.recv().unwrap());
        assert_eq!(o.ime.pending.len(), 1);
        for _ in 0..64 {
            engine
                .send(Command::Focus {
                    foreground: true,
                    focused: true,
                    remote: true,
                })
                .unwrap();
        }
        let producer = engine.clone();
        o.ime_admission_hook = Some(Box::new(move || {
            assert_eq!(
                producer.send(Command::LocalDisplay(display_message(
                    crate::local_display::sample_message(1, 2, 11, true)
                ))),
                Err(crate::QUEUE_FULL)
            );
            assert_ne!(
                producer.control.load(Ordering::Acquire) & DISPLAY_INVALID,
                0
            );
        }));
        present_key_owner(&mut o, &link);
        assert!(o.ime_admission_hook.is_none());
        assert_eq!(
            o.ime.dispatched, 0,
            "已发布 DISPLAY_INVALID 的 admission 必须拒绝"
        );
        assert!(o.ime.pending.is_empty());
        assert!(o.ime.ticket.is_none());
        assert_eq!(o.ime.pending_bytes, 0);
        assert!(o.input.owns_remote_focus(), "显示失效不能伪造物理焦点丢失");
        assert_eq!(
            engine.ime_source(ime_source_message(ticket, "attached")),
            Err(crate::INVALID_ARGUMENT)
        );
        while let Ok(input) = received.recv_timeout(Duration::from_millis(20)) {
            assert_eq!(input.event, InputEvent::ReleaseAll);
        }
    }
}

#[test]
fn ime_fix1_rejected_generation_events_preserve_current_source_and_pending() {
    for rejected in 0..3 {
        let (mut o, engine, queue, received, link, ticket) = ime_owner(true, true);
        ime_attach(&mut o, &engine, &queue, ticket);
        let mut geometry = o.input.geometry.unwrap();
        geometry.scale = 2.0;
        o.command(Command::Geometry(geometry));
        engine
            .ime_operation(ime_op(ticket, 1, "preserved-after-stale-event"))
            .unwrap();
        o.command(queue.recv().unwrap());
        let current = o.remote_epoch.unwrap();
        let revision = o.ime.revision;
        let (inner_session, generation) = match rejected {
            0 => (current.0, current.1),
            1 => (current.0, current.1 - 1),
            _ => (frd_core::SessionId::allocate(), current.1 + 1),
        };
        o.runtime_event(
            current.0,
            SessionEvent::SurfaceGenerationChanged {
                session_id: inner_session,
                generation,
                size: frd_core::PixelSize {
                    width: 2,
                    height: 2,
                },
            },
        );
        assert_eq!(o.remote_epoch, Some(current));
        assert_eq!(o.controller.interactive_input_epoch(), Some(current));
        assert_eq!(
            o.ime.ticket,
            Some(ticket),
            "拒绝的 generation 不能消耗当前来源 case={rejected}"
        );
        assert_eq!(o.ime.revision, revision);
        assert_eq!(o.ime.pending.len(), 1);
        assert_eq!(o.ime.accepted, 1);
        assert_eq!(o.ime.dispatched, 0);
        present_key_owner(&mut o, &link);
        assert_eq!(
            ime_take(&received, 1),
            vec![InputEvent::Text {
                utf8: "preserved-after-stale-event".into()
            }]
        );
        assert_eq!(o.ime.dispatched, 1);
        o.runtime_event(
            current.0,
            SessionEvent::SurfaceGenerationChanged {
                session_id: current.0,
                generation: current.1 + 1,
                size: frd_core::PixelSize {
                    width: 2,
                    height: 2,
                },
            },
        );
        assert!(o.ime.ticket.is_none());
        assert!(o.ime.pending.is_empty());
        assert_eq!(
            engine.ime_source(ime_source_message(ticket, "attached")),
            Err(crate::INVALID_ARGUMENT)
        );
    }
}
