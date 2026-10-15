use crate::{
    app_adapter::{AppEngine, Command, Edit, InputEpoch},
    input_adapter::ViewGeometry,
    *,
};
use frd_core::{
    InputEvent, KeyState, Modifiers, PhysicalKeyCode, PixelRect, PixelSize, PointerButtons,
    SecretBuffer, WheelDelta,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};
fn apps() -> &'static Mutex<(u64, BTreeMap<u64, Arc<AppEngine>>)> {
    static APPS: OnceLock<Mutex<(u64, BTreeMap<u64, Arc<AppEngine>>)>> = OnceLock::new();
    APPS.get_or_init(|| Mutex::new((0, BTreeMap::new())))
}
fn app(id: u64) -> Result<Arc<AppEngine>, i32> {
    apps()
        .lock()
        .map_err(|_| PANIC)?
        .1
        .get(&id)
        .cloned()
        .ok_or(INVALID_HANDLE)
}
unsafe fn bytes<'a>(ptr: *const u8, len: usize, max: usize) -> Result<&'a [u8], i32> {
    if len > max || (ptr.is_null() && len != 0) {
        return Err(INVALID_ARGUMENT);
    }
    if len == 0 {
        return Ok(&[]);
    }
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_create(path: *const u8, len: usize, out: *mut u64) -> i32 {
    boundary(|| {
        if out.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        unsafe { *out = 0 };
        let path = std::str::from_utf8(unsafe { bytes(path, len, 4096)? })
            .map_err(|_| INVALID_ARGUMENT)?;
        let root = PathBuf::from(path);
        if !root.is_absolute()
            || root
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(INVALID_ARGUMENT);
        }
        let engine = Arc::new(AppEngine::for_platform(root)?);
        let mut a = apps().lock().map_err(|_| PANIC)?;
        a.0 = a.0.checked_add(1).ok_or(EXHAUSTED)?;
        let id = a.0;
        a.1.insert(id, engine);
        unsafe { *out = id };
        Ok(())
    })
}
#[no_mangle]
pub extern "C" fn frd_ohos_app_destroy(id: u64) -> i32 {
    boundary(|| {
        let e = apps()
            .lock()
            .map_err(|_| PANIC)?
            .1
            .remove(&id)
            .ok_or(INVALID_HANDLE)?;
        e.stop();
        Ok(())
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_edit(id: u64, p: *const u8, n: usize) -> i32 {
    boundary(|| {
        let edit: Edit = serde_json::from_slice(unsafe { bytes(p, n, 16_384)? })
            .map_err(|_| INVALID_ARGUMENT)?;
        if edit.address.len() > 4096 || edit.username.len() > 4096 || edit.protocol.len() > 128 {
            return Err(INVALID_ARGUMENT);
        }
        app(id)?.send(Command::Edit(edit))
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_password(id: u64, p: *const u8, n: usize) -> i32 {
    boundary(|| {
        let b = unsafe { bytes(p, n, 65_536)? };
        std::str::from_utf8(b).map_err(|_| INVALID_ARGUMENT)?;
        app(id)?.send(Command::Password(SecretBuffer::new(b.to_vec())))
    })
}
#[no_mangle]
pub extern "C" fn frd_ohos_app_action(
    id: u64,
    action: u32,
    event: u64,
    flags: u32,
    index: u32,
) -> i32 {
    boundary(|| {
        let a = app(id)?;
        match action {
            1 => a.send(Command::Submit {
                event,
                composition: flags & 1 != 0,
                repeat: flags & 2 != 0,
            }),
            2 => {
                a.cancel();
                Ok(())
            }
            3 => a.send(Command::Intent(frd_app::AppIntent::ReturnToConnection)),
            4 => a.send(Command::Select(index as usize)),
            _ => Err(INVALID_ARGUMENT),
        }
    })
}
#[no_mangle]
pub extern "C" fn frd_ohos_app_focus(id: u64, foreground: u32, focused: u32, remote: u32) -> i32 {
    boundary(|| {
        if foreground > 1 || focused > 1 || remote > 1 {
            return Err(INVALID_ARGUMENT);
        }
        let a = app(id)?;
        if foreground == 0 || focused == 0 || remote == 0 {
            a.lose_focus();
            return Ok(());
        }
        a.send(Command::Focus {
            foreground: true,
            focused: true,
            remote: true,
        })
    })
}
/// ABI1 增量入口；不改变旧 POD 布局。JSON 不含凭据，最大 4096 字节。
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_local_display(id: u64, p: *const u8, n: usize) -> i32 {
    boundary(|| {
        let message = crate::local_display::Message::parse(unsafe { bytes(p, n, 4096)? })
            .ok_or(INVALID_ARGUMENT)?;
        app(id)?.send(Command::LocalDisplay(message))
    })
}
/// 版本化 IME 来源；仅借用调用期 JSON，固定错误，不导出输入内容。
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_ime_source(id: u64, p: *const u8, n: usize) -> i32 {
    boundary(|| {
        let source = crate::ime_policy::Source::parse(unsafe { bytes(p, n, 4096)? })
            .ok_or(INVALID_ARGUMENT)?;
        app(id)?.ime_source(source)
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_ime_operation(id: u64, p: *const u8, n: usize) -> i32 {
    boundary(|| {
        let operation = crate::ime_policy::Operation::parse(unsafe { bytes(p, n, 32768)? })
            .ok_or(INVALID_ARGUMENT)?;
        app(id)?.ime_operation(operation)
    })
}
#[repr(C)]
pub struct AppGeometry {
    pub abi_version: u32,
    pub struct_size: u32,
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    pub content_width: u32,
    pub content_height: u32,
    pub scale: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_geometry(id: u64, g: *const AppGeometry) -> i32 {
    boundary(|| {
        if g.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        let g = unsafe { &*g };
        if g.abi_version != 1 || g.struct_size as usize != std::mem::size_of::<AppGeometry>() {
            return Err(INVALID_ARGUMENT);
        }
        let v = ViewGeometry {
            drawable: PixelSize {
                width: g.width,
                height: g.height,
            },
            bounds: PixelRect {
                x: g.x,
                y: g.y,
                width: g.content_width,
                height: g.content_height,
            },
            scale: g.scale,
            pan_x: g.pan_x,
            pan_y: g.pan_y,
        };
        if v.viewport(PixelSize {
            width: 1,
            height: 1,
        })
        .is_none()
        {
            return Err(INVALID_ARGUMENT);
        }
        app(id)?.send(Command::Geometry(v))
    })
}
#[repr(C)]
pub struct AppInput {
    pub abi_version: u32,
    pub struct_size: u32,
    pub session: u64,
    pub generation: u64,
    pub revision: u64,
    pub kind: u32,
    pub flags: u32,
    pub x: f32,
    pub y: f32,
    pub hid: u32,
    pub modifiers: u32,
    pub wheel_x: i32,
    pub wheel_y: i32,
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_input(id: u64, p: *const AppInput) -> i32 {
    boundary(|| {
        if p.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        let p = unsafe { &*p };
        if p.abi_version != 1 || p.struct_size as usize != std::mem::size_of::<AppInput>() {
            return Err(INVALID_ARGUMENT);
        }
        let a = app(id)?;
        let epoch = InputEpoch {
            session: p.session,
            generation: p.generation,
            revision: p.revision,
        };
        match p.kind {
            1 => {
                if !p.x.is_finite()
                    || !p.y.is_finite()
                    || p.flags > 7
                    || !(-127..=127).contains(&p.wheel_x)
                    || !(-127..=127).contains(&p.wheel_y)
                {
                    return Err(INVALID_ARGUMENT);
                }
                a.send(Command::Pointer(
                    epoch,
                    p.x,
                    p.y,
                    PointerButtons {
                        primary: p.flags & 1 != 0,
                        middle: p.flags & 2 != 0,
                        secondary: p.flags & 4 != 0,
                    },
                    WheelDelta {
                        horizontal: p.wheel_x as i8,
                        vertical: p.wheel_y as i8,
                    },
                ))
            }
            2 => {
                if p.hid < 4 || p.hid > 0xe7 || p.flags > 1 || p.modifiers > 15 {
                    return Err(INVALID_ARGUMENT);
                }
                a.send(Command::Input(
                    epoch,
                    InputEvent::PhysicalKey {
                        code: PhysicalKeyCode::from_usb_hid_usage(p.hid as u16),
                        state: if p.flags == 1 {
                            KeyState::Pressed
                        } else {
                            KeyState::Released
                        },
                        modifiers: Modifiers {
                            shift: p.modifiers & 1 != 0,
                            control: p.modifiers & 2 != 0,
                            alt: p.modifiers & 4 != 0,
                            meta: p.modifiers & 8 != 0,
                        },
                    },
                    p.flags == 1,
                ))
            }
            3 => {
                a.lose_focus();
                Ok(())
            }
            _ => Err(INVALID_ARGUMENT),
        }
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_text(
    id: u64,
    session: u64,
    generation: u64,
    revision: u64,
    p: *const u8,
    n: usize,
) -> i32 {
    boundary(|| {
        let text =
            std::str::from_utf8(unsafe { bytes(p, n, 16_384)? }).map_err(|_| INVALID_ARGUMENT)?;
        if text.is_empty() {
            return Ok(());
        }
        app(id)?.legacy_text(
            InputEpoch {
                session,
                generation,
                revision,
            },
            text,
        )
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_snapshot(id: u64, out: *mut *mut u8, len: *mut usize) -> i32 {
    boundary(|| {
        if out.is_null() || len.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        unsafe {
            *out = std::ptr::null_mut();
            *len = 0
        };
        let snapshot = app(id)?.snapshot()?.into_boxed_slice();
        unsafe {
            *len = snapshot.len();
            *out = Box::into_raw(snapshot) as *mut u8
        };
        Ok(())
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_snapshot_free(p: *mut u8, n: usize) {
    if !p.is_null() {
        unsafe { drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(p, n))) }
    }
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_huks_probe(id: u64, path: *const u8, len: usize) -> i32 {
    boundary(|| {
        #[cfg(debug_assertions)]
        {
            let root = PathBuf::from(
                std::str::from_utf8(unsafe { bytes(path, len, 4096)? })
                    .map_err(|_| INVALID_ARGUMENT)?,
            );
            if !root.is_absolute() {
                return Err(INVALID_ARGUMENT);
            }
            app(id)?.send(Command::Probe(root.join("huks-probe")))
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = (id, path, len);
            Err(UNSUPPORTED_PLATFORM)
        }
    })
}

/// 仅 native bridge 使用；没有脚本可写的成功/首帧入口。
#[no_mangle]
pub extern "C" fn frd_ohos_app_bind_renderer(id: u64, renderer: u64) -> i32 {
    boundary(|| app(id)?.bind(lookup(renderer)?.link.clone()))
}

/// Native XComponent 的物理坐标；当前本地窗口代际必须匹配已呈现证明。
#[no_mangle]
pub extern "C" fn frd_ohos_app_native_pointer(
    id: u64,
    local: u64,
    x: f32,
    y: f32,
    buttons: u32,
    take_focus: u32,
) -> i32 {
    boundary(|| {
        if local == 0 || !x.is_finite() || !y.is_finite() || buttons > 7 || take_focus > 1 {
            return Err(INVALID_ARGUMENT);
        }
        app(id)?.native_pointer(
            local,
            x,
            y,
            PointerButtons {
                primary: buttons & 1 != 0,
                middle: buttons & 2 != 0,
                secondary: buttons & 4 != 0,
            },
            take_focus != 0,
        )
    })
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn limited_physical_profile_abi_uninstalled_down_is_unavailable_before_queue() {
        let root = std::env::temp_dir().join(format!(
            "frd-keyboard-abi-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        // 旧公共 spawn 不得因运行平台或 protocol 自报而默认安装 backend。
        let engine = Arc::new(
            AppEngine::spawn(
                crate::app_adapter::Stores::at(root),
                vec![],
                Arc::new(|_| {
                    Err(frd_media_api::VideoDecodeError::new(
                        frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
                    ))
                }),
                Arc::new(NoAudio),
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
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let value: serde_json::Value =
                serde_json::from_slice(&engine.snapshot().unwrap()).unwrap();
            if value["initialized"] == true {
                assert_eq!(value["keyboardBackend"], "unavailable");
                assert_eq!(value["physicalKeyInput"], false);
                assert_eq!(value["textInput"], false);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut p = AppInput {
            abi_version: 1,
            struct_size: std::mem::size_of::<AppInput>() as u32,
            session: 1,
            generation: 1,
            revision: 1,
            kind: 2,
            flags: 1,
            x: 0.0,
            y: 0.0,
            hid: 4,
            modifiers: 0,
            wheel_x: 0,
            wheel_y: 0,
        };
        let result = unsafe { frd_ohos_app_input(id, &p) };
        p.flags = 0;
        let tail = unsafe { frd_ohos_app_input(id, &p) };
        assert_eq!(frd_ohos_app_destroy(id), OK);
        assert_eq!(
            result,
            crate::UNSUPPORTED_PLATFORM,
            "未安装 backend 不得伪装已入队"
        );
        assert_eq!(tail, OK, "真实尾 Up 仍保留来源账本清理路径");
    }
    struct NoAudio;
    impl frd_shell_runtime::AudioOutputFactory for NoAudio {
        fn open(
            &self,
        ) -> Result<Box<dyn frd_media_api::AudioOutput>, frd_media_api::AudioOutputError> {
            Err(frd_media_api::AudioOutputError::Unavailable)
        }
    }
    struct CaptureFactory(std::sync::mpsc::Sender<frd_core::SessionInput>);
    impl frd_protocol_api::ProtocolFactory for CaptureFactory {
        fn descriptor(&self) -> frd_protocol_api::ProtocolDescriptor {
            frd_protocol_api::ProtocolDescriptor {
                id: frd_core::ProtocolId::rdp(),
                display_name: "实体键 consumer 夹具".into(),
                default_port: 3389,
                credential_requirements:
                    frd_protocol_api::CredentialRequirements::username_password(),
            }
        }
        fn create(
            &self,
            request: frd_protocol_api::ConnectRequest,
            runtime: frd_protocol_api::ProtocolRuntime,
        ) -> Result<Box<dyn frd_protocol_api::ProtocolSession>, frd_protocol_api::ProtocolError>
        {
            Ok(Box::new(CaptureSession {
                session: request.session_id,
                runtime,
                inputs: self.0.clone(),
            }))
        }
    }
    struct CaptureSession {
        session: frd_core::SessionId,
        runtime: frd_protocol_api::ProtocolRuntime,
        inputs: std::sync::mpsc::Sender<frd_core::SessionInput>,
    }
    impl frd_protocol_api::ProtocolSession for CaptureSession {
        fn run(mut self: Box<Self>) -> frd_protocol_api::ProtocolExit {
            use frd_protocol_api::*;
            let mut produce = || -> Result<(), ProtocolError> {
                self.runtime
                    .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))?;
                self.runtime
                    .publish_event(SessionEvent::CapabilitiesChanged(SessionCapabilities {
                        physical_key_input: true,
                        text_input: true,
                        ..Default::default()
                    }))?;
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
            if let Err(error) = produce() {
                return ProtocolExit::Failed(error);
            }
            while !self.runtime.requires_shutdown() {
                while let Some(command) = self.runtime.try_next_command() {
                    match command {
                        SessionCommand::Input(input) => {
                            let _ = self.inputs.send(input);
                        }
                        SessionCommand::Disconnect => return ProtocolExit::Closed,
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            ProtocolExit::Closed
        }
    }
    #[track_caller]
    fn wait_snapshot(
        engine: &AppEngine,
        predicate: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let value = serde_json::from_slice(&engine.snapshot().unwrap()).unwrap();
            if predicate(&value) {
                return value;
            }
            assert!(Instant::now() < deadline, "真实 owner FIFO 未达到指定阶段: page={} initialized={} code={} inputReady={} generation={}", value["page"],value["initialized"],value["statusCode"],value["inputReady"],value["generation"]);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn limited_physical_profile_actual_abi_engine_fifo_reaches_protocol_session() {
        use crate::app_adapter::Command;
        use crate::keyboard_policy::OhosKeyboardQualification;
        let (inputs, received) = std::sync::mpsc::channel();
        let engine = Arc::new(
            AppEngine::spawn_with_keyboard(
                isolated_abi_stores(),
                vec![Arc::new(CaptureFactory(inputs))],
                Arc::new(|_| {
                    Err(frd_media_api::VideoDecodeError::new(
                        frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
                    ))
                }),
                Arc::new(NoAudio),
                OhosKeyboardQualification::ArkUiLimited,
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
        struct App(u64);
        impl Drop for App {
            fn drop(&mut self) {
                if let Ok(mut registry) = apps().lock() {
                    if let Some(engine) = registry.1.remove(&self.0) {
                        engine.stop();
                    }
                }
            }
        }
        let _app = App(id);
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
        assert_eq!(waiting["physicalKeyInput"], true);
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
        let mut p = AppInput {
            abi_version: 1,
            struct_size: std::mem::size_of::<AppInput>() as u32,
            session: epoch.session.get(),
            generation: 1,
            revision: epoch.geometry,
            kind: 2,
            flags: 1,
            x: 0.0,
            y: 0.0,
            hid: 4,
            modifiers: 0,
            wheel_x: 0,
            wheel_y: 0,
        };
        // 首帧前已入队的 Down 不发到实际 consumer；真实 Up 清理隔离。
        assert_eq!(unsafe { frd_ohos_app_input(id, &p) }, OK);
        p.flags = 0;
        assert_eq!(unsafe { frd_ohos_app_input(id, &p) }, OK);
        engine
            .send(Command::Focus {
                foreground: true,
                focused: true,
                remote: true,
            })
            .unwrap();
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
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
        while received.try_recv().is_ok() {}
        for hid in [0x32, 0x48, 0x64, 0x66, 0xe3, 0xe7] {
            p.hid = hid;
            p.flags = 1;
            assert_eq!(unsafe { frd_ohos_app_input(id, &p) }, INVALID_ARGUMENT);
            p.flags = 0;
            assert_eq!(unsafe { frd_ohos_app_input(id, &p) }, OK);
        }
        // 旧 generation/revision 即使队列接受也不能到真实 consumer。
        for (generation, revision) in [(2, epoch.geometry), (1, epoch.geometry + 1)] {
            p.generation = generation;
            p.revision = revision;
            p.hid = 5;
            for down in [true, false] {
                p.flags = u32::from(down);
                assert_eq!(unsafe { frd_ohos_app_input(id, &p) }, OK);
            }
        }
        p.generation = 1;
        p.revision = epoch.geometry;
        // FIFO 中的物理键使用独立 usage，修饰由独立按键事件表达。
        let allowed = [
            0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11,
            0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
            0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d,
            0x2e, 0x2f, 0x30, 0x31, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c,
            0x3d, 0x3e, 0x3f, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x49, 0x4a, 0x4b,
            0x4c, 0x4d, 0x4e, 0x4f, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
            0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f, 0x60, 0x61, 0x62, 0x63, 0x65, 0xe0, 0xe1, 0xe2,
            0xe4, 0xe5, 0xe6,
        ];
        for hid in allowed {
            for down in [true, false] {
                p.hid = hid;
                p.flags = u32::from(down);
                assert_eq!(unsafe { frd_ohos_app_input(id, &p) }, OK);
                let input = received.recv_timeout(Duration::from_secs(2)).unwrap();
                assert_eq!(input.session_id, epoch.session);
                assert_eq!(input.generation, 1);
                assert_eq!(
                    input.event,
                    InputEvent::PhysicalKey {
                        code: PhysicalKeyCode::from_usb_hid_usage(hid as u16),
                        state: if down {
                            KeyState::Pressed
                        } else {
                            KeyState::Released
                        },
                        modifiers: Modifiers::default()
                    }
                );
            }
        }
        assert!(received.try_recv().is_err());
        assert_eq!(
            wait_snapshot(&engine, |s| s["page"] == 3)["keyboardBackend"],
            "arkui_limited"
        );
        engine.lose_focus();
        let release = received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(release.event, InputEvent::ReleaseAll);
        assert!(received.try_recv().is_err());
        for down in [true, false] {
            p.hid = 4;
            p.flags = u32::from(down);
            assert_eq!(unsafe { frd_ohos_app_input(id, &p) }, OK);
        }
        engine
            .send(Command::Focus {
                foreground: true,
                focused: true,
                remote: true,
            })
            .unwrap();
        // 上述后台 Down/Up 应完整阻断，恢复后普通键仍可继续。
        for down in [true, false] {
            p.hid = 5;
            p.flags = u32::from(down);
            assert_eq!(unsafe { frd_ohos_app_input(id, &p) }, OK);
            let input = received.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(
                input.event,
                InputEvent::PhysicalKey {
                    code: PhysicalKeyCode::from_usb_hid_usage(5),
                    state: if down {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    },
                    modifiers: Modifiers::default()
                }
            );
        }
        assert!(received.try_recv().is_err());
    }
    include!("ime_abi_tests.rs");
}
