//! Progressive 严格熵层。位流/KP 跨 subband 连续，失败不可补零。
//! MS-RDPEGFX 3.1.8.1.5.1/2，3.2.8.1.5.2.1。
//! 独立对照 FreeRDP 54a873e2710710841c6ec2b756df64285ec6e29a progressive.c。
//! 本层不改变持久 DWT/DAS；调用方在完整 tile 成功后事务提交 reference。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    TrailingData,
    InvalidBits,
    InvalidSign,
    RunOverrun,
    CoefficientRange,
    TooManyCoefficients,
}
pub type Result<T> = std::result::Result<T, Error>;
const MAX_COEFFICIENTS: usize = 4096;

#[derive(Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
}
impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }
    pub fn position(&self) -> usize {
        self.position
    }
    pub fn read(&mut self, n: u8) -> Result<u32> {
        if n > 32 {
            return Err(Error::InvalidBits);
        }
        let end = self
            .position
            .checked_add(usize::from(n))
            .ok_or(Error::Truncated)?;
        if end.div_ceil(8) > self.data.len() {
            return Err(Error::Truncated);
        }
        let mut v = 0;
        while self.position < end {
            v = (v << 1) | u32::from((self.data[self.position / 8] >> (7 - self.position % 8)) & 1);
            self.position += 1;
        }
        Ok(v)
    }
    /// 最后一字节仅允许零对齐；额外完整字节不能伪装为对齐。
    pub fn finish_zero_padding(mut self) -> Result<()> {
        let padding = (8 - self.position % 8) % 8;
        if self.read(padding as u8)? != 0 || self.position / 8 != self.data.len() {
            return Err(Error::TrailingData);
        }
        Ok(())
    }
}

fn gr(reader: &mut BitReader<'_>, krp: &mut u32) -> Result<u32> {
    let kr = *krp / 8;
    let mut q = 0u32;
    while reader.read(1)? != 0 {
        q += 1;
        if q > (65535 >> kr) {
            return Err(Error::CoefficientRange);
        }
    }
    let v = (q << kr) | reader.read(kr as u8)?;
    if q == 0 {
        *krp = krp.saturating_sub(2);
    } else if q > 1 {
        *krp = (*krp + q).min(80);
    }
    Ok(v)
}
fn signed(magnitude: u32, negative: bool) -> Result<i16> {
    let value = if negative {
        -(i64::from(magnitude))
    } else {
        i64::from(magnitude)
    };
    i16::try_from(value).map_err(|_| Error::CoefficientRange)
}

/// 初始/SIMPLE 每分量 RLGR1；先解入临时结果，截断/越界不部分修改调用方输出。
/// 零尾run需明确覆盖输出；允许exact escape覆盖后的byte零对齐，不隐式补系数。
pub fn decode_rlgr1(data: &[u8], output: &mut [i16]) -> Result<()> {
    if output.is_empty() || output.len() > MAX_COEFFICIENTS {
        return Err(Error::TooManyCoefficients);
    }
    let mut reader = BitReader::new(data);
    let mut result = vec![0; output.len()];
    let mut kp = 8u32;
    let mut krp = 8u32;
    let mut pos = 0usize;
    'coefficients: while pos < result.len() {
        let mut k = kp / 8;
        if k > 0 {
            let mut run = 0usize;
            while reader.read(1)? == 0 {
                run = run.checked_add(1usize << k).ok_or(Error::RunOverrun)?;
                if run > result.len() - pos {
                    return Err(Error::RunOverrun);
                }
                kp = (kp + 4).min(80);
                k = kp / 8;
                // escape本身已明确编码全部剩余零时，可直接以当前byte零对齐结束。
                // 不读padding为下一escape，不裁剪overshoot、不补缺失系数。
                if run == result.len() - pos && reader.clone().finish_zero_padding().is_ok() {
                    break 'coefficients;
                }
            }
            run = run
                .checked_add(reader.read(k as u8)? as usize)
                .ok_or(Error::RunOverrun)?;
            if run > result.len() - pos {
                return Err(Error::RunOverrun);
            }
            pos += run;
            if pos == result.len() {
                // 微软encoder的short尾与完整末符号均可结束精确覆盖的run。
                // 仅在short零对齐不成立时消费一个符号；绝不跳过任意残留。
                if reader.clone().finish_zero_padding().is_err() {
                    let negative = reader.read(1)? != 0;
                    let magnitude = gr(&mut reader, &mut krp)?
                        .checked_add(1)
                        .ok_or(Error::CoefficientRange)?;
                    signed(magnitude, negative)?;
                }
                break;
            }
            let negative = reader.read(1)? != 0;
            let magnitude = gr(&mut reader, &mut krp)?
                .checked_add(1)
                .ok_or(Error::CoefficientRange)?;
            result[pos] = signed(magnitude, negative)?;
            pos += 1;
            kp = kp.saturating_sub(6);
        } else {
            let code = gr(&mut reader, &mut krp)?;
            if code == 0 {
                kp = (kp + 3).min(80);
            } else {
                kp = kp.saturating_sub(3);
            }
            result[pos] = signed((code + 1) / 2, code & 1 != 0)?;
            pos += 1;
        }
    }
    reader.finish_zero_padding()?;
    output.copy_from_slice(&result);
    Ok(())
}

/// 不带反量化shift的位流值；符号/缩放/累加由目标像素kernel处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refinement {
    pub magnitude: u16,
    pub negative: bool,
}
impl Refinement {
    fn zero() -> Self {
        Self {
            magnitude: 0,
            negative: false,
        }
    }
}

fn checked_refinement(magnitude: u32, negative: bool) -> Result<Refinement> {
    let limit = if negative { 32768 } else { 32767 };
    if magnitude > limit {
        return Err(Error::CoefficientRange);
    }
    Ok(Refinement {
        magnitude: magnitude as u16,
        negative,
    })
}

#[derive(Clone)]
pub struct UpgradeReader<'a> {
    srl: BitReader<'a>,
    raw: BitReader<'a>,
    kp: u32,
    zeros: usize,
    unary_pending: bool,
    coefficients: usize,
}
impl<'a> UpgradeReader<'a> {
    pub fn new(srl: &'a [u8], raw: &'a [u8]) -> Self {
        Self {
            srl: BitReader::new(srl),
            raw: BitReader::new(raw),
            kp: 8,
            zeros: 0,
            unary_pending: false,
            coefficients: 0,
        }
    }
    fn srl_value(&mut self, bits: u8) -> Result<Refinement> {
        if self.zeros > 0 {
            self.zeros -= 1;
            return Ok(Refinement::zero());
        }
        if !self.unary_pending {
            let k = self.kp / 8;
            if self.srl.read(1)? == 0 {
                self.zeros = (1usize << k) - 1;
                self.kp = (self.kp + 4).min(80);
                return Ok(Refinement::zero());
            }
            self.zeros = self.srl.read(k as u8)? as usize;
            self.unary_pending = true;
            if self.zeros > 0 {
                self.zeros -= 1;
                return Ok(Refinement::zero());
            }
        }
        self.unary_pending = false;
        self.kp = self.kp.saturating_sub(6);
        let negative = self.srl.read(1)? != 0;
        let maximum = (1u32 << bits) - 1;
        let mut magnitude = 1;
        let signed_limit = if negative { 32768 } else { 32767 };
        while magnitude < maximum && self.srl.read(1)? == 0 {
            magnitude += 1;
            // numBits 允许22，但系数域仍是i16；拒绝超范围而非截断或饱和。
            // unary 工作量因此最多32768位，不按2^numBits分配或循环。
            if magnitude > signed_limit {
                return Err(Error::CoefficientRange);
            }
        }
        checked_refinement(magnitude, negative)
    }
    /// signs 是旧 DAS 的 -1/0/+1，仅用以选择 RAW/SRL；LL无论DAS总走RAW。
    /// 同一分量所有band共用此实例；错误回滚reader，调用方输出/reference均未修改。
    pub fn read_band(&mut self, signs: &[i16], bits: u8, is_ll: bool) -> Result<Vec<Refinement>> {
        if bits > 22 {
            return Err(Error::InvalidBits);
        }
        if signs.iter().any(|s| !(-1..=1).contains(s)) {
            return Err(Error::InvalidSign);
        }
        let mut tx = self.clone();
        tx.coefficients = tx
            .coefficients
            .checked_add(signs.len())
            .filter(|n| *n <= MAX_COEFFICIENTS)
            .ok_or(Error::TooManyCoefficients)?;
        let mut out = Vec::with_capacity(signs.len());
        for &sign in signs {
            out.push(if bits == 0 {
                Refinement::zero()
            } else if is_ll || sign != 0 {
                checked_refinement(tx.raw.read(bits)?, !is_ll && sign < 0)?
            } else {
                tx.srl_value(bits)?
            });
        }
        *self = tx;
        Ok(out)
    }
    /// RAW仅零bit对齐；SRL允许参考实现的一个零sentinel，不吞掉任意完整尾字节。
    pub fn finish(self) -> Result<()> {
        if self.zeros != 0 {
            return Err(Error::RunOverrun);
        }
        self.raw.finish_zero_padding()?;
        let consumed_bytes = self.srl.position.div_ceil(8);
        let remaining = self
            .srl
            .data
            .len()
            .checked_sub(consumed_bytes)
            .ok_or(Error::Truncated)?;
        if remaining > 1 {
            return Err(Error::TrailingData);
        }
        if remaining == 1 && self.srl.data.last() != Some(&0) {
            return Err(Error::TrailingData);
        }
        BitReader {
            data: &self.srl.data[..consumed_bytes],
            position: self.srl.position,
        }
        .finish_zero_padding()
    }
    pub fn positions(&self) -> (usize, usize) {
        (self.srl.position(), self.raw.position())
    }
}

#[cfg(test)]
#[path = "entropy_tests.rs"]
mod tests;
