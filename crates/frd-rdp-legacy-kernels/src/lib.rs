//! 受检查的 RemoteFX 数值、packed Bitmap run 与 RDP6 planar 叶子，不解析协议，也不代表 codec/session 就绪。
//! 仅 AArch64 NEON 或非 macOS x86/x86_64 SSE2；没有 scalar 后备。
//! 所有错误均在修改调用方输出之前返回。参见 README.md 的算术契约。

#[cfg(any(
    target_arch = "aarch64",
    all(
        not(target_os = "macos"),
        any(target_arch = "x86", target_arch = "x86_64")
    )
))]
mod native;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Neon,
    Sse2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelError {
    Unavailable,
    Length,
    InvalidQuant,
    ColorOverflow,
    InvalidDimensions,
    InvalidColorLossLevel,
    MissingPalette,
}
impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "当前 CPU 或平台不支持原生叶子",
            Self::Length => "缓冲区长度不符合叶子契约",
            Self::InvalidQuant => "量化值必须在 0..=16 范围",
            Self::ColorOverflow => "色彩转换的上游算术中间值溢出",
            Self::InvalidDimensions => "平面尺寸为零或尺寸算术溢出",
            Self::InvalidColorLossLevel => "色彩损失级别必须在 1..=7 范围",
            Self::MissingPalette => "索引像素行缺少已归一的 RGB 调色板",
        })
    }
}
impl std::error::Error for KernelError {}

/// 以缓冲区顺序 HL1,LH1,HH1,HL2,LH2,HH2,HL3,LH3,HH3,LL3 映射。
#[derive(Clone, Copy, Debug)]
pub struct Quantization(pub [u8; 10]);

/// packed little-endian 原始像素宽度；555 与565保留各自的wire值和所有原始位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackedDepth {
    Index8,
    Rgb555,
    Rgb565,
    Bgr24,
}

impl PackedDepth {
    fn bytes(self) -> usize {
        match self {
            Self::Index8 => 1,
            Self::Rgb555 | Self::Rgb565 => 2,
            Self::Bgr24 => 3,
        }
    }
    fn check(self, pixel: &[u8], dst: &[u8]) -> Result<(), KernelError> {
        if pixel.len() != self.bytes() || !dst.len().is_multiple_of(self.bytes()) {
            return Err(KernelError::Length);
        }
        Ok(())
    }
}

/// 名称表示内存字节顺序；X 格式的 padding 行为由源模式决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pixel32Format {
    ARgb,
    XRgb,
    ABgr,
    XBgr,
    BgrA,
    BgrX,
    RgbA,
    RgbX,
}
impl Pixel32Format {
    fn offsets(self) -> [u8; 4] {
        match self {
            Self::ARgb | Self::XRgb => [1, 2, 3, 0],
            Self::ABgr | Self::XBgr => [3, 2, 1, 0],
            Self::BgrA | Self::BgrX => [2, 1, 0, 3],
            Self::RgbA | Self::RgbX => [0, 1, 2, 3],
        }
    }
    fn has_alpha(self) -> bool {
        matches!(self, Self::ARgb | Self::ABgr | Self::BgrA | Self::RgbA)
    }
}
/// Raw32 保留原 bitmap alpha 规则；Region32 保留原 tile copy/read/write 规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelRowSource {
    Packed(PackedDepth),
    Rgb24,
    Raw32(Pixel32Format),
    Region32(Pixel32Format),
}
impl PixelRowSource {
    fn bytes(self) -> usize {
        match self {
            Self::Packed(depth) => depth.bytes(),
            Self::Rgb24 => 3,
            Self::Raw32(_) | Self::Region32(_) => 4,
        }
    }
}

/// RDP6 YCoCg 布局；alpha flag 决定输出通道顺序，不消费 alpha plane。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanarLayout {
    pub width: usize,
    pub height: usize,
    pub subsampled: bool,
    pub color_loss_level: u8,
    pub has_alpha: bool,
}

/// Cursor XOR 的封闭字节布局；RGBA 输出固定，不接受 framebuffer 通道偏移。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorXorFormat {
    Mono1,
    Rgb565,
    Bgr24,
    Bgra32,
}
impl CursorXorFormat {
    fn data_bytes(self, pixels: usize) -> Option<usize> {
        match self {
            Self::Mono1 => Some(pixels.div_ceil(8)),
            Self::Rgb565 => pixels.checked_mul(2),
            Self::Bgr24 => pixels.checked_mul(3),
            Self::Bgra32 => pixels.checked_mul(4),
        }
    }
}
/// Accelerated 的奇偶行来自输出行；row 叶子从 column 0 开始。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorBitmapMode {
    Software,
    Accelerated { odd_row: bool },
}

type CursorDecodeKernel =
    unsafe fn(CursorXorFormat, CursorBitmapMode, &[u8], Option<&[u8]>, &mut [u8]);
type CursorCompositeKernel = unsafe fn(&[u8], &[u8], &mut [u8]);

type XorKernel = unsafe fn(&[u8], &[u8], &mut [u8]);
type MaskKernel = unsafe fn(Option<&[u8]>, &[u8], u8, u8, &mut [u8]);
type DitherKernel = unsafe fn(&[u8], &[u8], bool, &mut [u8]);
type RgbPlanarKernel = unsafe fn(&[u8], &[u8], &[u8], &mut [u8]);
type YcocgKernel = unsafe fn(PlanarLayout, &[u8], &[u8], &[u8], &mut [u8]);
type ColorKernel = unsafe fn(&[i16], &[i16], &[i16], &mut [u8]) -> Result<(), KernelError>;
type PixelRowKernel = unsafe fn(
    PixelRowSource,
    Pixel32Format,
    &[u8],
    Option<&[[u8; 3]; 256]>,
    &mut [u8],
) -> Result<(), KernelError>;

/// 构造即验证 CPU；私有函数表不能由调用者伪造，也不能启用 scalar。
#[derive(Clone, Copy)]
pub struct NativeKernels {
    backend: Backend,
    dequant: unsafe fn(&mut [i16], &[u8; 10]),
    dwt: unsafe fn(&mut [i16], &mut [i16]),
    prefix: unsafe fn(&mut [i16]),
    zero: unsafe fn(&mut [i16]),
    color: ColorKernel,
    copy: unsafe fn(&[u8], &mut [u8]),
    fill: unsafe fn(&[u8], &mut [u8]),
    xor_previous: XorKernel,
    mask_previous: MaskKernel,
    dither: DitherKernel,
    planar_delta: unsafe fn(&[u8], &mut [u8]),
    planar_rgb: RgbPlanarKernel,
    planar_ycocg: YcocgKernel,
    pixel_row: PixelRowKernel,
    cursor_decode: CursorDecodeKernel,
    cursor_composite: CursorCompositeKernel,
}
impl NativeKernels {
    /// 精确 XOR/AND 数据行展开为 RGBA；Software 保留原版 c*c>>8 数值语义。
    /// 所有长度先检验；空输出只接受空源，不消费 padding，不分配。
    pub fn decode_cursor_row(
        &self,
        format: CursorXorFormat,
        mode: CursorBitmapMode,
        xor: &[u8],
        and: Option<&[u8]>,
        rgba: &mut [u8],
    ) -> Result<(), KernelError> {
        if !rgba.len().is_multiple_of(4) {
            return Err(KernelError::Length);
        }
        let pixels = rgba.len() / 4;
        if format.data_bytes(pixels) != Some(xor.len())
            || and.is_some_and(|mask| mask.len() != pixels.div_ceil(8))
        {
            return Err(KernelError::Length);
        }
        // SAFETY: 真实 detect 验证 ISA；精确行长度及 checked arithmetic 先于任何写入。
        unsafe { (self.cursor_decode)(format, mode, xor, and, rgba) };
        Ok(())
    }
    /// 原版 release 的 byte 合成：普通 RGB 加法显式模 256；保留背景 byte3。
    /// 合法 byte 行是 total，不返回 ColorOverflow；invert sentinel 将 byte3 设 255。
    pub fn composite_cursor_row(
        &self,
        rgba: &[u8],
        background: &[u8],
        output: &mut [u8],
    ) -> Result<(), KernelError> {
        if !rgba.len().is_multiple_of(4)
            || rgba.len() != background.len()
            || rgba.len() != output.len()
        {
            return Err(KernelError::Length);
        }
        // SAFETY: 真实 ISA、完整像素、相等 slice 长度与不重叠借用已验证。
        unsafe { (self.cursor_composite)(rgba, background, output) };
        Ok(())
    }
    /// 转换恰好一行；不消费 padding、stride、rect 或协议状态，不分配内存。
    pub fn convert_pixel_row(
        &self,
        source_format: PixelRowSource,
        destination_format: Pixel32Format,
        source: &[u8],
        palette: Option<&[[u8; 3]; 256]>,
        destination: &mut [u8],
    ) -> Result<(), KernelError> {
        let width = source_format.bytes();
        if !source.len().is_multiple_of(width)
            || (source.len() / width).checked_mul(4) != Some(destination.len())
        {
            return Err(KernelError::Length);
        }
        if source_format == PixelRowSource::Packed(PackedDepth::Index8) && palette.is_none() {
            return Err(KernelError::MissingPalette);
        }
        // 原版 raw 同枚举及 region 同字节序分支是真正的整行copy，保留 X padding。
        // 复用已经检查 ISA 的 bulk copy；不逐四像素重复执行格式分派。
        let raw_copy = match source_format {
            PixelRowSource::Raw32(src) => src == destination_format,
            PixelRowSource::Region32(src) => src.offsets() == destination_format.offsets(),
            _ => false,
        };
        if raw_copy {
            return self.copy_u8(source, destination);
        }
        // SAFETY: 构造器验证 ISA；完整像素、精确输出长度和 Index8 palette 先于任何写入检查。
        unsafe {
            (self.pixel_row)(
                source_format,
                destination_format,
                source,
                palette,
                destination,
            )
        }
    }
    /// 原始字节精确搬运；任意长度含空，不要求 padding 或对齐。
    pub fn copy_u8(&self, src: &[u8], dst: &mut [u8]) -> Result<(), KernelError> {
        if src.len() != dst.len() {
            return Err(KernelError::Length);
        }
        // SAFETY: 能力对象已验证 ISA；输入输出长度相同且借用不重叠。
        unsafe { (self.copy)(src, dst) };
        Ok(())
    }
    /// 重复 packed pixel，不修改 15-bit 原始高位；不决定 WHITE 的 wire 值。
    pub fn fill_pixel(
        &self,
        depth: PackedDepth,
        pixel_le: &[u8],
        dst: &mut [u8],
    ) -> Result<(), KernelError> {
        depth.check(pixel_le, dst)?;
        // SAFETY: ISA、pixel 尺寸及完整目标 pixel 数已检查。
        unsafe { (self.fill)(pixel_le, dst) };
        Ok(())
    }
    /// previous XOR 同一个 packed FG；调用方限定上一行依赖的有效范围。
    pub fn xor_previous(
        &self,
        depth: PackedDepth,
        previous: &[u8],
        fg_le: &[u8],
        dst: &mut [u8],
    ) -> Result<(), KernelError> {
        depth.check(fg_le, dst)?;
        if previous.len() != dst.len() {
            return Err(KernelError::Length);
        }
        // SAFETY: ISA 与全部长度已检查，两个 slice 的借用不重叠。
        unsafe { (self.xor_previous)(previous, fg_le, dst) };
        Ok(())
    }
    /// LSB-first；None 表示 caller 已决定的 first-line 零背景，最多 8 个 pixel。
    pub fn mask_previous(
        &self,
        depth: PackedDepth,
        previous: Option<&[u8]>,
        fg_le: &[u8],
        mask: u8,
        bit_offset: u8,
        dst: &mut [u8],
    ) -> Result<(), KernelError> {
        depth.check(fg_le, dst)?;
        if previous.is_some_and(|p| p.len() != dst.len())
            || usize::from(bit_offset)
                .checked_add(dst.len() / depth.bytes())
                .is_none_or(|end| end > 8)
        {
            return Err(KernelError::Length);
        }
        // SAFETY: ISA、全部长度和 mask 的 bit_offset+count 已在任何写入前检查。
        unsafe { (self.mask_previous)(previous, fg_le, mask, bit_offset, dst) };
        Ok(())
    }
    /// caller 传入已发 pixel 的奇偶相位；kernel 不解析 wire pair 数或行边界。
    pub fn dither_pixels(
        &self,
        depth: PackedDepth,
        a_le: &[u8],
        b_le: &[u8],
        starts_with_b: bool,
        dst: &mut [u8],
    ) -> Result<(), KernelError> {
        depth.check(a_le, dst)?;
        depth.check(b_le, dst)?;
        // SAFETY: ISA、两种 pixel 尺寸和完整目标 pixel 数已检查。
        unsafe { (self.dither)(a_le, b_le, starts_with_b, dst) };
        Ok(())
    }
    /// 仅用于 caller 指定的非首行：逐 byte delta 解码后与上一行模 256 相加。
    pub fn resolve_planar_delta(
        &self,
        previous: &[u8],
        current: &mut [u8],
    ) -> Result<(), KernelError> {
        if previous.len() != current.len() {
            return Err(KernelError::Length);
        }
        // SAFETY: ISA、相等长度及无重叠借用已检查；短尾在本地数组上执行。
        unsafe { (self.planar_delta)(previous, current) };
        Ok(())
    }
    /// 同长度 R/G/B planes 合并为 RGB24；允许空 slice，没有 stride 或 padding 假设。
    pub fn rgb_planes_to_rgb24(
        &self,
        r: &[u8],
        g: &[u8],
        b: &[u8],
        out: &mut [u8],
    ) -> Result<(), KernelError> {
        if r.len() != g.len() || r.len() != b.len() || r.len().checked_mul(3) != Some(out.len()) {
            return Err(KernelError::Length);
        }
        // SAFETY: ISA 与全部输入/输出长度在写入前检查。
        unsafe { (self.planar_rgb)(r, g, b, out) };
        Ok(())
    }
    /// u8 shift 截断后再符号扩展；无 alpha 时输出 B/G/R，有 alpha 时输出 R/G/B。
    /// alpha plane 的解析/消费由 caller 负责。2×2 chroma 支持最后单行/单列。
    pub fn ycocg_planes_to_rgb24(
        &self,
        layout: PlanarLayout,
        y: &[u8],
        co: &[u8],
        cg: &[u8],
        out: &mut [u8],
    ) -> Result<(), KernelError> {
        if layout.width == 0 || layout.height == 0 {
            return Err(KernelError::InvalidDimensions);
        }
        let full = layout
            .width
            .checked_mul(layout.height)
            .ok_or(KernelError::InvalidDimensions)?;
        let output = full.checked_mul(3).ok_or(KernelError::InvalidDimensions)?;
        let (cw, ch) = if layout.subsampled {
            (layout.width.div_ceil(2), layout.height.div_ceil(2))
        } else {
            (layout.width, layout.height)
        };
        let chroma = cw.checked_mul(ch).ok_or(KernelError::InvalidDimensions)?;
        if !(1..=7).contains(&layout.color_loss_level) {
            return Err(KernelError::InvalidColorLossLevel);
        }
        if y.len() != full || co.len() != chroma || cg.len() != chroma || out.len() != output {
            return Err(KernelError::Length);
        }
        // SAFETY: ISA、非零几何、checked sizes、CLL 与精确 plane/output 长度已检查。
        unsafe { (self.planar_ycocg)(layout, y, co, cg, out) };
        Ok(())
    }
    pub fn detect() -> Result<Self, KernelError> {
        #[cfg(any(
            target_arch = "aarch64",
            all(
                not(target_os = "macos"),
                any(target_arch = "x86", target_arch = "x86_64")
            )
        ))]
        if native::available() {
            return Ok(native::kernels());
        }
        Err(KernelError::Unavailable)
    }
    /// 外部策略只能禁用能力，不能绕过真实 CPU 检查。
    pub fn detect_if(allowed: bool) -> Result<Self, KernelError> {
        if allowed {
            Self::detect()
        } else {
            Err(KernelError::Unavailable)
        }
    }
    pub fn backend(&self) -> Backend {
        self.backend
    }
    pub fn dequantize(&self, data: &mut [i16], quant: Quantization) -> Result<(), KernelError> {
        if data.len() != 4096 {
            return Err(KernelError::Length);
        }
        if quant.0.iter().any(|&q| q > 16) {
            return Err(KernelError::InvalidQuant);
        }
        // SAFETY: 构造器验证 ISA；长度和全部移位已先验证。
        unsafe { (self.dequant)(data, &quant.0) };
        Ok(())
    }
    pub fn inverse_dwt(&self, data: &mut [i16], scratch: &mut [i16]) -> Result<(), KernelError> {
        if data.len() != 4096 || scratch.len() != 4096 {
            return Err(KernelError::Length);
        }
        // SAFETY: ISA 已检查，两个独立可变借用恰为 4096 元素。
        unsafe { (self.dwt)(data, scratch) };
        Ok(())
    }
    /// 任意长度 modulo-16 prefix，包括空输入；调用方选择 LL3 范围。
    pub fn differential(&self, data: &mut [i16]) {
        // SAFETY: ISA 已检查；后端仅在完整向量或本地 staging 上操作。
        unsafe { (self.prefix)(data) }
    }
    /// RLGR 的零 run 与末尾补零叶子；不解析或改写 entropy 状态。
    pub fn zero_i16(&self, data: &mut [i16]) {
        // SAFETY: ISA 已检查；尾部通过本地 staging。
        unsafe { (self.zero)(data) }
    }
    pub fn ycbcr_to_rgba(
        &self,
        y: &[i16],
        cb: &[i16],
        cr: &[i16],
        out: &mut [u8],
    ) -> Result<(), KernelError> {
        if y.len() != cb.len() || y.len() != cr.len() || y.len().checked_mul(4) != Some(out.len()) {
            return Err(KernelError::Length);
        }
        // SAFETY: ISA 和全部输入输出长度已检查；后端预检全部算术再写输出。
        unsafe { (self.color)(y, cb, cr, out) }
    }
}

#[cfg(test)]
mod cursor_length_tests {
    use super::CursorXorFormat as F;
    #[test]
    fn cursor_encoded_lengths_use_checked_multiply_and_overflow_free_ceil() {
        assert_eq!(F::Mono1.data_bytes(usize::MAX), Some(usize::MAX / 8 + 1));
        for f in [F::Rgb565, F::Bgr24, F::Bgra32] {
            assert_eq!(f.data_bytes(usize::MAX), None);
            assert_eq!(f.data_bytes(0), Some(0));
        }
    }
}
