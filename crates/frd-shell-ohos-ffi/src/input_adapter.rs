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
    quarantined_keys: std::collections::BTreeSet<u16>,
    quarantined_buttons: bool,
    owned_epoch: Option<(u64, u64, u64)>,
    last_pointer: Option<PixelPoint>,
    held_keys: std::collections::BTreeSet<u16>,
    held_buttons: frd_core::PointerButtons,
}
impl InputAdapter {
    // 必须在 controller epoch 仍有效时调用；即使 ledger 未见按下，也释放协议侧状态。
    pub fn release(&mut self, c: &AppController) -> Option<SessionCommand> {
        self.quarantine_owned();
        c.route_input(InputEvent::ReleaseAll)
    }
    fn quarantine_owned(&mut self) {
        self.quarantined_keys.append(&mut self.held_keys);
        self.quarantined_buttons |= self.held_buttons.any_pressed();
        self.held_buttons = frd_core::PointerButtons::default();
        self.last_pointer = None;
    }
    pub fn pointer(
        &mut self,
        c: &AppController,
        session: u64,
        generation: u64,
        revision: u64,
        x: f32,
        y: f32,
        buttons: frd_core::PointerButtons,
        wheel: frd_core::WheelDelta,
    ) -> Option<SessionCommand> {
        if revision != self.revision
            || c.interactive_input_epoch().map(|(s, g)| (s.get(), g)) != Some((session, generation))
        {
            return None;
        }
        if let Some(remote) = self.viewport().and_then(|v| v.map(x, y)) {
            let command = self.route(
                c,
                session,
                generation,
                revision,
                InputEvent::PointerSample(frd_core::PointerSample::new(remote, buttons, wheel)),
                buttons.any_pressed(),
            );
            if command.is_some() {
                self.last_pointer = Some(remote);
            }
            return command;
        }
        // 坐标在 letterbox/clip 外也必须处理释放；绝不把越界移动变成新的按下。
        if !buttons.any_pressed() {
            self.quarantined_buttons = false;
        }
        if self.held_buttons.any_pressed() && buttons != self.held_buttons {
            let release_buttons = frd_core::PointerButtons {
                primary: self.held_buttons.primary && buttons.primary,
                middle: self.held_buttons.middle && buttons.middle,
                secondary: self.held_buttons.secondary && buttons.secondary,
            };
            let remote = self.last_pointer?;
            return self.route(
                c,
                session,
                generation,
                revision,
                InputEvent::PointerSample(frd_core::PointerSample::new(
                    remote,
                    release_buttons,
                    frd_core::WheelDelta::default(),
                )),
                release_buttons.any_pressed(),
            );
        }
        None
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
        _pressed: bool,
    ) -> Option<SessionCommand> {
        if revision != self.revision
            || c.interactive_input_epoch().map(|(s, g)| (s.get(), g)) != Some((session, generation))
            || self.viewport().is_none()
        {
            return None;
        }
        let epoch = (session, generation, revision);
        if self.owned_epoch != Some(epoch) {
            self.quarantine_owned();
            self.owned_epoch = Some(epoch);
        }
        let enabled = self.foreground && self.focused && self.remote_owned;
        match &event {
            InputEvent::PhysicalKey { code, state, .. } => {
                let code = code.usb_hid_usage();
                if *state == frd_core::KeyState::Released {
                    self.held_keys.remove(&code);
                    if self.quarantined_keys.remove(&code) {
                        return None;
                    }
                } else {
                    // 旧物理按键的重复不重放；只隔离该键，不阻塞其他键/文本/指针。
                    if !enabled {
                        self.quarantined_keys.insert(code);
                        return None;
                    }
                    if self.quarantined_keys.contains(&code) {
                        return None;
                    }
                    self.held_keys.insert(code);
                }
            }
            InputEvent::PointerSample(sample) => {
                if self.quarantined_buttons {
                    self.quarantined_buttons = sample.buttons.any_pressed();
                    return None;
                }
                if !enabled {
                    self.quarantined_buttons |= sample.buttons.any_pressed();
                    return None;
                }
                self.held_buttons = sample.buttons;
            }
            InputEvent::ReleaseAll => self.quarantine_owned(),
            _ => {}
        }
        if !enabled {
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
    fn active_fixture() -> (AppController, InputAdapter, SessionId) {
        let (mut c, i, s) = fixture();
        c.set_platform_capabilities(frd_platform_api::PlatformCapabilities {
            text_input: true,
            ..Default::default()
        });
        c.set_product_policy(frd_app::ProductPolicy {
            text_input: true,
            ..Default::default()
        });
        let stores =
            crate::app_adapter::Stores::at(std::env::temp_dir().join("unused-ohos-fixture"));
        c.handle_session_event_with_stores(
            s,
            frd_protocol_api::SessionEvent::CapabilitiesChanged(
                frd_protocol_api::SessionCapabilities {
                    text_input: true,
                    ..Default::default()
                },
            ),
            frd_app::AppPlatformStores {
                server_identities: &Pins,
                profiles: stores.profiles.as_ref(),
                credentials: stores.credentials.as_ref(),
            },
        );
        c.handle_presentation(PresentationEvent::FramePresented {
            session_id: s,
            generation: 7,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        });
        (c, i, s)
    }
    fn key(code: u16, state: frd_core::KeyState) -> InputEvent {
        InputEvent::PhysicalKey {
            code: frd_core::PhysicalKeyCode::from_usb_hid_usage(code),
            state,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn pointer_up_in_letterbox_and_beyond_panzoom_clip_releases_at_last_valid_point() {
        for (zoom, outside_y) in [(1.0, 210.0), (2.0, 10.0)] {
            let (c, mut i, s) = active_fixture();
            let mut g = i.geometry.unwrap();
            g.scale = zoom;
            i.set_geometry(&c, g);
            let revision = i.revision;
            let down = frd_core::PointerButtons {
                primary: true,
                ..Default::default()
            };
            let sent = i.pointer(
                &c,
                s.get(),
                7,
                revision,
                250.0,
                210.0,
                down,
                Default::default(),
            );
            assert!(sent.is_some());
            assert!(i
                .pointer(
                    &c,
                    s.get(),
                    7,
                    revision,
                    31.0,
                    outside_y,
                    down,
                    Default::default()
                )
                .is_none());
            let up = i
                .pointer(
                    &c,
                    s.get(),
                    7,
                    revision,
                    31.0,
                    outside_y,
                    Default::default(),
                    Default::default(),
                )
                .unwrap();
            let SessionCommand::Input(input) = up else {
                panic!("input");
            };
            let InputEvent::PointerSample(sample) = input.event else {
                panic!("sample");
            };
            assert!(!sample.buttons.any_pressed());
            assert_eq!(sample.remote, i.last_pointer.unwrap());
            assert!(!i.held_buttons.any_pressed());
        }
    }
    #[test]
    fn missing_old_key_up_does_not_block_fresh_epoch_or_replay_old_repeat() {
        let (c, mut i, s) = active_fixture();
        let revision = i.revision;
        assert!(i
            .route(
                &c,
                s.get(),
                7,
                revision,
                key(4, frd_core::KeyState::Pressed),
                true
            )
            .is_some());
        assert!(i.focus(&c, false, false, false).is_some());
        assert!(i.held_keys.is_empty());
        // 原 up 在应用失焦期间丢失；下一会话不能被旧 remote ledger 永久阻塞。
        let (next, _, next_session) = active_fixture();
        assert_ne!(s, next_session);
        i.focus(&next, true, true, true);
        assert!(i
            .route(
                &next,
                next_session.get(),
                7,
                revision,
                key(4, frd_core::KeyState::Pressed),
                true
            )
            .is_none());
        assert!(i
            .route(
                &next,
                next_session.get(),
                7,
                revision,
                key(5, frd_core::KeyState::Pressed),
                true
            )
            .is_some());
        assert!(i
            .route(
                &next,
                next_session.get(),
                7,
                revision,
                InputEvent::Text {
                    utf8: "新输入".into()
                },
                false
            )
            .is_some());
        assert!(i
            .pointer(
                &next,
                next_session.get(),
                7,
                revision,
                250.0,
                210.0,
                Default::default(),
                Default::default()
            )
            .is_some());
        assert!(i
            .route(
                &next,
                next_session.get(),
                7,
                revision,
                key(4, frd_core::KeyState::Released),
                false
            )
            .is_none());
        assert!(i
            .route(
                &next,
                next_session.get(),
                7,
                revision,
                key(4, frd_core::KeyState::Pressed),
                true
            )
            .is_some());
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
