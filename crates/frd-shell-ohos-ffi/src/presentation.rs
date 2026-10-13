//! 单个有界批次与单个确认槽。两个 owner 从不在持锁时跨线程等待。
use crate::{input_adapter::ViewGeometry, worker::Backend, GPU_FAILED, PANIC};
use frd_core::{SessionId, TransformedViewport};
use frd_frame::FrameTransaction;
use frd_protocol_api::PresentationEvent;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Epoch {
    pub session: SessionId,
    pub remote: u64,
    pub local: u64,
    pub lifecycle: u64,
    pub geometry: u64,
}
#[derive(Clone, Copy, Default)]
pub(crate) struct Surface {
    pub local: u64,
    pub lifecycle: u64,
    pub width: u32,
    pub height: u32,
    pub attached: bool,
    pub foreground: bool,
}
pub(crate) struct Feedback {
    pub epoch: Epoch,
    pub event: Option<PresentationEvent>,
}
#[derive(Default)]
pub(crate) struct LinkState {
    pub bound: bool,
    pub surface: Surface,
    pub session: Option<(SessionId, u64)>,
    pub geometry: Option<ViewGeometry>,
    pub geometry_revision: u64,
    pub remote_size: Option<frd_core::PixelSize>,
    pub batch: Option<(Epoch, Vec<FrameTransaction>)>,
    pub feedback: Option<Feedback>,
    pub busy: bool,
    pub failure: Option<i32>,
    pub applied: u64,
    pub presented: u64,
}
impl LinkState {
    pub fn epoch(&self) -> Option<Epoch> {
        let (session, remote) = self.session?;
        Some(Epoch {
            session,
            remote,
            local: self.surface.local,
            lifecycle: self.surface.lifecycle,
            geometry: self.geometry_revision,
        })
    }
    pub fn viewport(&self) -> Option<TransformedViewport> {
        if !self.surface.attached || !self.surface.foreground {
            return None;
        }
        let g = self.geometry?;
        if g.drawable.width != self.surface.width || g.drawable.height != self.surface.height {
            return None;
        }
        g.transformed(self.remote_size?)
    }
    pub fn ready_epoch(&self) -> Option<Epoch> {
        if self.failure.is_some() || self.viewport().is_none() {
            return None;
        }
        self.epoch()
    }
}
#[derive(Default)]
pub(crate) struct Link(pub Mutex<LinkState>);
impl Link {
    pub fn fail(&self, code: i32) {
        if let Ok(mut s) = self.0.lock() {
            s.failure.get_or_insert(code);
        }
    }
    pub fn surface(&self, status: crate::PocStatus) {
        if let Ok(mut s) = self.0.lock() {
            s.surface = Surface {
                local: status.surface_generation,
                lifecycle: s.surface.lifecycle.saturating_add(1),
                width: status.width,
                height: status.height,
                attached: status.attached != 0,
                foreground: status.foreground != 0,
            };
        }
    }
}
#[derive(Default)]
pub(crate) struct Consumer {
    session: Option<(SessionId, u64)>,
    presented: Option<Epoch>,
    initialized: bool,
}
impl Consumer {
    pub fn poll(&mut self, link: &Link, backend: &mut dyn Backend) -> Result<bool, i32> {
        let (epoch, session, viewport, batch) = {
            let mut s = link.0.lock().map_err(|_| PANIC)?;
            if !s.bound || s.failure.is_some() {
                return Ok(false);
            }
            if s.feedback.is_some() {
                if self.session == s.session {
                    return Ok(false);
                }
                s.feedback = None;
            }
            (s.epoch(), s.session, s.viewport(), s.batch.take())
        };
        if self.session != session {
            backend.reset_remote();
            self.session = session;
            self.presented = None;
            self.initialized = false;
        }
        let Some(epoch) = epoch else {
            return Ok(false);
        };
        let had_batch = batch.is_some();
        if let Some((identity, batch)) = batch {
            if (identity.session, identity.remote) == (epoch.session, epoch.remote) {
                for transaction in &batch {
                    let revision = match transaction {
                        FrameTransaction::Startup {
                            reset, revision, ..
                        } => {
                            if reset.session_id != epoch.session || reset.generation != epoch.remote
                            {
                                return Err(GPU_FAILED);
                            }
                            revision
                        }
                        FrameTransaction::Revision { revision, .. } => revision,
                    };
                    if revision.session_id != epoch.session || revision.generation != epoch.remote {
                        return Err(GPU_FAILED);
                    }
                }
                backend.apply(batch)?;
                self.initialized = true;
                link.0.lock().map_err(|_| PANIC)?.applied += 1;
            }
        }
        if !had_batch && self.presented == Some(epoch) {
            return Ok(false);
        }
        let mut event = None;
        if let Some(viewport) = viewport.filter(|_| self.initialized) {
            let repaint = self.presented != Some(epoch);
            event = backend.render_remote(viewport, repaint)?;
            if event.is_some() {
                self.presented = Some(epoch);
            }
        }
        let presented = event.is_some();
        if had_batch || presented {
            let mut s = link.0.lock().map_err(|_| PANIC)?;
            // 唯一 render consumer，feedback 槽在取任务前为空；app 只消费不生产。
            if presented {
                s.presented += 1;
            }
            s.feedback = Some(Feedback { epoch, event });
        }
        Ok(presented)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frd_frame::{FrameCompleteness, FrameReset, FrameRevision, PixelFormat};
    struct Mock {
        revisions: Vec<u64>,
        receipt: Option<PresentationEvent>,
        fail: bool,
        resets: usize,
    }
    impl Backend for Mock {
        fn attach(&mut self, _: usize, _: u32, _: u32) -> Result<(), i32> {
            Ok(())
        }
        fn resize(&mut self, _: u32, _: u32) -> Result<(), i32> {
            Ok(())
        }
        fn pause(&mut self) {}
        fn detach(&mut self) {}
        fn render_poc(&mut self) -> Result<bool, i32> {
            panic!("远端链路不能调用POC")
        }
        fn reset_remote(&mut self) {
            self.receipt = None;
            self.resets += 1;
        }
        fn apply(&mut self, batch: Vec<FrameTransaction>) -> Result<(), i32> {
            for t in batch {
                let r = match t {
                    FrameTransaction::Startup { revision, .. }
                    | FrameTransaction::Revision { revision, .. } => revision,
                };
                self.revisions.push(r.revision);
                self.receipt = Some(PresentationEvent::FramePresented {
                    session_id: r.session_id,
                    generation: r.generation,
                    revision: r.revision,
                    completeness: r.completeness,
                });
            }
            Ok(())
        }
        fn render_remote(
            &mut self,
            _: TransformedViewport,
            _: bool,
        ) -> Result<Option<PresentationEvent>, i32> {
            if self.fail {
                Err(GPU_FAILED)
            } else {
                Ok(self.receipt.clone())
            }
        }
    }
    fn link() -> (Link, Epoch) {
        let link = Link::default();
        let mut s = link.0.lock().unwrap();
        s.bound = true;
        s.session = Some((SessionId::allocate(), 1));
        s.remote_size = Some(frd_core::PixelSize {
            width: 2,
            height: 2,
        });
        s.geometry = Some(ViewGeometry {
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
        });
        s.surface = Surface {
            local: 1,
            lifecycle: 1,
            width: 100,
            height: 100,
            attached: true,
            foreground: true,
        };
        let epoch = s.epoch().unwrap();
        drop(s);
        (link, epoch)
    }
    fn transaction(e: Epoch, revision: u64) -> FrameTransaction {
        let revision = FrameRevision {
            session_id: e.session,
            generation: e.remote,
            revision,
            patches: vec![],
            completeness: FrameCompleteness::FullBaseline,
        };
        FrameTransaction::Startup {
            earliest_constituent_enqueue_at: std::time::Instant::now(),
            reset: FrameReset {
                session_id: e.session,
                generation: e.remote,
                size: frd_core::PixelSize {
                    width: 2,
                    height: 2,
                },
                format: PixelFormat::Bgrx8UnormSrgb,
            },
            revision,
        }
    }
    #[test]
    fn feedback_backpressure_preserves_batch_prefix_and_stale_session_never_applies() {
        let (link, epoch) = link();
        let mut consumer = Consumer::default();
        let mut backend = Mock {
            revisions: vec![],
            receipt: None,
            fail: false,
            resets: 0,
        };
        link.0.lock().unwrap().batch = Some((epoch, vec![transaction(epoch, 1)]));
        assert!(consumer.poll(&link, &mut backend).unwrap());
        link.0.lock().unwrap().batch = Some((epoch, vec![transaction(epoch, 2)]));
        assert!(!consumer.poll(&link, &mut backend).unwrap());
        assert_eq!(backend.revisions, vec![1]);
        link.0.lock().unwrap().feedback.take();
        assert!(consumer.poll(&link, &mut backend).unwrap());
        assert_eq!(backend.revisions, vec![1, 2]);
        let mut s = link.0.lock().unwrap();
        s.feedback.take();
        s.session = Some((SessionId::allocate(), 1));
        s.batch = Some((epoch, vec![transaction(epoch, 3)]));
        drop(s);
        assert!(!consumer.poll(&link, &mut backend).unwrap());
        assert_eq!(backend.revisions, vec![1, 2]);
        assert_eq!(backend.resets, 2);
    }
    #[test]
    fn pause_keeps_owned_prefix_repaint_requires_actual_present_and_failure_is_visible() {
        let (link, epoch) = link();
        let mut consumer = Consumer::default();
        let mut backend = Mock {
            revisions: vec![],
            receipt: None,
            fail: false,
            resets: 0,
        };
        {
            let mut s = link.0.lock().unwrap();
            s.surface.foreground = false;
            s.batch = Some((epoch, vec![transaction(epoch, 1)]));
        }
        assert!(!consumer.poll(&link, &mut backend).unwrap());
        assert_eq!(backend.revisions, vec![1]);
        {
            let mut s = link.0.lock().unwrap();
            assert!(s.feedback.take().unwrap().event.is_none());
            s.surface.foreground = true;
            s.surface.lifecycle += 1;
        }
        backend.fail = true;
        assert_eq!(consumer.poll(&link, &mut backend), Err(GPU_FAILED));
        assert!(link.0.lock().unwrap().feedback.is_none());
        backend.fail = false;
        assert!(consumer.poll(&link, &mut backend).unwrap());
        assert_ne!(
            link.0.lock().unwrap().feedback.as_ref().unwrap().epoch,
            epoch
        );
    }
}
