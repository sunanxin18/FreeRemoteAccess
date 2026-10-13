//! 编译期显式开启的非网络验收夹具。只通过真实 runtime 生产帧，不提供成功 setter。
use frd_core::{PixelRect, PixelSize, ProtocolId, SessionId};
use frd_frame::{FrameCompleteness, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};
use frd_protocol_api::*;
use std::sync::atomic::{AtomicU64, Ordering};
pub(crate) static INPUTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static POINTER_X: AtomicU64 = AtomicU64::new(0);
pub(crate) static POINTER_Y: AtomicU64 = AtomicU64::new(0);
pub(crate) static RELEASES: AtomicU64 = AtomicU64::new(0);
pub(crate) struct Factory;
impl ProtocolFactory for Factory {
    fn descriptor(&self) -> ProtocolDescriptor {
        ProtocolDescriptor {
            id: ProtocolId::rdp(),
            display_name: "合成帧验收（无网络）".into(),
            default_port: 3389,
            credential_requirements: CredentialRequirements::username_password(),
        }
    }
    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
        if request.endpoint.host() != "fixture.invalid" {
            return Err(ProtocolError::UnsupportedTargetProtocol);
        }
        Ok(Box::new(Session {
            session: request.session_id,
            runtime,
        }))
    }
}
struct Session {
    session: SessionId,
    runtime: ProtocolRuntime,
}
impl Session {
    fn produce(&mut self) -> Result<(), ProtocolError> {
        let size = PixelSize {
            width: 320,
            height: 200,
        };
        self.runtime
            .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))?;
        self.runtime
            .publish_event(SessionEvent::CapabilitiesChanged(SessionCapabilities {
                text_input: true,
                ..Default::default()
            }))?;
        self.runtime
            .begin_generation(self.session, 1, size, PixelFormat::Bgrx8UnormSrgb)?;
        let mut pixels = Vec::with_capacity(320 * 200 * 4);
        for y in 0..200 {
            for x in 0..320 {
                pixels.extend_from_slice(match (x < 160, y < 100) {
                    (true, true) => &[0, 0, 255, 0],
                    (false, true) => &[0, 255, 0, 0],
                    (true, false) => &[255, 0, 0, 0],
                    _ => &[255, 255, 255, 0],
                });
            }
        }
        self.runtime.publish_surface(SurfaceUpdate::Damage {
            session_id: self.session,
            generation: 1,
            revision: 1,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 320,
                    height: 200,
                },
                stride_bytes: 1280,
                pixels: PixelBuffer::new(pixels),
            }],
        })?;
        self.runtime.publish_surface(SurfaceUpdate::FrameBoundary {
            session_id: self.session,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        })
    }
}
impl ProtocolSession for Session {
    fn run(mut self: Box<Self>) -> ProtocolExit {
        if let Err(error) = self.produce() {
            return ProtocolExit::Failed(error);
        }
        while !self.runtime.requires_shutdown() {
            while let Some(command) = self.runtime.try_next_command() {
                match command {
                    SessionCommand::Disconnect => return ProtocolExit::Closed,
                    SessionCommand::Input(input) => {
                        INPUTS.fetch_add(1, Ordering::Relaxed);
                        if let frd_core::InputEvent::PointerSample(sample) = &input.event {
                            POINTER_X.store(sample.remote.x.into(), Ordering::Relaxed);
                            POINTER_Y.store(sample.remote.y.into(), Ordering::Relaxed);
                        }
                        if matches!(input.event, frd_core::InputEvent::ReleaseAll) {
                            RELEASES.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    _ => {}
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        ProtocolExit::Closed
    }
}

#[cfg(target_env = "ohos")]
pub(crate) fn observe(s: &crate::presentation::LinkState, ready: bool) {
    use std::sync::Mutex;
    static LAST: Mutex<Option<(u64, u64, u64, u64, bool, u64, u64, u64, u64)>> = Mutex::new(None);
    let marker = (
        s.applied,
        s.presented,
        s.surface.local,
        s.surface.lifecycle,
        ready,
        INPUTS.load(Ordering::Relaxed),
        RELEASES.load(Ordering::Relaxed),
        POINTER_X.load(Ordering::Relaxed),
        POINTER_Y.load(Ordering::Relaxed),
    );
    let Ok(mut last) = LAST.lock() else {
        return;
    };
    if *last == Some(marker) {
        return;
    }
    *last = Some(marker);
    let text=std::ffi::CString::new(format!("synthetic=1 network=0 applied={} presented={} local={} lifecycle={} ready={} inputs={} releases={} remoteX={} remoteY={}",marker.0,marker.1,marker.2,marker.3,u8::from(marker.4),marker.5,marker.6,marker.7,marker.8)).unwrap();
    #[link(name = "hilog_ndk.z")]
    extern "C" {
        fn OH_LOG_Print(
            t: u32,
            l: u32,
            d: u32,
            tag: *const std::ffi::c_char,
            fmt: *const std::ffi::c_char,
            ...
        ) -> i32;
    }
    // 固定类别与非秘密计数；绝不输出通用应用快照、凭据或网络内容。
    unsafe {
        OH_LOG_Print(
            0,
            4,
            0xFF00,
            c"FrdSynthetic".as_ptr(),
            c"%{public}s".as_ptr(),
            text.as_ptr(),
        );
    }
}
