#![cfg(all(
    target_os = "linux",
    feature = "gtk-shell",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
use frd_app::AppLaunch;
use frd_core::{
    CredentialProviderId, Endpoint, PixelRect, PixelSize, ProtocolId, SecretBuffer, SessionId,
    TargetSystem,
};
use frd_frame::{FrameCompleteness, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};
use frd_media_api::{AudioOutput, AudioOutputError};
use frd_platform_api::*;
use frd_protocol_api::*;
use frd_shell_desktop::AudioOutputFactory;
use frd_shell_gtk::{GtkRunner, GtkRunnerStores};
use frd_ui_model::LaunchOptions;
use gtk4::{glib, prelude::*};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
const PASSWORD: &str = "fixture-only-secret";
#[path = "support/gtk_snapshot.rs"]
mod gtk_snapshot;
struct Stores {
    saves: AtomicUsize,
    runner_started: AtomicBool,
    gtk_thread: std::thread::ThreadId,
}
fn key() -> ConnectionProfileKey {
    ConnectionProfileKey::new(ProtocolId::rdp(), "fixture.invalid", 3389, "fixture-user").unwrap()
}
impl CredentialProvider for Stores {
    fn load_username(&self, _: &CredentialProviderId) -> Result<String, PlatformError> {
        Err(PlatformError::Unavailable)
    }
    fn load_password(&self, _: &CredentialProviderId) -> Result<SecretBuffer, PlatformError> {
        Err(PlatformError::Unavailable)
    }
}
impl ServerIdentityStore for Stores {
    fn load_pin(&self, _: &ProtocolId, _: &Endpoint) -> Result<Option<[u8; 32]>, PlatformError> {
        Ok(None)
    }
    fn store_pin(&self, _: &ProtocolId, _: &Endpoint, _: [u8; 32]) -> Result<(), PlatformError> {
        Ok(())
    }
}
impl ConnectionProfileStore for Stores {
    fn list(&self) -> Result<Vec<SavedConnectionProfile>, PlatformError> {
        assert!(
            !self.runner_started.load(Ordering::SeqCst)
                || std::thread::current().id() != self.gtk_thread,
            "runner不得在GTK线程同步读取真实profile目录"
        );
        Ok(vec![SavedConnectionProfile {
            key: key(),
            target_system: TargetSystem::Windows,
            last_success_order: 1,
        }])
    }
    fn upsert(&self, _: &SavedConnectionProfile) -> Result<(), PlatformError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn delete(&self, _: &ConnectionProfileKey) -> Result<(), PlatformError> {
        Ok(())
    }
}
impl SecureCredentialStore for Stores {
    fn load(&self, _: &ConnectionProfileKey) -> Result<Option<SecretBuffer>, PlatformError> {
        Ok(Some(SecretBuffer::from_text(PASSWORD.into())))
    }
    fn stage(
        &self,
        _: SessionId,
        _: &ConnectionProfileKey,
        _: &SecretBuffer,
    ) -> Result<(), PlatformError> {
        Ok(())
    }
    fn commit(&self, _: SessionId, _: &ConnectionProfileKey) -> Result<(), PlatformError> {
        Ok(())
    }
    fn discard(&self, _: SessionId) -> Result<(), PlatformError> {
        Ok(())
    }
    fn delete(&self, _: &ConnectionProfileKey) -> Result<(), PlatformError> {
        Ok(())
    }
    fn purge_pending(&self) -> Result<(), PlatformError> {
        Ok(())
    }
}
struct Audio;
impl AudioOutputFactory for Audio {
    fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
        Err(AudioOutputError::Unavailable)
    }
}
struct Factory {
    starts: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
}
impl ProtocolFactory for Factory {
    fn descriptor(&self) -> ProtocolDescriptor {
        ProtocolId::rdp().into()
    }
    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
        let credentials = request.credentials.as_ref().unwrap();
        assert!(
            credentials.password.expose() == PASSWORD.as_bytes(),
            "保存的密码必须完整进入协议请求"
        );
        Ok(Box::new(Session {
            request,
            runtime,
            starts: self.starts.clone(),
            closed: self.closed.clone(),
        }))
    }
}
struct Session {
    request: ConnectRequest,
    runtime: ProtocolRuntime,
    starts: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
}
impl ProtocolSession for Session {
    fn run(mut self: Box<Self>) -> ProtocolExit {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let id = self.request.session_id;
        self.runtime
            .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))
            .unwrap();
        self.runtime
            .begin_generation(
                id,
                1,
                PixelSize::new(2, 2).unwrap(),
                PixelFormat::Bgrx8UnormSrgb,
            )
            .unwrap();
        self.runtime
            .publish_surface(SurfaceUpdate::Damage {
                session_id: id,
                generation: 1,
                revision: 1,
                patches: vec![PixelPatch {
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 2,
                    },
                    stride_bytes: 8,
                    pixels: PixelBuffer::new(vec![96; 16]),
                }],
            })
            .unwrap();
        self.runtime
            .publish_surface(SurfaceUpdate::FrameBoundary {
                session_id: id,
                generation: 1,
                revision: 1,
                completeness: FrameCompleteness::FullBaseline,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let increment_at = Instant::now() + Duration::from_millis(50);
        let mut increment_sent = false;
        while Instant::now() < deadline {
            if !increment_sent && Instant::now() >= increment_at {
                self.runtime
                    .publish_surface(SurfaceUpdate::Damage {
                        session_id: id,
                        generation: 1,
                        revision: 2,
                        patches: vec![PixelPatch {
                            rect: PixelRect {
                                x: 0,
                                y: 0,
                                width: 1,
                                height: 1,
                            },
                            stride_bytes: 4,
                            pixels: PixelBuffer::new(vec![160; 4]),
                        }],
                    })
                    .unwrap();
                self.runtime
                    .publish_surface(SurfaceUpdate::FrameBoundary {
                        session_id: id,
                        generation: 1,
                        revision: 2,
                        completeness: FrameCompleteness::Incremental,
                    })
                    .unwrap();
                increment_sent = true;
            }
            if matches!(
                self.runtime.try_next_command(),
                Some(SessionCommand::Disconnect)
            ) {
                self.closed.fetch_add(1, Ordering::SeqCst);
                return ProtocolExit::Closed;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("mock会话没有收到取消命令");
    }
}
fn find(root: &gtk4::Widget, name: &str) -> gtk4::Widget {
    if root.widget_name() == name {
        return root.clone();
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_optional(&widget, name) {
            return found;
        }
        child = widget.next_sibling();
    }
    panic!("缺少测试控件 {name}");
}
fn find_optional(root: &gtk4::Widget, name: &str) -> Option<gtk4::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_optional(&widget, name) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for _ in 0..128 {
            let context = glib::MainContext::default();
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        if condition() {
            return;
        }
        assert!(Instant::now() < deadline, "GTK runner状态超时");
        std::thread::sleep(Duration::from_millis(3));
    }
}
#[test]
#[ignore = "需要独立Linux GTK显示；真实GTK表单与mock协议，不连接网络服务器"]
fn native_gtk_login_session_cancel() {
    gtk4::init().unwrap();
    let backend = std::env::var("FRD_GTK_TEST_BACKEND").unwrap_or_else(|_| "x11".into());
    assert!(matches!(backend.as_str(), "x11" | "wayland"));
    let expected_type = if backend == "x11" {
        "GdkX11Display"
    } else {
        "GdkWaylandDisplay"
    };
    assert_eq!(
        gtk4::gdk::Display::default().unwrap().type_().name(),
        expected_type
    );
    let scale: i32 = std::env::var("FRD_GTK_TEST_SCALE")
        .unwrap_or_else(|_| "1".into())
        .parse()
        .unwrap();
    assert!(matches!(scale, 1 | 2));
    let artifacts = gtk_snapshot::Artifacts::from_env(&backend, scale);
    let stores = Arc::new(Stores {
        saves: AtomicUsize::new(0),
        runner_started: AtomicBool::new(false),
        gtk_thread: std::thread::current().id(),
    });
    let starts = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    let catalog = ProtocolCatalog::new([ProtocolId::rdp()]);
    let launch = AppLaunch::new_with_stores(
        LaunchOptions::default(),
        stores.as_ref(),
        &catalog,
        frd_app::AppPlatformStores {
            server_identities: stores.as_ref(),
            profiles: stores.as_ref(),
            credentials: stores.as_ref(),
        },
    );
    let runner = GtkRunner::new(
        launch,
        vec![Arc::new(Factory {
            starts: starts.clone(),
            closed: closed.clone(),
        })],
        GtkRunnerStores::new(stores.clone(), stores.clone(), stores.clone()),
        Arc::new(Audio),
    );
    stores.runner_started.store(true, Ordering::SeqCst);
    runner.present();
    let window = runner.window();
    let root = window.clone().upcast::<gtk4::Widget>();
    let profiles = find(&root, "frd-profiles")
        .downcast::<gtk4::DropDown>()
        .unwrap();
    let target = find(&root, "frd-target")
        .downcast::<gtk4::DropDown>()
        .unwrap();
    let password = find(&root, "frd-password")
        .downcast::<gtk4::PasswordEntry>()
        .unwrap();
    let connect = find(&root, "frd-connect")
        .downcast::<gtk4::Button>()
        .unwrap();
    let action = find(&root, "frd-session-action")
        .downcast::<gtk4::Button>()
        .unwrap();
    let status = find(&root, "frd-status").downcast::<gtk4::Label>().unwrap();
    if let Some(artifacts) = &artifacts {
        artifacts.login_themes(&window);
    }
    connect.emit_clicked();
    assert_eq!(starts.load(Ordering::SeqCst), 0, "无效表单不能启动");
    profiles.set_selected(1);
    until(|| password.text().as_str() == PASSWORD);
    if let Some(artifacts) = &artifacts {
        artifacts.masked_credentials(&window, &password);
    }
    profiles.set_selected(0);
    assert!(password.text().is_empty(), "新连接不能携带旧profile密码");
    connect.emit_clicked();
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    profiles.set_selected(1);
    until(|| password.text().as_str() == PASSWORD);
    target.set_selected(1);
    assert!(password.text().is_empty(), "改变远程身份必须清除旧凭据");
    profiles.set_selected(1);
    until(|| password.text().as_str() == PASSWORD);
    find(&root, "frd-remember")
        .downcast::<gtk4::CheckButton>()
        .unwrap()
        .set_active(true);
    // 原生PasswordEntry激活信号与按钮同一路径；不是XTEST硬件按键注入证明。
    password.emit_by_name::<()>("activate", &[]);
    password.emit_by_name::<()>("activate", &[]);
    until(|| starts.load(Ordering::SeqCst) == 1 && status.text().contains("准备远程画面"));
    assert!(password.text().is_empty());
    let first_area = find(&root, "frd-remote")
        .downcast::<gtk4::GLArea>()
        .unwrap();
    until(|| first_area.context().is_some());
    until(|| read_increment(&first_area));
    if let Some(artifacts) = &artifacts {
        artifacts.capture(&window, "connected-preparing");
    }
    assert_eq!(first_area.scale_factor(), scale);
    until(|| stores.saves.load(Ordering::SeqCst) == 1);
    action.emit_clicked();
    until(|| closed.load(Ordering::SeqCst) == 1 && status.text() == "未连接");
    assert_eq!(starts.load(Ordering::SeqCst), 1, "Enter必须只启动一个会话");
    profiles.set_selected(1);
    until(|| password.text().as_str() == PASSWORD);
    password.emit_by_name::<()>("activate", &[]);
    until(|| starts.load(Ordering::SeqCst) == 2 && status.text().contains("准备远程画面"));
    let second_area = find(&root, "frd-remote")
        .downcast::<gtk4::GLArea>()
        .unwrap();
    assert_ne!(first_area, second_area, "新会话必须替换旧画面对象");
    until(|| second_area.context().is_some());
    until(|| read_increment(&second_area));
    action.emit_clicked();
    until(|| closed.load(Ordering::SeqCst) == 2 && status.text() == "未连接");
    profiles.set_selected(1);
    until(|| password.text().as_str() == PASSWORD);
    password.emit_by_name::<()>("activate", &[]);
    action.emit_clicked();
    until(|| status.text() == "未连接");
    window.close();
    until(|| !window.is_visible());
    println!("native GTK login saved_secret=1 identity_invalidation=1 enter_single_launch=1 deferred_save=1 cancel_cleanup=1 pending_launch_cancel=1");
}

// 仅测试读回：真实mock增量须经过host mailbox→compiler→新画布GL上传/绘制。
fn read_increment(area: &gtk4::GLArea) -> bool {
    use glow::HasContext;
    if area.context().is_none() {
        return false;
    }
    area.make_current();
    unsafe {
        let library = libloading::Library::new("libepoxy.so.0").unwrap();
        let gl = glow::Context::from_loader_function(|name| {
            library
                .get::<*const *const std::ffi::c_void>(format!("epoxy_{name}\0").as_bytes())
                .map_or(std::ptr::null(), |symbol| **symbol)
        });
        let fbo = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING);
        if fbo == 0 {
            return false;
        }
        let previous = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
        gl.bind_framebuffer(
            glow::READ_FRAMEBUFFER,
            std::num::NonZeroU32::new(fbo as u32).map(glow::NativeFramebuffer),
        );
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
        gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
        gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, 0);
        gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 0);
        let width = area.width() * area.scale_factor();
        let height = area.height() * area.scale_factor();
        let side = width.min(height);
        let mut pixel = [0u8; 4];
        gl.read_pixels(
            (width - side) / 2,
            (height + side) / 2 - 1,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixel)),
        );
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        gl.bind_framebuffer(
            glow::READ_FRAMEBUFFER,
            std::num::NonZeroU32::new(previous as u32).map(glow::NativeFramebuffer),
        );
        pixel[3] == 255 && pixel[..3].iter().all(|value| value.abs_diff(160) <= 1)
    }
}
