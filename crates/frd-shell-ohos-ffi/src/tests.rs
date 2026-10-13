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
