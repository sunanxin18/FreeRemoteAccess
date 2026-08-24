//! 帧缓冲。内部每像素一个 u32（0x00RRGGBB，小端），协议适配层按需
//! 转换为统一的 BGRA GPU 上传格式。

use anyhow::{ensure, Context, Result};

use crate::vnc::client::RectOp;

pub const PIXEL_RED_SHIFT: u32 = 16;
pub const PIXEL_GREEN_SHIFT: u32 = 8;
pub const PIXEL_BLUE_SHIFT: u32 = 0;
pub const PNG_ALPHA_OPAQUE: u8 = 0xff;
pub const PNG_CHANNEL_BYTES: usize = 4;

pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> Result<Self> {
        let pixel_count = validate_framebuffer_geometry(width, height)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(pixel_count)
            .context("无法为帧缓冲预留内存")?;
        pixels.resize(pixel_count, 0u32);
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    #[cfg(feature = "media")]
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    pub fn apply(&mut self, ops: &[RectOp]) {
        for op in ops {
            match op {
                RectOp::Raw { x, y, w, h, pixels } => self.write_rect(*x, *y, *w, *h, pixels),
                RectOp::Copy { x, y, w, h, sx, sy } => self.copy_rect(*sx, *sy, *x, *y, *w, *h),
            }
        }
    }

    /// 写入一块 Raw 矩形（越界部分裁剪，防止异常数据流破坏内存）
    fn write_rect(&mut self, x: usize, y: usize, w: usize, h: usize, data: &[u32]) {
        for row in 0..h {
            let dy = y + row;
            if dy >= self.height || x >= self.width {
                continue;
            }
            let cw = w.min(self.width - x);
            let dst = dy * self.width + x;
            let src = row * w;
            self.pixels[dst..dst + cw].copy_from_slice(&data[src..src + cw]);
        }
    }

    /// CopyRect：先取出源区域再写回目标区域，天然处理二者重叠的情况
    fn copy_rect(&mut self, sx: usize, sy: usize, dx: usize, dy: usize, w: usize, h: usize) {
        let mut tmp = Vec::with_capacity(w * h);
        for row in 0..h {
            let y = sy + row;
            if y >= self.height || sx >= self.width {
                tmp.extend(std::iter::repeat_n(0, w));
                continue;
            }
            let cw = w.min(self.width - sx);
            let s = y * self.width + sx;
            tmp.extend_from_slice(&self.pixels[s..s + cw]);
            tmp.extend(std::iter::repeat_n(0, w - cw));
        }
        for row in 0..h {
            let y = dy + row;
            if y >= self.height || dx >= self.width {
                continue;
            }
            let cw = w.min(self.width - dx);
            let d = y * self.width + dx;
            let s = row * w;
            self.pixels[d..d + cw].copy_from_slice(&tmp[s..s + cw]);
        }
    }

    /// 导出为 RGBA 字节序列（保存 PNG 用）
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * PNG_CHANNEL_BYTES);
        for p in &self.pixels {
            out.extend_from_slice(&[
                (p >> PIXEL_RED_SHIFT) as u8,
                (p >> PIXEL_GREEN_SHIFT) as u8,
                (p >> PIXEL_BLUE_SHIFT) as u8,
                PNG_ALPHA_OPAQUE,
            ]);
        }
        out
    }

    /// 保存为 PNG 文件
    pub fn save_png(&self, path: &std::path::Path) -> Result<()> {
        let file =
            std::fs::File::create(path).with_context(|| format!("创建 {}", path.display()))?;
        let mut enc = png::Encoder::new(
            std::io::BufWriter::new(file),
            self.width as u32,
            self.height as u32,
        );
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().context("写 PNG 头失败")?;
        writer
            .write_image_data(&self.to_rgba())
            .context("写 PNG 像素数据失败")?;
        writer.finish().context("收尾 PNG 失败")?;
        Ok(())
    }
}

pub fn validate_framebuffer_geometry(width: usize, height: usize) -> Result<usize> {
    ensure!(width > 0 && height > 0, "帧缓冲尺寸不能为零");
    let pixel_count = width.checked_mul(height).context("帧缓冲像素数量溢出")?;
    ensure!(
        pixel_count <= crate::vnc::protocol::limits::MAX_FRAMEBUFFER_PIXELS,
        "帧缓冲尺寸超过资源预算: {width}x{height}"
    );
    Ok(pixel_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vnc::protocol::limits;

    #[test]
    fn bounded_framebuffer_constructor_rejects_remote_oom_geometry() {
        const TEST_WIDTH: usize = 64;
        const TEST_HEIGHT: usize = 32;
        assert!(Framebuffer::new(u16::MAX as usize, u16::MAX as usize).is_err());
        let allowed = Framebuffer::new(TEST_WIDTH, TEST_HEIGHT).unwrap();
        assert_eq!(allowed.pixels().len(), TEST_WIDTH * TEST_HEIGHT);
        assert_eq!(limits::RFB_BYTES_PER_PIXEL, size_of::<u32>());
    }

    #[test]
    fn framebuffer_rgba_uses_named_rgb_channel_layout_and_opaque_alpha() {
        let mut framebuffer = Framebuffer::new(1, 1).unwrap();
        framebuffer.pixels[0] = 0x0012_3456;

        assert_eq!(framebuffer.to_rgba(), [0x12, 0x34, 0x56, 0xff]);
    }
}
