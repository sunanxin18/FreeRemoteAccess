//! 鸿蒙的输入/呈现共用物理几何。local revision 从不冒充远程 surface generation。
use frd_app::AppController;
use frd_core::{ContentViewport, InputEvent, PixelPoint, PixelRect, PixelSize, SessionId};
use frd_protocol_api::SessionCommand;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewGeometry {
    pub drawable: PixelSize,
    pub bounds: PixelRect,
    pub scale: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}
impl ViewGeometry {
    pub fn viewport(self, remote: PixelSize) -> Option<RemoteViewport> {
        if !self.scale.is_finite()
            || !(1.0..=8.0).contains(&self.scale)
            || !self.pan_x.is_finite()
            || !self.pan_y.is_finite()
        {
            return None;
        }
        let fit = ContentViewport::fit_in(remote, self.drawable, self.bounds)?;
        let w = fit.content.width as f32 * self.scale;
        let h = fit.content.height as f32 * self.scale;
        let x = fit.content.x as f32 + (fit.content.width as f32 - w) / 2.0 + self.pan_x;
        let y = fit.content.y as f32 + (fit.content.height as f32 - h) / 2.0 + self.pan_y;
        // 渲染器使用 origin/extent 的同一逆变换，clip 永远限制在安全内容矩形内。
        Some(RemoteViewport {
            remote,
            clip: self.bounds,
            x,
            y,
            width: w,
            height: h,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RemoteViewport {
    pub remote: PixelSize,
    pub clip: PixelRect,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl RemoteViewport {
    pub fn map(self, x: f32, y: f32) -> Option<PixelPoint> {
        if !x.is_finite()
            || !y.is_finite()
            || x < self.clip.x as f32
            || y < self.clip.y as f32
            || x >= (self.clip.x + self.clip.width) as f32
            || y >= (self.clip.y + self.clip.height) as f32
            || x < self.x
            || y < self.y
            || x >= self.x + self.width
            || y >= self.y + self.height
        {
            return None;
        }
        Some(PixelPoint {
            x: ((x - self.x) / (self.width - 1.0).max(1.0) * (self.remote.width - 1) as f32)
                .floor()
                .min((self.remote.width - 1) as f32) as u32,
            y: ((y - self.y) / (self.height - 1.0).max(1.0) * (self.remote.height - 1) as f32)
                .floor()
                .min((self.remote.height - 1) as f32) as u32,
        })
    }
}
#[derive(Default)]
pub struct InputAdapter {
    pub revision: u64,
    pub geometry: Option<ViewGeometry>,
    pub remote: Option<PixelSize>,
    focused: bool,
    foreground: bool,
    remote_owned: bool,
    blocked_until_release: bool,
    held_keys: std::collections::BTreeSet<u16>,
    held_buttons: frd_core::PointerButtons,
}
impl InputAdapter {
    // 必须在 controller epoch 仍有效时调用；即使 ledger 未见按下，也释放协议侧状态。
    pub fn release(&mut self, c: &AppController) -> Option<SessionCommand> {
        self.blocked_until_release = true;
        c.route_input(InputEvent::ReleaseAll)
    }
    pub fn focus(
        &mut self,
        c: &AppController,
        foreground: bool,
        focused: bool,
        remote_owned: bool,
    ) -> Option<SessionCommand> {
        let release = (!foreground || !focused || !remote_owned)
            .then(|| self.release(c))
            .flatten();
        self.foreground = foreground;
        self.focused = focused;
        self.remote_owned = remote_owned;
        release
    }
    pub fn set_geometry(&mut self, c: &AppController, g: ViewGeometry) -> Option<SessionCommand> {
        if self.geometry == Some(g) {
            return None;
        }
        let release = self.release(c);
        self.revision = self.revision.checked_add(1).unwrap_or(u64::MAX);
        self.geometry = Some(g);
        release
    }
    pub fn viewport(&self) -> Option<RemoteViewport> {
        self.geometry?.viewport(self.remote?)
    }
    pub fn route(
        &mut self,
        c: &AppController,
        session: u64,
        generation: u64,
        revision: u64,
        event: InputEvent,
        pressed: bool,
    ) -> Option<SessionCommand> {
        if revision != self.revision
            || c.interactive_input_epoch().map(|(s, g)| (s.get(), g)) != Some((session, generation))
            || self.viewport().is_none()
        {
            return None;
        }
        match &event {
            InputEvent::PhysicalKey { code, state, .. } => {
                if *state == frd_core::KeyState::Pressed {
                    self.held_keys.insert(code.usb_hid_usage());
                } else {
                    self.held_keys.remove(&code.usb_hid_usage());
                }
            }
            InputEvent::PointerSample(sample) => self.held_buttons = sample.buttons,
            _ => {}
        }
        // 已进入本地控件仍观察释放，避免前台恢复后重放旧held state。
        if !self.foreground || !self.focused || !self.remote_owned {
            return None;
        }
        if self.blocked_until_release {
            if !pressed && self.held_keys.is_empty() && !self.held_buttons.any_pressed() {
                self.blocked_until_release = false
            };
            return None;
        }
        c.route_input(event)
    }
    pub fn epoch(&self, c: &AppController) -> Option<(SessionId, u64)> {
        c.interactive_input_epoch()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use frd_frame::FrameCompleteness;
    use frd_protocol_api::PresentationEvent;
    struct Pins;
    impl frd_platform_api::ServerIdentityStore for Pins {
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
    fn fixture() -> (AppController, InputAdapter, SessionId) {
        let catalog = frd_protocol_api::ProtocolCatalog::new([frd_core::ProtocolId::rdp()]);
        let mut f = frd_ui_model::ConnectionForm::new(frd_ui_model::ConnectionDraft {
            target_system: Some(frd_core::TargetSystem::Windows),
            address: "fixture.invalid".into(),
            port: Some(3389),
            username: "fixture".into(),
            ..Default::default()
        });
        f.set_password(frd_core::SecretBuffer::from_text("fixture".into()));
        let submission = f.take_submission(&catalog).unwrap();
        let mut c = AppController::connection_form(f);
        let stores =
            crate::app_adapter::Stores::at(std::env::temp_dir().join("unused-ohos-fixture"));
        let action = c
            .handle_intent_with_stores(
                frd_app::AppIntent::Connect(submission),
                &catalog,
                frd_app::AppPlatformStores {
                    server_identities: &Pins,
                    profiles: stores.profiles.as_ref(),
                    credentials: stores.credentials.as_ref(),
                },
            )
            .unwrap()
            .unwrap();
        let frd_app::AppAction::StartSession(request, _permit) = action else {
            panic!("start")
        };
        let s = request.session_id;
        c.handle_session_event_with_stores(
            s,
            frd_protocol_api::SessionEvent::StageChanged(
                frd_protocol_api::ConnectionStage::TransportReady,
            ),
            frd_app::AppPlatformStores {
                server_identities: &Pins,
                profiles: stores.profiles.as_ref(),
                credentials: stores.credentials.as_ref(),
            },
        );
        c.handle_session_event_with_stores(
            s,
            frd_protocol_api::SessionEvent::SurfaceGenerationChanged {
                session_id: s,
                generation: 7,
                size: PixelSize {
                    width: 100,
                    height: 100,
                },
            },
            frd_app::AppPlatformStores {
                server_identities: &Pins,
                profiles: stores.profiles.as_ref(),
                credentials: stores.credentials.as_ref(),
            },
        );
        let g = ViewGeometry {
            drawable: PixelSize {
                width: 500,
                height: 400,
            },
            bounds: PixelRect {
                x: 30,
                y: 40,
                width: 440,
                height: 340,
            },
            scale: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        };
        let mut i = InputAdapter::default();
        i.set_geometry(&c, g);
        i.remote = Some(PixelSize {
            width: 100,
            height: 100,
        });
        i.focus(&c, true, true, true);
        (c, i, s)
    }
    #[test]
    fn only_full_current_baseline_accepts_input_and_old_geometry_rejects() {
        let (mut c, mut i, s) = fixture();
        let r = i.revision;
        i.route(&c, s.get(), 7, r, InputEvent::ReleaseAll, false);
        assert!(i
            .route(&c, s.get(), 7, r, InputEvent::ReleaseAll, false)
            .is_none());
        c.handle_presentation(PresentationEvent::FramePresented {
            session_id: s,
            generation: 7,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        });
        i.route(&c, s.get(), 7, r, InputEvent::ReleaseAll, false);
        assert!(i
            .route(&c, s.get(), 7, r, InputEvent::ReleaseAll, false)
            .is_some());
        let mut g = i.geometry.unwrap();
        g.bounds.x = 31;
        assert!(i.set_geometry(&c, g).is_some());
        assert!(i
            .route(&c, s.get(), 7, r, InputEvent::ReleaseAll, false)
            .is_none());
        assert!(i.focus(&c, false, true, true).is_some());
        assert!(i
            .route(&c, s.get(), 7, i.revision, InputEvent::ReleaseAll, false)
            .is_none());
    }
    #[test]
    fn clip_panzoom_nonfinite_and_edges_share_transform() {
        let (_, i, _) = fixture();
        let v = i.viewport().unwrap();
        assert!(v.map(0.0, 0.0).is_none());
        assert!(v.map(f32::NAN, 50.0).is_none());
        assert!(v.map(v.x + v.width, v.y).is_none());
        let mut g = i.geometry.unwrap();
        g.scale = 2.0;
        g.pan_x = 20.0;
        let v = g.viewport(i.remote.unwrap()).unwrap();
        assert!(v.map(30.0, 40.0).is_some());
        assert!(v.map(470.0, 40.0).is_none());
        g.scale = f32::INFINITY;
        assert!(g.viewport(i.remote.unwrap()).is_none());
    }
}
