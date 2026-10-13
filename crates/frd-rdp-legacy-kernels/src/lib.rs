//! 受检查的 RemoteFX 数值叶子，不解析协议，也不代表 codec/session 就绪。
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
}
impl NativeKernels {
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
