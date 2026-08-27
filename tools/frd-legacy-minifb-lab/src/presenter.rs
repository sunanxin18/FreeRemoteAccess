use anyhow::{bail, Context, Result};
use frd_core::{ContentViewport, PixelSize, SessionId};
use frd_frame::{PixelFormat, SurfaceUpdate};

pub struct LegacySurface {
    session_id: Option<SessionId>,
    generation: u64,
    size: Option<PixelSize>,
    pixels: Vec<u32>,
}

impl LegacySurface {
    pub fn empty() -> Self {
        Self {
            session_id: None,
            generation: 0,
            size: None,
            pixels: Vec::new(),
        }
    }

    pub fn apply(&mut self, update: SurfaceUpdate) -> Result<bool> {
        match update {
            SurfaceUpdate::Reset {
                session_id,
                generation,
                size,
                format,
            } => {
                if format != PixelFormat::Bgrx8UnormSrgb {
                    bail!("legacy minifb lab 只接受 BGRX surface");
                }
                let pixel_count = usize::try_from(size.width)
                    .ok()
                    .and_then(|width| {
                        usize::try_from(size.height)
                            .ok()
                            .and_then(|height| width.checked_mul(height))
                    })
                    .context("legacy surface 尺寸超出本机地址空间")?;
                let generation_changed = self.session_id != Some(session_id)
                    || self.generation != generation
                    || self.size != Some(size);
                self.session_id = Some(session_id);
                self.generation = generation;
                self.size = Some(size);
                self.pixels.clear();
                self.pixels.resize(pixel_count, 0);
                Ok(generation_changed)
            }
            SurfaceUpdate::Damage {
                session_id,
                generation,
                patches,
                ..
            } => {
                if self.session_id != Some(session_id) || self.generation != generation {
                    return Ok(false);
                }
                let size = self.size.context("legacy surface 尚未 reset")?;
                for patch in patches {
                    let rect = patch.rect;
                    let stride = usize::try_from(patch.stride_bytes)
                        .context("BGRX patch stride 无法表示")?;
                    let bytes = patch.pixels.as_bytes();
                    let row_bytes = usize::try_from(rect.width)
                        .ok()
                        .and_then(|width| width.checked_mul(4))
                        .context("BGRX patch 行宽溢出")?;
                    let end_x = rect.x.checked_add(rect.width).context("patch x 溢出")?;
                    let end_y = rect.y.checked_add(rect.height).context("patch y 溢出")?;
                    if end_x > size.width || end_y > size.height || stride < row_bytes {
                        bail!("BGRX patch 超出 legacy surface");
                    }
                    for row in 0..usize::try_from(rect.height).unwrap_or(usize::MAX) {
                        let source_start = row.checked_mul(stride).context("patch offset 溢出")?;
                        let source_end = source_start
                            .checked_add(row_bytes)
                            .context("patch row end 溢出")?;
                        let source = bytes
                            .get(source_start..source_end)
                            .context("BGRX patch 像素长度不足")?;
                        let destination_start = usize::try_from(rect.y)
                            .ok()
                            .and_then(|y| y.checked_add(row))
                            .and_then(|y| y.checked_mul(size.width as usize))
                            .and_then(|offset| offset.checked_add(rect.x as usize))
                            .context("legacy surface offset 溢出")?;
                        let destination_end = destination_start
                            .checked_add(rect.width as usize)
                            .context("legacy surface row end 溢出")?;
                        let destination = self
                            .pixels
                            .get_mut(destination_start..destination_end)
                            .context("BGRX patch destination 越界")?;
                        for (pixel, bgrx) in destination.iter_mut().zip(source.chunks_exact(4)) {
                            *pixel = (u32::from(bgrx[2]) << 16)
                                | (u32::from(bgrx[1]) << 8)
                                | u32::from(bgrx[0]);
                        }
                    }
                }
                Ok(false)
            }
            SurfaceUpdate::FrameBoundary { .. } => Ok(false),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn size(&self) -> Option<PixelSize> {
        self.size
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }
}

pub fn render_nearest(surface: &LegacySurface, drawable: PixelSize, output: &mut Vec<u32>) {
    output.clear();
    output.resize(drawable.width as usize * drawable.height as usize, 0);
    let Some(remote) = surface.size else {
        return;
    };
    let viewport = ContentViewport::fit(remote, drawable);
    for y in 0..viewport.content.height as usize {
        let source_y = y * remote.height as usize / viewport.content.height as usize;
        let destination_y = viewport.content.y as usize + y;
        for x in 0..viewport.content.width as usize {
            let source_x = x * remote.width as usize / viewport.content.width as usize;
            let destination_x = viewport.content.x as usize + x;
            output[destination_y * drawable.width as usize + destination_x] =
                surface.pixels[source_y * remote.width as usize + source_x];
        }
    }
}

#[cfg(test)]
mod tests {
    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_frame::{PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};

    use super::{render_nearest, LegacySurface};

    #[test]
    fn current_generation_bgrx_damage_renders_centered_without_channel_swap() {
        let session_id = SessionId::allocate();
        let mut surface = LegacySurface::empty();
        assert!(surface
            .apply(SurfaceUpdate::Reset {
                session_id,
                generation: 1,
                size: PixelSize::new(2, 1).unwrap(),
                format: PixelFormat::Bgrx8UnormSrgb,
            })
            .unwrap());
        surface
            .apply(SurfaceUpdate::Damage {
                session_id,
                generation: 1,
                revision: 1,
                patches: vec![PixelPatch {
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 1,
                    },
                    stride_bytes: 8,
                    pixels: PixelBuffer::new(vec![0, 0, 255, 0, 0, 255, 0, 0]),
                }],
            })
            .unwrap();

        let mut output = Vec::new();
        render_nearest(&surface, PixelSize::new(4, 4).unwrap(), &mut output);

        assert_eq!(output.len(), 16);
        assert_eq!(
            &output[4..8],
            &[0x00ff_0000, 0x00ff_0000, 0x0000_ff00, 0x0000_ff00]
        );
        assert_eq!(
            &output[8..12],
            &[0x00ff_0000, 0x00ff_0000, 0x0000_ff00, 0x0000_ff00]
        );
        assert!(output[..4].iter().all(|pixel| *pixel == 0));
        assert!(output[12..].iter().all(|pixel| *pixel == 0));
    }

    #[test]
    fn stale_generation_damage_cannot_mutate_the_visible_surface() {
        let session_id = SessionId::allocate();
        let mut surface = LegacySurface::empty();
        surface
            .apply(SurfaceUpdate::Reset {
                session_id,
                generation: 2,
                size: PixelSize::new(1, 1).unwrap(),
                format: PixelFormat::Bgrx8UnormSrgb,
            })
            .unwrap();

        assert!(!surface
            .apply(SurfaceUpdate::Damage {
                session_id,
                generation: 1,
                revision: 1,
                patches: vec![PixelPatch {
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    stride_bytes: 4,
                    pixels: PixelBuffer::new(vec![255, 255, 255, 0]),
                }],
            })
            .unwrap());
        assert_eq!(surface.pixels(), &[0]);
    }
}
