use anyhow::{bail, Context, Result};
use frd_core::{PixelRect, PixelSize, SessionId};
use frd_frame::{FrameCompleteness, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};
use frd_protocol_api::{ProtocolError, ProtocolRuntime};

const FULL_SNAPSHOT_PATCH_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeMvsRenderObservability {
    pub(crate) type_zero_applied_count: u64,
    pub(crate) content_revision: u64,
    pub(crate) first_nonblack_render_revision: Option<u64>,
}

/// Apple decoder 的 generation-bound canonical CPU surface。
pub(crate) struct CpuFramebuffer {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pixels: Vec<u32>,
}

impl CpuFramebuffer {
    pub(crate) fn new(width: usize, height: usize) -> Result<Self> {
        if width == 0 || height == 0 {
            bail!("framebuffer 尺寸必须非零");
        }
        let pixel_count = width
            .checked_mul(height)
            .context("Apple surface 像素数量溢出")?;
        if pixel_count > crate::protocol::limits::MAX_FRAMEBUFFER_PIXELS {
            bail!("Apple surface 超出资源预算: {width}x{height}");
        }
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(pixel_count)
            .context("Apple surface 内存预留失败")?;
        pixels.resize(pixel_count, 0);
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub(crate) fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub(crate) fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }
}

pub(crate) struct DisplaySurface {
    pub(crate) generation: u64,
    pub(crate) framebuffer: CpuFramebuffer,
    pub(crate) native_mvs_observability: NativeMvsRenderObservability,
}

impl DisplaySurface {
    pub(crate) fn new(generation: u64, size: PixelSize) -> Result<Self> {
        if generation == 0 {
            bail!("Apple surface generation 必须非零");
        }
        Ok(Self {
            generation,
            framebuffer: CpuFramebuffer::new(size.width as usize, size.height as usize)?,
            native_mvs_observability: NativeMvsRenderObservability::default(),
        })
    }

    pub(crate) fn width(&self) -> usize {
        self.framebuffer.width
    }

    pub(crate) fn height(&self) -> usize {
        self.framebuffer.height
    }

    #[cfg(test)]
    pub(crate) fn pixels_mut(&mut self) -> &mut [u32] {
        self.framebuffer.pixels_mut()
    }

    pub(crate) fn record_native_type_zero_applied(&mut self) -> NativeMvsRenderObservability {
        self.native_mvs_observability.type_zero_applied_count = self
            .native_mvs_observability
            .type_zero_applied_count
            .saturating_add(1);
        self.native_mvs_observability.content_revision = self
            .native_mvs_observability
            .content_revision
            .saturating_add(1);
        self.native_mvs_observability
    }

    pub(crate) fn record_native_partial_applied(&mut self) -> NativeMvsRenderObservability {
        self.native_mvs_observability.content_revision = self
            .native_mvs_observability
            .content_revision
            .saturating_add(1);
        self.native_mvs_observability
    }

    fn bgrx_patch(&self, rect: PixelRect) -> Result<PixelPatch> {
        if rect.width == 0 || rect.height == 0 {
            bail!("Apple surface dirty rect 不能为空");
        }
        let right = rect
            .x
            .checked_add(rect.width)
            .context("Apple surface dirty rect x 溢出")?;
        let bottom = rect
            .y
            .checked_add(rect.height)
            .context("Apple surface dirty rect y 溢出")?;
        if right > self.width() as u32 || bottom > self.height() as u32 {
            bail!("Apple surface dirty rect 超出 surface");
        }
        let stride_bytes = rect
            .width
            .checked_mul(4)
            .context("Apple surface stride 溢出")?;
        let byte_count = usize::try_from(stride_bytes)
            .ok()
            .and_then(|stride| stride.checked_mul(rect.height as usize))
            .context("Apple surface dirty payload 溢出")?;
        let mut bytes = Vec::with_capacity(byte_count);
        let surface_width = self.width();
        let x = rect.x as usize;
        let width = rect.width as usize;
        for row in rect.y as usize..bottom as usize {
            let start = row
                .checked_mul(surface_width)
                .and_then(|offset| offset.checked_add(x))
                .context("Apple surface dirty row 溢出")?;
            for pixel in &self.framebuffer.pixels()[start..start + width] {
                bytes.extend_from_slice(&pixel.to_le_bytes());
            }
        }
        Ok(PixelPatch {
            rect,
            stride_bytes,
            pixels: PixelBuffer::new(bytes),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MvsFrameKind {
    TypeZero {
        complete_surface: bool,
        initial_nonblack: bool,
    },
    TypeOne,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicationOutcome {
    Published,
    NeedsFullBaseline,
    NeedsFullSnapshot,
    IgnoredStale,
}

pub(crate) struct AppleSurfacePublisher {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
    revision: u64,
    baseline_established: bool,
}

impl AppleSurfacePublisher {
    pub(crate) fn begin(
        runtime: &mut ProtocolRuntime,
        session_id: SessionId,
        size: PixelSize,
    ) -> Result<Self, ProtocolError> {
        runtime.begin_generation(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb)?;
        Ok(Self {
            session_id,
            generation: 1,
            size,
            revision: 0,
            baseline_established: false,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn begin_next_generation(
        &mut self,
        runtime: &mut ProtocolRuntime,
        generation: u64,
        size: PixelSize,
    ) -> Result<(), ProtocolError> {
        runtime.begin_generation(
            self.session_id,
            generation,
            size,
            PixelFormat::Bgrx8UnormSrgb,
        )?;
        self.generation = generation;
        self.size = size;
        self.revision = 0;
        self.baseline_established = false;
        Ok(())
    }

    pub(crate) fn publish_committed(
        &mut self,
        runtime: &mut ProtocolRuntime,
        surface: &DisplaySurface,
        generation: u64,
        dirty: PixelRect,
        kind: MvsFrameKind,
    ) -> Result<PublicationOutcome, ProtocolError> {
        if generation != self.generation || surface.generation != self.generation {
            return Ok(PublicationOutcome::IgnoredStale);
        }
        let completeness = match self.publication_completeness(kind) {
            Ok(completeness) => completeness,
            Err(outcome) => return Ok(outcome),
        };
        let patch = surface
            .bgrx_patch(dirty)
            .map_err(|_| ProtocolError::FramePortRejected)?;
        if completeness == FrameCompleteness::FullBaseline
            && validate_complete_baseline_patch(surface, &patch).is_err()
        {
            return Err(ProtocolError::FramePortRejected);
        }
        self.publish_patch(runtime, patch, completeness)
    }

    /// 发布已在 decoder prepare 期间构造并校验的 BGRX patch。
    /// type-0 调用方已保证 patch 与当前 surface 的 dirty rect 一致，
    /// 因而这里不得重新读取 CPU surface 打包像素。
    pub(crate) fn publish_committed_patch(
        &mut self,
        runtime: &mut ProtocolRuntime,
        surface: &DisplaySurface,
        generation: u64,
        patch: PixelPatch,
        kind: MvsFrameKind,
    ) -> Result<PublicationOutcome, ProtocolError> {
        if generation != self.generation || surface.generation != self.generation {
            return Ok(PublicationOutcome::IgnoredStale);
        }
        let completeness = match self.publication_completeness(kind) {
            Ok(completeness) => completeness,
            Err(outcome) => return Ok(outcome),
        };
        if completeness == FrameCompleteness::FullBaseline
            && validate_complete_baseline_patch(surface, &patch).is_err()
        {
            return Err(ProtocolError::FramePortRejected);
        }
        self.publish_patch(runtime, patch, completeness)
    }

    #[allow(dead_code)] // Task 2 接入 network reader 前保留为 crate 内恢复入口。
    pub(crate) fn republish_full_snapshot(
        &mut self,
        runtime: &mut ProtocolRuntime,
        surface: &DisplaySurface,
        generation: u64,
    ) -> Result<(), ProtocolError> {
        self.republish_full_snapshot_with_patch_limit(
            runtime,
            surface,
            generation,
            FULL_SNAPSHOT_PATCH_BYTES,
        )
    }

    #[allow(dead_code)] // 生产入口仅使用固定预算，测试以较小预算验证分段。
    fn republish_full_snapshot_with_patch_limit(
        &mut self,
        runtime: &mut ProtocolRuntime,
        surface: &DisplaySurface,
        generation: u64,
        patch_byte_limit: usize,
    ) -> Result<(), ProtocolError> {
        if generation != self.generation
            || surface.generation != self.generation
            || surface.width() != self.size.width as usize
            || surface.height() != self.size.height as usize
        {
            return Err(ProtocolError::FramePortRejected);
        }

        let width = surface.width();
        let row_bytes = width
            .checked_mul(4)
            .ok_or(ProtocolError::FramePortRejected)?;
        if patch_byte_limit < row_bytes {
            return Err(ProtocolError::FramePortRejected);
        }
        let rows_per_patch = patch_byte_limit / row_bytes;

        self.baseline_established = false;
        let mut y = 0usize;
        while y < surface.height() {
            let band_height = rows_per_patch.min(surface.height() - y);
            let patch = surface
                .bgrx_patch(PixelRect {
                    x: 0,
                    y: u32::try_from(y).map_err(|_| ProtocolError::FramePortRejected)?,
                    width: u32::try_from(width).map_err(|_| ProtocolError::FramePortRejected)?,
                    height: u32::try_from(band_height)
                        .map_err(|_| ProtocolError::FramePortRejected)?,
                })
                .map_err(|_| ProtocolError::FramePortRejected)?;
            let completeness = if y + band_height == surface.height() {
                FrameCompleteness::FullBaseline
            } else {
                FrameCompleteness::Incremental
            };
            match self.publish_patch(runtime, patch, completeness)? {
                PublicationOutcome::Published => {}
                PublicationOutcome::NeedsFullSnapshot => {
                    return Err(ProtocolError::FramePortRejected);
                }
                PublicationOutcome::NeedsFullBaseline | PublicationOutcome::IgnoredStale => {
                    return Err(ProtocolError::FramePortRejected);
                }
            }
            y = y
                .checked_add(band_height)
                .ok_or(ProtocolError::FramePortRejected)?;
        }

        Ok(())
    }

    fn publication_completeness(
        &self,
        kind: MvsFrameKind,
    ) -> std::result::Result<FrameCompleteness, PublicationOutcome> {
        match kind {
            MvsFrameKind::TypeZero {
                complete_surface: true,
                initial_nonblack: true,
            } => Ok(FrameCompleteness::FullBaseline),
            MvsFrameKind::TypeZero {
                complete_surface: true,
                initial_nonblack: false,
            } => Err(PublicationOutcome::NeedsFullBaseline),
            MvsFrameKind::TypeZero {
                complete_surface: false,
                ..
            } if !self.baseline_established => Err(PublicationOutcome::NeedsFullBaseline),
            MvsFrameKind::TypeZero {
                complete_surface: false,
                ..
            } => Ok(FrameCompleteness::Incremental),
            MvsFrameKind::TypeOne if !self.baseline_established => {
                Err(PublicationOutcome::NeedsFullBaseline)
            }
            MvsFrameKind::TypeOne => Ok(FrameCompleteness::Incremental),
        }
    }

    fn publish_patch(
        &mut self,
        runtime: &mut ProtocolRuntime,
        patch: PixelPatch,
        completeness: FrameCompleteness,
    ) -> Result<PublicationOutcome, ProtocolError> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(ProtocolError::FramePortRejected)?;
        match runtime.publish_surface(SurfaceUpdate::Damage {
            session_id: self.session_id,
            generation: self.generation,
            revision,
            patches: vec![patch],
        }) {
            Ok(()) => {}
            Err(ProtocolError::NeedsFullSnapshot) => {
                return Ok(PublicationOutcome::NeedsFullSnapshot);
            }
            Err(error) => return Err(error),
        }
        match runtime.publish_surface(SurfaceUpdate::FrameBoundary {
            session_id: self.session_id,
            generation: self.generation,
            revision,
            completeness,
        }) {
            Ok(()) => {}
            Err(ProtocolError::NeedsFullSnapshot) => {
                // Damage 已被 mailbox 接受并推进其 last_damage_revision；
                // overflow 清队列但不会回滚生命周期。下一次 full recovery
                // 必须使用更高 revision，同时本次不能建立 baseline。
                self.revision = revision;
                return Ok(PublicationOutcome::NeedsFullSnapshot);
            }
            Err(error) => return Err(error),
        }
        self.revision = revision;
        if completeness == FrameCompleteness::FullBaseline {
            self.baseline_established = true;
        }
        Ok(PublicationOutcome::Published)
    }
}

fn validate_complete_baseline_patch(surface: &DisplaySurface, patch: &PixelPatch) -> Result<()> {
    let width = u32::try_from(surface.width()).context("Apple complete baseline 宽度溢出")?;
    let height = u32::try_from(surface.height()).context("Apple complete baseline 高度溢出")?;
    if patch.rect
        != (PixelRect {
            x: 0,
            y: 0,
            width,
            height,
        })
    {
        bail!("Apple complete baseline patch 必须精确覆盖 surface");
    }
    let expected_stride = width
        .checked_mul(4)
        .context("Apple complete baseline stride 溢出")?;
    if patch.stride_bytes != expected_stride {
        bail!("Apple complete baseline patch stride 不匹配");
    }
    let expected_len = usize::try_from(expected_stride)
        .ok()
        .and_then(|stride| stride.checked_mul(height as usize))
        .context("Apple complete baseline payload 溢出")?;
    if patch.pixels.len() != expected_len {
        bail!("Apple complete baseline patch payload 长度不匹配");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn apply_rgb_rect_for_generation(
    surface: &mut DisplaySurface,
    generation: u64,
    rgb: &[u8],
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Result<bool> {
    if surface.generation != generation {
        return Ok(false);
    }
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .context("RGB 矩形尺寸溢出")?;
    if rgb.len() != expected {
        bail!("RGB 数据长度不匹配: 期望 {expected}, 实际 {}", rgb.len());
    }
    if x.checked_add(width)
        .is_none_or(|right| right > surface.width())
        || y.checked_add(height)
            .is_none_or(|bottom| bottom > surface.height())
    {
        bail!("RGB 矩形超出 framebuffer");
    }
    let surface_width = surface.width();
    for row in 0..height {
        for column in 0..width {
            let source = (row * width + column) * 3;
            let destination = (y + row) * surface_width + x + column;
            let red = u32::from(rgb[source]);
            let green = u32::from(rgb[source + 1]);
            let blue = u32::from(rgb[source + 2]);
            surface.pixels_mut()[destination] = (red << 16) | (green << 8) | blue;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};

    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_frame::{
        FrameCompleteness, FrameMailbox, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate,
    };
    use frd_protocol_api::{
        MailboxSurfacePublisher, ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake,
        SessionEvent, SurfacePublisher,
    };

    use super::{
        apply_rgb_rect_for_generation, AppleSurfacePublisher, DisplaySurface, MvsFrameKind,
        PublicationOutcome,
    };

    struct NoopEvents;

    impl RuntimeEventSink for NoopEvents {
        fn publish(&self, _event: SessionEvent) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct RecordingFrames(Arc<Mutex<Vec<SurfaceUpdate>>>);

    impl SurfacePublisher for RecordingFrames {
        fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
            self.0.lock().expect("frame log lock").push(update);
            Ok(())
        }
    }

    struct NoopWake;

    impl RuntimeWake for NoopWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    fn runtime_with_frames(
        session_id: SessionId,
    ) -> (ProtocolRuntime, Arc<Mutex<Vec<SurfaceUpdate>>>) {
        let (_commands, command_rx) = mpsc::channel();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(RecordingFrames(frames.clone())),
            None,
            Box::new(NoopWake),
        );
        (runtime, frames)
    }

    #[test]
    fn type_one_cannot_publish_full_baseline_before_complete_type_zero() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 1).unwrap();
        let (mut runtime, frames) = runtime_with_frames(session_id);
        let mut surface = DisplaySurface::new(1, size).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();
        apply_rgb_rect_for_generation(&mut surface, 1, &[1, 2, 3], 0, 0, 1, 1).unwrap();

        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    MvsFrameKind::TypeOne,
                )
                .unwrap(),
            PublicationOutcome::NeedsFullBaseline
        );

        assert!(!frames.lock().unwrap().iter().any(|update| matches!(
            update,
            SurfaceUpdate::FrameBoundary {
                completeness: FrameCompleteness::FullBaseline,
                ..
            }
        )));
    }

    #[test]
    fn stale_and_missing_baseline_short_circuit_before_invalid_dirty_patch_build() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(1, 1).unwrap();
        let (mut runtime, _frames) = runtime_with_frames(session_id);
        let surface = DisplaySurface::new(1, size).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();
        let invalid = PixelRect {
            x: 1,
            y: 0,
            width: 1,
            height: 1,
        };

        assert_eq!(
            publisher
                .publish_committed(&mut runtime, &surface, 2, invalid, MvsFrameKind::TypeOne)
                .unwrap(),
            PublicationOutcome::IgnoredStale
        );
        assert_eq!(
            publisher
                .publish_committed(&mut runtime, &surface, 1, invalid, MvsFrameKind::TypeOne)
                .unwrap(),
            PublicationOutcome::NeedsFullBaseline
        );
    }

    #[test]
    fn complete_type_zero_patch_rejects_invalid_full_baseline_layout_before_publication() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 1).unwrap();
        let invalid_patches = [
            PixelPatch {
                rect: PixelRect {
                    x: 1,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                stride_bytes: 4,
                pixels: PixelBuffer::new(vec![0x33, 0x22, 0x11, 0]),
            },
            PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                stride_bytes: 4,
                pixels: PixelBuffer::new(vec![0x33, 0x22, 0x11, 0, 0x66, 0x55, 0x44, 0]),
            },
            PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                stride_bytes: 8,
                pixels: PixelBuffer::new(vec![0x33, 0x22, 0x11, 0]),
            },
        ];

        for patch in invalid_patches {
            let (mut runtime, frames) = runtime_with_frames(session_id);
            let surface = DisplaySurface::new(1, size).unwrap();
            let mut publisher =
                AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();

            assert!(matches!(
                publisher.publish_committed_patch(
                    &mut runtime,
                    &surface,
                    1,
                    patch,
                    MvsFrameKind::TypeZero {
                        complete_surface: true,
                        initial_nonblack: true,
                    },
                ),
                Err(ProtocolError::FramePortRejected)
            ));
            assert_eq!(
                frames.lock().unwrap().len(),
                1,
                "invalid baseline must not publish damage"
            );
            assert_eq!(
                publisher
                    .publish_committed(
                        &mut runtime,
                        &surface,
                        1,
                        PixelRect {
                            x: 0,
                            y: 0,
                            width: 1,
                            height: 1,
                        },
                        MvsFrameKind::TypeOne,
                    )
                    .unwrap(),
                PublicationOutcome::NeedsFullBaseline
            );
        }

        let (mut runtime, frames) = runtime_with_frames(session_id);
        let surface = DisplaySurface::new(1, size).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();
        assert!(matches!(
            publisher.publish_committed(
                &mut runtime,
                &surface,
                1,
                PixelRect {
                    x: 1,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                MvsFrameKind::TypeZero {
                    complete_surface: true,
                    initial_nonblack: true,
                },
            ),
            Err(ProtocolError::FramePortRejected)
        ));
        assert_eq!(frames.lock().unwrap().len(), 1);
    }

    #[test]
    fn direct_local_type_zero_patch_publishes_owned_bgrx_without_rereading_surface() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 1).unwrap();
        let (mut runtime, frames) = runtime_with_frames(session_id);
        let surface = DisplaySurface::new(1, size).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();
        let full_patch = PixelPatch {
            rect: PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            },
            stride_bytes: 8,
            pixels: PixelBuffer::new(vec![0x66, 0x55, 0x44, 0, 0x33, 0x22, 0x11, 0]),
        };
        assert_eq!(
            publisher
                .publish_committed_patch(
                    &mut runtime,
                    &surface,
                    1,
                    full_patch,
                    MvsFrameKind::TypeZero {
                        complete_surface: true,
                        initial_nonblack: true,
                    },
                )
                .unwrap(),
            PublicationOutcome::Published
        );
        let local_patch = PixelPatch {
            rect: PixelRect {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
            },
            stride_bytes: 4,
            pixels: PixelBuffer::new(vec![0x33, 0x22, 0x11, 0]),
        };

        assert_eq!(
            publisher
                .publish_committed_patch(
                    &mut runtime,
                    &surface,
                    1,
                    local_patch,
                    MvsFrameKind::TypeZero {
                        complete_surface: false,
                        initial_nonblack: false,
                    },
                )
                .unwrap(),
            PublicationOutcome::Published
        );

        let frames = frames.lock().unwrap();
        let SurfaceUpdate::Damage { ref patches, .. } = frames[3] else {
            panic!("local type-0 must publish its prepared patch");
        };
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].rect.x, 1);
        assert_eq!(patches[0].rect.width, 1);
        assert_eq!(patches[0].stride_bytes, 4);
        assert_eq!(patches[0].pixels.as_bytes(), &[0x33, 0x22, 0x11, 0]);
    }

    #[test]
    fn publication_orders_initial_reset_full_bgrx_then_incremental_and_drops_stale() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 1).unwrap();
        let (mut runtime, frames) = runtime_with_frames(session_id);
        let mut surface = DisplaySurface::new(1, size).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();
        apply_rgb_rect_for_generation(
            &mut surface,
            1,
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            0,
            0,
            2,
            1,
        )
        .unwrap();
        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 1,
                    },
                    MvsFrameKind::TypeZero {
                        complete_surface: true,
                        initial_nonblack: true,
                    },
                )
                .unwrap(),
            PublicationOutcome::Published
        );
        apply_rgb_rect_for_generation(&mut surface, 1, &[0xaa, 0xbb, 0xcc], 1, 0, 1, 1).unwrap();
        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 1,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    MvsFrameKind::TypeOne,
                )
                .unwrap(),
            PublicationOutcome::Published
        );
        let next_size = PixelSize::new(1, 1).unwrap();
        publisher
            .begin_next_generation(&mut runtime, 2, next_size)
            .unwrap();
        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    MvsFrameKind::TypeOne,
                )
                .unwrap(),
            PublicationOutcome::IgnoredStale
        );

        let frames = frames.lock().unwrap();
        assert!(matches!(
            frames[0],
            SurfaceUpdate::Reset {
                generation: 1,
                format: PixelFormat::Bgrx8UnormSrgb,
                ..
            }
        ));
        let SurfaceUpdate::Damage {
            revision: 1,
            ref patches,
            ..
        } = frames[1]
        else {
            panic!("complete type-0 must publish damage first");
        };
        assert_eq!(patches.len(), 1);
        assert_eq!(
            patches[0].pixels.as_bytes(),
            &[0x33, 0x22, 0x11, 0, 0x66, 0x55, 0x44, 0]
        );
        assert!(matches!(
            frames[2],
            SurfaceUpdate::FrameBoundary {
                revision: 1,
                completeness: FrameCompleteness::FullBaseline,
                ..
            }
        ));
        let SurfaceUpdate::Damage {
            revision: 2,
            ref patches,
            ..
        } = frames[3]
        else {
            panic!("type-1 must publish dirty damage");
        };
        assert_eq!(patches[0].rect.width, 1);
        assert_eq!(patches[0].pixels.as_bytes(), &[0xcc, 0xbb, 0xaa, 0]);
        assert!(matches!(
            frames[4],
            SurfaceUpdate::FrameBoundary {
                revision: 2,
                completeness: FrameCompleteness::Incremental,
                ..
            }
        ));
        assert!(matches!(
            frames[5],
            SurfaceUpdate::Reset { generation: 2, .. }
        ));
        assert_eq!(frames.len(), 6, "stale generation must publish nothing");
    }

    #[test]
    fn boundary_overflow_consumes_damage_revision_but_full_baseline_recovers_higher() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(1, 1).unwrap();
        let (_commands, command_rx) = mpsc::channel();
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(2, 4)));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut surface = DisplaySurface::new(1, size).unwrap();
        apply_rgb_rect_for_generation(&mut surface, 1, &[0x11, 0x22, 0x33], 0, 0, 1, 1).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();
        let full = MvsFrameKind::TypeZero {
            complete_surface: true,
            initial_nonblack: true,
        };

        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    full,
                )
                .unwrap(),
            PublicationOutcome::NeedsFullSnapshot
        );
        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    MvsFrameKind::TypeOne,
                )
                .unwrap(),
            PublicationOutcome::NeedsFullBaseline,
            "an unqueued boundary must not establish the baseline"
        );
        assert!(matches!(
            mailbox.lock().unwrap().pop(),
            Some(SurfaceUpdate::Reset { .. })
        ));

        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    full,
                )
                .unwrap(),
            PublicationOutcome::Published
        );
        let mut mailbox = mailbox.lock().unwrap();
        assert!(matches!(
            mailbox.pop(),
            Some(SurfaceUpdate::Damage { revision: 2, .. })
        ));
        assert!(matches!(
            mailbox.pop(),
            Some(SurfaceUpdate::FrameBoundary {
                revision: 2,
                completeness: FrameCompleteness::FullBaseline,
                ..
            })
        ));
    }

    fn seed_distinct_rows(surface: &mut DisplaySurface) {
        surface.pixels_mut().copy_from_slice(&[
            0x0011_2233,
            0x0044_5566,
            0x0077_8899,
            0x00aa_bbcc,
            0x0012_3456,
            0x0065_4321,
            0x00de_adbe,
            0x00fe_dcba,
            0x0001_0203,
            0x0004_0506,
            0x0007_0809,
            0x000a_0b0c,
            0x00c0_ffee,
            0x00fa_ce00,
            0x000b_adf0,
            0x0013_3713,
        ]);
    }

    #[test]
    fn damage_overflow_republishes_latest_canonical_bgrx() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 1).unwrap();
        let (_commands, command_rx) = mpsc::channel();
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(3, 8)));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut surface = DisplaySurface::new(1, size).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();
        let full = MvsFrameKind::TypeZero {
            complete_surface: true,
            initial_nonblack: true,
        };

        apply_rgb_rect_for_generation(
            &mut surface,
            1,
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            0,
            0,
            2,
            1,
        )
        .unwrap();
        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 1,
                    },
                    full,
                )
                .unwrap(),
            PublicationOutcome::Published
        );

        apply_rgb_rect_for_generation(&mut surface, 1, &[0xaa, 0xbb, 0xcc], 1, 0, 1, 1).unwrap();
        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 1,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    MvsFrameKind::TypeOne,
                )
                .unwrap(),
            PublicationOutcome::NeedsFullSnapshot
        );

        publisher
            .republish_full_snapshot(&mut runtime, &surface, 1)
            .unwrap();

        let mut mailbox = mailbox.lock().unwrap();
        assert!(matches!(mailbox.pop(), Some(SurfaceUpdate::Reset { .. })));
        let Some(SurfaceUpdate::Damage {
            revision: 2,
            patches,
            ..
        }) = mailbox.pop()
        else {
            panic!("recovery must publish revision-2 damage");
        };
        assert_eq!(patches.len(), 1);
        assert_eq!(
            patches[0].pixels.as_bytes(),
            &[0x33, 0x22, 0x11, 0, 0xcc, 0xbb, 0xaa, 0]
        );
        assert!(matches!(
            mailbox.pop(),
            Some(SurfaceUpdate::FrameBoundary {
                revision: 2,
                completeness: FrameCompleteness::FullBaseline,
                ..
            })
        ));
    }

    #[test]
    fn boundary_overflow_advances_recovery_revision() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(1, 1).unwrap();
        let (_commands, command_rx) = mpsc::channel();
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(2, 4)));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut surface = DisplaySurface::new(1, size).unwrap();
        apply_rgb_rect_for_generation(&mut surface, 1, &[0x11, 0x22, 0x33], 0, 0, 1, 1).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();
        let full = MvsFrameKind::TypeZero {
            complete_surface: true,
            initial_nonblack: true,
        };

        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    full,
                )
                .unwrap(),
            PublicationOutcome::NeedsFullSnapshot
        );
        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    full,
                )
                .unwrap(),
            PublicationOutcome::NeedsFullSnapshot
        );
        assert!(matches!(
            mailbox.lock().unwrap().pop(),
            Some(SurfaceUpdate::Reset { .. })
        ));

        publisher
            .republish_full_snapshot(&mut runtime, &surface, 1)
            .unwrap();

        let mut mailbox = mailbox.lock().unwrap();
        assert!(matches!(
            mailbox.pop(),
            Some(SurfaceUpdate::Damage { revision: 3, .. })
        ));
        assert!(matches!(
            mailbox.pop(),
            Some(SurfaceUpdate::FrameBoundary {
                revision: 3,
                completeness: FrameCompleteness::FullBaseline,
                ..
            })
        ));
    }

    #[test]
    fn full_snapshot_recovery_bands_rows_and_marks_only_the_end_full() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(4, 4).unwrap();
        let (mut runtime, frames) = runtime_with_frames(session_id);
        let mut surface = DisplaySurface::new(1, size).unwrap();
        seed_distinct_rows(&mut surface);
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();

        publisher
            .republish_full_snapshot_with_patch_limit(&mut runtime, &surface, 1, 16)
            .unwrap();

        let frames = frames.lock().unwrap();
        assert_eq!(frames.len(), 9);
        let mut bgrx = Vec::new();
        for row in 0..4 {
            let SurfaceUpdate::Damage {
                revision,
                ref patches,
                ..
            } = frames[row * 2 + 1]
            else {
                panic!("recovery band must begin with damage");
            };
            assert_eq!(revision, row as u64 + 1);
            assert_eq!(patches.len(), 1);
            assert_eq!(patches[0].rect.x, 0);
            assert_eq!(patches[0].rect.y, row as u32);
            assert_eq!(patches[0].rect.width, 4);
            assert_eq!(patches[0].rect.height, 1);
            bgrx.extend_from_slice(patches[0].pixels.as_bytes());

            let SurfaceUpdate::FrameBoundary {
                revision,
                completeness,
                ..
            } = frames[row * 2 + 2]
            else {
                panic!("recovery band must end with a boundary");
            };
            assert_eq!(revision, row as u64 + 1);
            assert_eq!(
                completeness,
                if row == 3 {
                    FrameCompleteness::FullBaseline
                } else {
                    FrameCompleteness::Incremental
                }
            );
        }
        assert_eq!(
            bgrx,
            vec![
                0x33, 0x22, 0x11, 0, 0x66, 0x55, 0x44, 0, 0x99, 0x88, 0x77, 0, 0xcc, 0xbb, 0xaa, 0,
                0x56, 0x34, 0x12, 0, 0x21, 0x43, 0x65, 0, 0xbe, 0xad, 0xde, 0, 0xba, 0xdc, 0xfe, 0,
                0x03, 0x02, 0x01, 0, 0x06, 0x05, 0x04, 0, 0x09, 0x08, 0x07, 0, 0x0c, 0x0b, 0x0a, 0,
                0xee, 0xff, 0xc0, 0, 0x00, 0xce, 0xfa, 0, 0xf0, 0xad, 0x0b, 0, 0x13, 0x37, 0x13, 0,
            ]
        );
    }

    #[test]
    fn full_snapshot_recovery_rejects_a_limit_smaller_than_one_row() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(4, 4).unwrap();
        let (mut runtime, _frames) = runtime_with_frames(session_id);
        let surface = DisplaySurface::new(1, size).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();

        let error = publisher
            .republish_full_snapshot_with_patch_limit(&mut runtime, &surface, 1, 15)
            .unwrap_err();
        assert_eq!(error, ProtocolError::FramePortRejected);
    }

    #[test]
    fn full_snapshot_recovery_rejects_same_generation_geometry_mismatch_before_publication() {
        let session_id = SessionId::allocate();
        let publisher_size = PixelSize::new(2, 1).unwrap();
        let (mut runtime, frames) = runtime_with_frames(session_id);
        let surface = DisplaySurface::new(1, PixelSize::new(1, 1).unwrap()).unwrap();
        let mut publisher =
            AppleSurfacePublisher::begin(&mut runtime, session_id, publisher_size).unwrap();

        assert_eq!(
            publisher
                .republish_full_snapshot(&mut runtime, &surface, 1)
                .unwrap_err(),
            ProtocolError::FramePortRejected
        );
        let frames = frames.lock().unwrap();
        assert!(matches!(frames.as_slice(), [SurfaceUpdate::Reset { .. }]));
    }

    #[test]
    fn full_snapshot_recovery_second_overflow_fails_without_retry_or_baseline() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(1, 1).unwrap();
        let (_commands, command_rx) = mpsc::channel();
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(2, 4)));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut surface = DisplaySurface::new(1, size).unwrap();
        apply_rgb_rect_for_generation(&mut surface, 1, &[0x11, 0x22, 0x33], 0, 0, 1, 1).unwrap();
        let mut publisher = AppleSurfacePublisher::begin(&mut runtime, session_id, size).unwrap();
        let full = MvsFrameKind::TypeZero {
            complete_surface: true,
            initial_nonblack: true,
        };

        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    full,
                )
                .unwrap(),
            PublicationOutcome::NeedsFullSnapshot
        );

        assert_eq!(
            publisher
                .republish_full_snapshot(&mut runtime, &surface, 1)
                .unwrap_err(),
            ProtocolError::FramePortRejected
        );
        let mut mailbox = mailbox.lock().unwrap();
        assert!(matches!(mailbox.pop(), Some(SurfaceUpdate::Reset { .. })));
        assert!(
            mailbox.pop().is_none(),
            "recovery must not recursively retry"
        );
        drop(mailbox);

        assert_eq!(
            publisher
                .publish_committed(
                    &mut runtime,
                    &surface,
                    1,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    MvsFrameKind::TypeOne,
                )
                .unwrap(),
            PublicationOutcome::NeedsFullBaseline
        );
    }
}
