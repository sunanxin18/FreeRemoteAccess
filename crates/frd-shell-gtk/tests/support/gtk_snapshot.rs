//! 测试专用 WidgetPaintable → GSK 离屏 PNG；不是桌面截图或窗口提交证明。
use gtk4::{glib, prelude::*};
use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct Artifacts {
    directory: PathBuf,
    backend: String,
    scale: i32,
}
impl Artifacts {
    pub fn from_env(backend: &str, scale: i32) -> Option<Self> {
        let directory = std::env::var_os("FRD_GTK_ARTIFACT_DIR")?;
        assert!(!directory.is_empty(), "显式 artifact 目录不能为空");
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).expect("必须创建 PNG artifact 目录");
        Some(Self {
            directory,
            backend: backend.to_owned(),
            scale,
        })
    }
    pub fn login_themes(&self, window: &gtk4::Window) {
        struct Restore(gtk4::Settings, bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                self.0.set_gtk_application_prefer_dark_theme(self.1);
            }
        }
        let settings = gtk4::Settings::for_display(&WidgetExt::display(window));
        let original = settings.is_gtk_application_prefer_dark_theme();
        let _restore = Restore(settings.clone(), original);
        settings.set_gtk_application_prefer_dark_theme(false);
        self.capture(window, "login-light");
        settings.set_gtk_application_prefer_dark_theme(true);
        self.capture(window, "login-dark");
    }
    pub fn masked_credentials(&self, window: &gtk4::Window, password: &gtk4::PasswordEntry) {
        fn text_child(widget: &gtk4::Widget) -> Option<gtk4::Text> {
            if let Ok(text) = widget.clone().downcast::<gtk4::Text>() {
                return Some(text);
            }
            let mut next = widget.first_child();
            while let Some(child) = next {
                if let Some(text) = text_child(&child) {
                    return Some(text);
                }
                next = child.next_sibling();
            }
            None
        }
        assert!(
            !password.text().is_empty(),
            "此快照要求已加载 synthetic 凭据"
        );
        let text = text_child(password.upcast_ref()).expect("PasswordEntry 必须有受保护的文本部件");
        assert!(!text.is_visible(), "PasswordEntry 内部文本必须处于掩码模式");
        self.capture(window, "saved-credentials-masked");
    }
    pub fn capture(&self, window: &gtk4::Window, stage: &str) {
        assert!(
            window.is_realized() && window.is_mapped(),
            "仅捕获真实已 present 的 runner window"
        );
        let clock = window.frame_clock().expect("窗口必须有 frame clock");
        let previous = clock.frame_counter();
        window.queue_draw();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let context = glib::MainContext::default();
            if context.pending() {
                context.iteration(false);
            }
            if clock.frame_counter() > previous && window.width() > 0 && window.height() > 0 {
                break;
            }
            assert!(Instant::now() < deadline, "widget 快照前必须完成有界重绘");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(window.scale_factor(), self.scale);
        let width = window.width().checked_mul(self.scale).unwrap();
        let height = window.height().checked_mul(self.scale).unwrap();
        assert!(
            (1..=4096).contains(&width) && (1..=4096).contains(&height),
            "快照尺寸必须有界"
        );
        let snapshot = gtk4::Snapshot::new();
        snapshot.scale(self.scale as f32, self.scale as f32);
        gtk4::WidgetPaintable::new(Some(window)).snapshot(
            &snapshot,
            window.width() as f64,
            window.height() as f64,
        );
        let node = snapshot
            .to_node()
            .expect("已绘制窗口必须产生 widget render node");
        let texture = window
            .renderer()
            .expect("必须使用实际窗口的 GSK renderer")
            .render_texture(
                node,
                Some(&gtk4::graphene::Rect::new(
                    0.,
                    0.,
                    width as f32,
                    height as f32,
                )),
            );
        assert_eq!((texture.width(), texture.height()), (width, height));
        let path = self.directory.join(format!(
            "gtk-widget-{}-{}x-{stage}.png",
            self.backend, self.scale
        ));
        texture.save_to_png(&path).expect("PNG 保存失败不能跳过");
        let mut file = std::fs::File::open(&path).expect("PNG 必须存在");
        assert!((24..=64 * 1024 * 1024).contains(&file.metadata().unwrap().len()));
        let mut header = [0u8; 24];
        file.read_exact(&mut header).unwrap();
        assert_eq!(&header[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&header[12..16], b"IHDR");
        assert_eq!(
            u32::from_be_bytes(header[16..20].try_into().unwrap()),
            width as u32
        );
        assert_eq!(
            u32::from_be_bytes(header[20..24].try_into().unwrap()),
            height as u32
        );
        println!("GTK widget snapshot stage={stage} backend={} scale={} width={width} height={height}; offscreen GSK PNG, not desktop or window-submission evidence", self.backend, self.scale);
    }
}
