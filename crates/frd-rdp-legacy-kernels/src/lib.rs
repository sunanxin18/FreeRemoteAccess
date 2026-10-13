//! 受检查的 RemoteFX 数值与 packed Bitmap run 叶子，不解析协议，也不代表 codec/session 就绪。
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
}
impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "当前 CPU 或平台不支持原生叶子",
            Self::Length => "缓冲区长度不符合叶子契约",
            Self::InvalidQuant => "量化值必须在 0..=16 范围",
            Self::ColorOverflow => "色彩转换的上游算术中间值溢出",
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

type XorKernel = unsafe fn(&[u8], &[u8], &mut [u8]);
type MaskKernel = unsafe fn(Option<&[u8]>, &[u8], u8, u8, &mut [u8]);
type DitherKernel = unsafe fn(&[u8], &[u8], bool, &mut [u8]);
type ColorKernel = unsafe fn(&[i16], &[i16], &[i16], &mut [u8]) -> Result<(), KernelError>;

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
}
impl NativeKernels {
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
