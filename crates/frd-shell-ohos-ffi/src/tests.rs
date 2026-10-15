use super::*;

#[test]
fn invalid_handles_are_recognizable_and_never_dereferenced() {
    assert_eq!(frd_ohos_destroy(0), INVALID_HANDLE);
    assert_eq!(frd_ohos_resize(u64::MAX, 1, 10, 10), INVALID_HANDLE);
    assert_eq!(frd_ohos_detach(0, 1), INVALID_HANDLE);
}

#[test]
fn engine_ids_are_not_reused_after_destroy() {
    let a = frd_ohos_create();
    assert_ne!(a, 0);
    assert_eq!(frd_ohos_destroy(a), OK);
    assert_eq!(frd_ohos_destroy(a), INVALID_HANDLE);
    let b = frd_ohos_create();
    assert!(b > a);
    assert_eq!(frd_ohos_destroy(b), OK);
}

#[test]
fn null_window_and_zero_extent_are_rejected_before_platform_access() {
    let engine = frd_ohos_create();
    let mut generation = 42;
    assert_eq!(
        unsafe { frd_ohos_attach(engine, std::ptr::null_mut(), 10, 10, std::ptr::null_mut()) },
        INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { frd_ohos_poc_status(engine, std::ptr::null_mut()) },
        INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { frd_ohos_attach(engine, std::ptr::null_mut(), 10, 10, &mut generation) },
        INVALID_ARGUMENT
    );
    assert_eq!(generation, 0);
    assert_eq!(
        unsafe { frd_ohos_attach(engine, std::ptr::dangling_mut(), 0, 10, &mut generation) },
        INVALID_ARGUMENT
    );
    assert_eq!(generation, 0);
    assert_eq!(frd_ohos_destroy(engine), OK);
}

use std::sync::{Arc, Mutex};
use worker::{Backend, Command, Engine};
struct Probe {
    events: Arc<Mutex<Vec<&'static str>>>,
    panic_attach: bool,
}
impl Backend for Probe {
    fn attach(&mut self, _: usize, _: u32, _: u32) -> Result<(), i32> {
        self.events.lock().unwrap().push("ref");
        if self.panic_attach {
            panic!("graphics probe failure")
        };
        Ok(())
    }
    fn resize(&mut self, _: u32, _: u32) -> Result<(), i32> {
        self.events.lock().unwrap().push("resize");
        Ok(())
    }
    fn pause(&mut self) {
        self.events.lock().unwrap().push("pause");
    }
    fn render_poc(&mut self) -> Result<bool, i32> {
        self.events.lock().unwrap().push("present");
        Ok(true)
    }
    fn detach(&mut self) {
        self.events.lock().unwrap().push("surface-drop-unref");
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.events.lock().unwrap().push("backend-drop");
    }
}
fn probe(panic_attach: bool) -> (Engine, Arc<Mutex<Vec<&'static str>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let owned = events.clone();
    (
        Engine::spawn(move || {
            Box::new(Probe {
                events: owned,
                panic_attach,
            })
        })
        .unwrap(),
        events,
    )
}
#[test]
fn old_generation_cannot_resize_detach_or_render_new_surface() {
    let (engine, events) = probe(false);
    let first = engine
        .call(Command::Attach {
            window: 1,
            width: 10,
            height: 10,
        })
        .unwrap();
    let second = engine
        .call(Command::Attach {
            window: 2,
            width: 20,
            height: 20,
        })
        .unwrap();
    assert!(second > first);
    assert_eq!(engine.call(Command::Detach(first)), Err(STALE_GENERATION));
    assert_eq!(
        engine.call(Command::Resize {
            generation: first,
            width: 30,
            height: 30
        }),
        Err(STALE_GENERATION)
    );
    assert_eq!(engine.call(Command::Tick(first)), Err(STALE_GENERATION));
    assert_eq!(engine.status().unwrap().width, 20);
    assert_eq!(engine.call(Command::Tick(second)), Ok(second));
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == "present")
            .count(),
        1
    );
    engine.shutdown().unwrap();
}
#[test]
fn detach_is_idempotent_and_destroys_surface_before_backend() {
    let (engine, events) = probe(false);
    let generation = engine
        .call(Command::Attach {
            window: 1,
            width: 10,
            height: 10,
        })
        .unwrap();
    engine.call(Command::Detach(generation)).unwrap();
    engine.call(Command::Detach(generation)).unwrap();
    engine.shutdown().unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        vec!["ref", "pause", "surface-drop-unref", "backend-drop"]
    );
}
#[test]
fn background_and_zero_extent_suppress_present_until_restored() {
    let (engine, events) = probe(false);
    let g = engine
        .call(Command::Attach {
            window: 1,
            width: 10,
            height: 10,
        })
        .unwrap();
    engine.call(Command::Foreground(false)).unwrap();
    engine.call(Command::Tick(g)).unwrap();
    engine.call(Command::Foreground(true)).unwrap();
    engine
        .call(Command::Resize {
            generation: g,
            width: 0,
            height: 0,
        })
        .unwrap();
    engine.call(Command::Tick(g)).unwrap();
    assert!(!events.lock().unwrap().contains(&"present"));
    engine
        .call(Command::Resize {
            generation: g,
            width: 20,
            height: 10,
        })
        .unwrap();
    engine.call(Command::Tick(g)).unwrap();
    assert_eq!(engine.status().unwrap().presented_frames, 1);
    engine.shutdown().unwrap();
}
#[test]
fn attach_panic_ack_waits_for_worker_cleanup_and_identifies_panic() {
    let (engine, events) = probe(true);
    assert_eq!(
        engine.call(Command::Attach {
            window: 1,
            width: 10,
            height: 10
        }),
        Err(PANIC)
    );
    assert_eq!(events.lock().unwrap().last(), Some(&"backend-drop"));
}

#[test]
fn status_pod_layout_matches_version_one_c_header() {
    assert_eq!(std::mem::size_of::<PocStatus>(), 64);
    assert_eq!(std::mem::offset_of!(PocStatus, width), 40);
    assert_eq!(std::mem::offset_of!(PocStatus, last_error), 56);
}

#[test]
fn attach_borrow_waits_for_ack_without_blocking_another_engine_registry() {
    use std::sync::mpsc;
    use std::time::Duration;
    struct SlowAttach {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl Backend for SlowAttach {
        fn attach(&mut self, _: usize, _: u32, _: u32) -> Result<(), i32> {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(())
        }
        fn resize(&mut self, _: u32, _: u32) -> Result<(), i32> {
            Ok(())
        }
        fn pause(&mut self) {}
        fn render_poc(&mut self) -> Result<bool, i32> {
            Ok(true)
        }
        fn detach(&mut self) {}
    }
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let engine = Arc::new(
        Engine::spawn(move || {
            Box::new(SlowAttach {
                entered: entered_tx,
                release: release_rx,
            })
        })
        .unwrap(),
    );
    let id = {
        let mut registry = registry().lock().unwrap();
        registry.next += 1;
        let id = registry.next;
        registry.engines.insert(id, engine);
        id
    };
    let (finished_tx, finished_rx) = mpsc::channel();
    let attach = std::thread::spawn(move || {
        let mut generation = 0;
        let code =
            unsafe { frd_ohos_attach(id, std::ptr::dangling_mut(), 10, 10, &mut generation) };
        finished_tx.send((code, generation)).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        matches!(finished_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "borrow cannot return while acquire is pending"
    );
    let (independent_tx, independent_rx) = mpsc::channel();
    let independent = std::thread::spawn(move || {
        let other = frd_ohos_create();
        independent_tx.send(frd_ohos_destroy(other)).unwrap();
    });
    let outcome = independent_rx.recv_timeout(Duration::from_secs(2));
    // 失败时也放行已借用的 attach，避免测试把借用悬挂在后台。
    release_tx.send(()).unwrap();
    assert_eq!(outcome.unwrap(), OK);
    let (code, generation) = finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(code, OK);
    assert_eq!(generation, 1);
    attach.join().unwrap();
    independent.join().unwrap();
    assert_eq!(frd_ohos_destroy(id), OK);
}

#[test]
fn skipped_surface_acquisition_is_not_counted_as_presented() {
    struct Skipped;
    impl Backend for Skipped {
        fn attach(&mut self, _: usize, _: u32, _: u32) -> Result<(), i32> {
            Ok(())
        }
        fn resize(&mut self, _: u32, _: u32) -> Result<(), i32> {
            Ok(())
        }
        fn pause(&mut self) {}
        fn render_poc(&mut self) -> Result<bool, i32> {
            Ok(false)
        }
        fn detach(&mut self) {}
    }
    let engine = Engine::spawn(|| Box::new(Skipped)).unwrap();
    let generation = engine
        .call(Command::Attach {
            window: 1,
            width: 10,
            height: 10,
        })
        .unwrap();
    engine.call(Command::Tick(generation)).unwrap();
    assert_eq!(engine.status().unwrap().presented_frames, 0);
    engine.shutdown().unwrap();
}

#[test]
#[cfg(not(debug_assertions))]
fn release_abi_rejects_graphical_poc_ticks() {
    let engine = frd_ohos_create();
    assert_eq!(frd_ohos_poc_tick(engine, 1), UNSUPPORTED_PLATFORM);
    assert_eq!(frd_ohos_destroy(engine), OK);
}

#[test]
fn controlled_panics_are_redacted_in_an_independent_process() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "tests::panic_redaction_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FRD_OHOS_PANIC_PROBE", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "子进程失败：stdout={stdout} stderr={stderr}"
    );
    assert!(!stdout.contains("FRD_PANIC_SECRET_SENTINEL"));
    assert!(
        !stderr.contains("FRD_PANIC_SECRET_SENTINEL"),
        "受控 panic 不得输出原始 payload"
    );
    assert!(stdout.contains("abi=-8 worker=-8 cleanup=true"));
    assert!(
        stderr.contains("FRD_UNSCOPED_PANIC_SENTINEL"),
        "非桥范围仍须使用先前 hook"
    );
}

#[test]
#[ignore = "仅由 panic 脱敏回归的独立子进程执行"]
fn panic_redaction_child() {
    assert_eq!(std::env::var("FRD_OHOS_PANIC_PROBE").as_deref(), Ok("1"));
    std::panic::set_hook(Box::new(|info| {
        if let Some(payload) = info.payload().downcast_ref::<&str>() {
            eprintln!("previous-hook: {payload}");
        }
    }));
    let abi = boundary(|| {
        assert_eq!(
            boundary(|| panic!("FRD_PANIC_SECRET_SENTINEL_NESTED")),
            PANIC
        );
        panic!("FRD_PANIC_SECRET_SENTINEL_ABI")
    });
    struct PayloadProbe(Arc<std::sync::atomic::AtomicBool>);
    impl Backend for PayloadProbe {
        fn attach(&mut self, _: usize, _: u32, _: u32) -> Result<(), i32> {
            panic!("FRD_PANIC_SECRET_SENTINEL_WORKER");
        }
        fn resize(&mut self, _: u32, _: u32) -> Result<(), i32> {
            Ok(())
        }
        fn pause(&mut self) {}
        fn render_poc(&mut self) -> Result<bool, i32> {
            Ok(false)
        }
        fn detach(&mut self) {}
    }
    impl Drop for PayloadProbe {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let cleanup = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = cleanup.clone();
    let engine = Arc::new(Engine::spawn(move || Box::new(PayloadProbe(observed))).unwrap());
    let id = {
        let mut registry = registry().lock().unwrap();
        registry.next += 1;
        let id = registry.next;
        registry.engines.insert(id, engine);
        id
    };
    let mut generation = 42;
    let worker = unsafe { frd_ohos_attach(id, std::ptr::dangling_mut(), 10, 10, &mut generation) };
    assert_eq!(generation, 0);
    let cleaned = cleanup.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!((abi, worker, cleaned), (PANIC, PANIC, true));
    assert_eq!(frd_ohos_destroy(id), WORKER_STOPPED);
    let factory = Engine::spawn(|| panic!("FRD_PANIC_SECRET_SENTINEL_FACTORY")).unwrap();
    assert_eq!(factory.call(Command::Foreground(true)), Err(WORKER_STOPPED));
    println!("abi={abi} worker={worker} cleanup={cleaned}");
    let _ = std::panic::catch_unwind(|| panic!("FRD_UNSCOPED_PANIC_SENTINEL"));
}

#[test]
fn full_remote_feedback_slot_cannot_block_resize_detach_or_shutdown_ack() {
    let (engine, _) = probe(false);
    let generation = engine
        .call(Command::Attach {
            window: 1,
            width: 10,
            height: 10,
        })
        .unwrap();
    {
        let mut link = engine.link.0.lock().unwrap();
        link.bound = true;
        let session = frd_core::SessionId::allocate();
        link.session = Some((session, 1));
        link.feedback = Some(crate::presentation::Feedback {
            epoch: crate::presentation::Epoch {
                session,
                remote: 1,
                local: generation,
                lifecycle: 1,
                geometry: 1,
            },
            event: None,
        });
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let join = std::thread::spawn(move || {
        engine
            .call(Command::Resize {
                generation,
                width: 20,
                height: 20,
            })
            .unwrap();
        engine.call(Command::Foreground(false)).unwrap();
        engine.call(Command::Detach(generation)).unwrap();
        engine.shutdown().unwrap();
        tx.send(()).unwrap();
    });
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    join.join().unwrap();
}

fn register_query_engine(engine: Arc<Engine>) -> u64 {
    let mut registry = registry().lock().unwrap();
    registry.next = registry.next.checked_add(1).unwrap();
    let id = registry.next;
    registry.engines.insert(id, engine);
    id
}

fn query_link_snapshot(engine: &Engine) -> impl std::fmt::Debug + PartialEq {
    let s = engine.link.0.lock().unwrap();
    (
        (
            s.surface.local,
            s.surface.lifecycle,
            s.surface.width,
            s.surface.height,
            s.surface.attached,
            s.surface.foreground,
        ),
        (
            s.bound,
            s.session,
            s.geometry,
            s.geometry_revision,
            s.remote_size,
        ),
        (
            s.batch.is_some(),
            s.feedback.is_some(),
            s.busy,
            s.failure,
            s.applied,
            s.presented,
        ),
    )
}

#[test]
fn window_surface_id_abi_is_exact_generation_bound_and_readonly_even_on_backend_failure() {
    struct QueryBackend(Arc<Mutex<Result<u64, i32>>>);
    impl Backend for QueryBackend {
        fn attach(&mut self, _: usize, _: u32, _: u32) -> Result<(), i32> {
            Ok(())
        }
        fn resize(&mut self, _: u32, _: u32) -> Result<(), i32> {
            Ok(())
        }
        fn pause(&mut self) {}
        fn render_poc(&mut self) -> Result<bool, i32> {
            Ok(false)
        }
        fn detach(&mut self) {}
        fn window_surface_id(&mut self) -> Result<u64, i32> {
            *self.0.lock().unwrap()
        }
    }
    let result = Arc::new(Mutex::new(Ok(9_007_199_254_740_993)));
    let owned = result.clone();
    let engine = Arc::new(Engine::spawn(move || Box::new(QueryBackend(owned))).unwrap());
    let id = register_query_engine(engine.clone());
    let mut surface_id = 42;
    assert_eq!(
        unsafe { frd_ohos_window_surface_id(id, 0, &mut surface_id) },
        STALE_GENERATION
    );
    assert_eq!(surface_id, 0);
    let generation = engine
        .call(Command::Attach {
            window: 1,
            width: 100,
            height: 60,
        })
        .unwrap();
    let status = format!("{:?}", engine.status().unwrap());
    let link = query_link_snapshot(&engine);
    for (reply, expected) in [
        (Ok(9_007_199_254_740_993), OK),
        (Ok(u64::MAX), OK),
        (Ok(0), GPU_FAILED),
        (Err(GPU_FAILED), GPU_FAILED),
        (Err(WINDOW_REFERENCE_FAILED), WINDOW_REFERENCE_FAILED),
        (Err(WORKER_STOPPED), WORKER_STOPPED),
        (Err(PANIC), PANIC),
    ] {
        *result.lock().unwrap() = reply;
        surface_id = 42;
        assert_eq!(
            unsafe { frd_ohos_window_surface_id(id, generation, &mut surface_id) },
            expected
        );
        assert_eq!(surface_id, if expected == OK { reply.unwrap() } else { 0 });
        assert_eq!(format!("{:?}", engine.status().unwrap()), status);
        assert_eq!(query_link_snapshot(&engine), link);
    }
    surface_id = 42;
    assert_eq!(
        unsafe { frd_ohos_window_surface_id(id, generation + 1, &mut surface_id) },
        STALE_GENERATION
    );
    assert_eq!(surface_id, 0);
    assert_eq!(format!("{:?}", engine.status().unwrap()), status);
    assert_eq!(query_link_snapshot(&engine), link);
    engine.call(Command::Detach(generation)).unwrap();
    let detached = format!("{:?}", engine.status().unwrap());
    let detached_link = query_link_snapshot(&engine);
    surface_id = 42;
    assert_eq!(
        unsafe { frd_ohos_window_surface_id(id, generation, &mut surface_id) },
        NOT_ATTACHED
    );
    assert_eq!(surface_id, 0);
    assert_eq!(format!("{:?}", engine.status().unwrap()), detached);
    assert_eq!(query_link_snapshot(&engine), detached_link);
    let second = engine
        .call(Command::Attach {
            window: 2,
            width: 100,
            height: 60,
        })
        .unwrap();
    assert!(second > generation);
    let replacement = format!("{:?}", engine.status().unwrap());
    let replacement_link = query_link_snapshot(&engine);
    surface_id = 42;
    assert_eq!(
        unsafe { frd_ohos_window_surface_id(id, generation, &mut surface_id) },
        STALE_GENERATION
    );
    assert_eq!(surface_id, 0);
    assert_eq!(format!("{:?}", engine.status().unwrap()), replacement);
    assert_eq!(query_link_snapshot(&engine), replacement_link);
    assert_eq!(frd_ohos_destroy(id), OK);
}

#[test]
fn window_surface_id_abi_rejects_invalid_output_handle_and_unsupported_backend() {
    let mut surface_id = 42;
    assert_eq!(
        unsafe { frd_ohos_window_surface_id(0, 1, &mut surface_id) },
        INVALID_HANDLE
    );
    assert_eq!(surface_id, 0);
    assert_eq!(
        unsafe { frd_ohos_window_surface_id(0, 1, std::ptr::null_mut()) },
        INVALID_ARGUMENT
    );
    let (engine, _) = probe(false); // 没有实现查询的 backend 必须明确不可用。
    let engine = Arc::new(engine);
    let generation = engine
        .call(Command::Attach {
            window: 1,
            width: 10,
            height: 10,
        })
        .unwrap();
    let id = register_query_engine(engine.clone());
    let before = format!("{:?}", engine.status().unwrap());
    let link = query_link_snapshot(&engine);
    surface_id = 42;
    assert_eq!(
        unsafe { frd_ohos_window_surface_id(id, generation, &mut surface_id) },
        UNSUPPORTED_PLATFORM
    );
    assert_eq!(surface_id, 0);
    assert_eq!(format!("{:?}", engine.status().unwrap()), before);
    assert_eq!(query_link_snapshot(&engine), link);
    assert_eq!(frd_ohos_destroy(id), OK);
}

#[test]
fn window_surface_id_runs_on_unique_worker_after_background_remote_render() {
    use std::{sync::mpsc, thread, time::Duration};
    struct SerialBackend {
        entered: mpsc::Sender<thread::ThreadId>,
        release: mpsc::Receiver<()>,
        events: Arc<Mutex<Vec<(&'static str, thread::ThreadId)>>>,
        rendered: bool,
    }
    impl SerialBackend {
        fn record(&self, name: &'static str) {
            self.events
                .lock()
                .unwrap()
                .push((name, thread::current().id()));
        }
    }
    impl Backend for SerialBackend {
        fn attach(&mut self, _: usize, _: u32, _: u32) -> Result<(), i32> {
            self.record("attach");
            Ok(())
        }
        fn resize(&mut self, _: u32, _: u32) -> Result<(), i32> {
            self.record("resize");
            Ok(())
        }
        fn pause(&mut self) {
            self.record("pause");
        }
        fn render_poc(&mut self) -> Result<bool, i32> {
            panic!("远端呈现不能使用 POC")
        }
        fn detach(&mut self) {
            self.record("detach");
        }
        fn apply(&mut self, _: Vec<frd_frame::FrameTransaction>) -> Result<(), i32> {
            Ok(())
        }
        fn render_remote(
            &mut self,
            _: frd_core::TransformedViewport,
            _: bool,
        ) -> Result<Option<frd_protocol_api::PresentationEvent>, i32> {
            if !self.rendered {
                self.rendered = true;
                self.record("render-enter");
                self.entered.send(thread::current().id()).unwrap();
                self.release.recv().unwrap();
                self.record("render-exit");
            }
            Ok(None)
        }
        fn window_surface_id(&mut self) -> Result<u64, i32> {
            self.record("query");
            Ok(u64::MAX)
        }
    }
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let events = Arc::new(Mutex::new(Vec::new()));
    let owned = events.clone();
    let engine = Arc::new(
        Engine::spawn(move || {
            Box::new(SerialBackend {
                entered: entered_tx,
                release: release_rx,
                events: owned,
                rendered: false,
            })
        })
        .unwrap(),
    );
    let generation = engine
        .call(Command::Attach {
            window: 1,
            width: 100,
            height: 60,
        })
        .unwrap();
    {
        let mut link = engine.link.0.lock().unwrap();
        link.bound = true;
        link.session = Some((frd_core::SessionId::allocate(), 1));
        link.remote_size = Some(frd_core::PixelSize {
            width: 100,
            height: 60,
        });
        link.geometry = Some(input_adapter::ViewGeometry {
            drawable: frd_core::PixelSize {
                width: 100,
                height: 60,
            },
            bounds: frd_core::PixelRect {
                x: 0,
                y: 0,
                width: 100,
                height: 60,
            },
            scale: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        });
        let epoch = link.epoch().unwrap();
        link.batch = Some((epoch, vec![]));
    }
    let worker = entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_ne!(worker, thread::current().id());
    let id = register_query_engine(engine.clone());
    let (done_tx, done_rx) = mpsc::channel();
    let query = thread::spawn(move || {
        let mut surface_id = 0;
        let code = unsafe { frd_ohos_window_surface_id(id, generation, &mut surface_id) };
        done_tx.send((code, surface_id)).unwrap();
    });
    let pending = done_rx.recv_timeout(Duration::from_millis(40));
    release_tx.send(()).unwrap(); // 失败时也先放行 worker，避免测试泄漏线程。
    assert!(matches!(pending, Err(mpsc::RecvTimeoutError::Timeout)));
    assert_eq!(
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        (OK, u64::MAX)
    );
    query.join().unwrap();
    assert_eq!(frd_ohos_destroy(id), OK);
    let events = events.lock().unwrap();
    assert!(events.iter().all(|event| event.1 == worker));
    let names: Vec<_> = events.iter().map(|event| event.0).collect();
    assert!(
        names
            .iter()
            .position(|name| *name == "render-exit")
            .unwrap()
            < names.iter().position(|name| *name == "query").unwrap()
    );
    assert_eq!(names.iter().filter(|name| **name == "query").count(), 1);
}
