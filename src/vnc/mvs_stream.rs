//! MVS 记录的精确传输层重组。

#[cfg(any(feature = "media", test))]
use anyhow::ensure;
use anyhow::{anyhow, Result};

pub const MAX_MVS_RECORD_PAYLOAD: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MvsRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[cfg(any(feature = "media", test))]
pub fn validate_mvs_rect_against_surface(
    rect: MvsRect,
    surface_width: u16,
    surface_height: u16,
) -> Result<()> {
    ensure!(
        rect.width > 0 && rect.height > 0,
        "MVS 媒体矩形尺寸不能为零"
    );
    let right = rect
        .x
        .checked_add(rect.width)
        .ok_or_else(|| anyhow!("MVS 矩形水平边界溢出"))?;
    let bottom = rect
        .y
        .checked_add(rect.height)
        .ok_or_else(|| anyhow!("MVS 矩形垂直边界溢出"))?;
    ensure!(
        right <= surface_width && bottom <= surface_height,
        "MVS 矩形超出当前 surface: {}x{}+{},{} > {}x{}",
        rect.width,
        rect.height,
        rect.x,
        rect.y,
        surface_width,
        surface_height
    );
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsRecord {
    pub rect: MvsRect,
    pub payload: Vec<u8>,
}

#[derive(Default)]
pub struct MvsRecordAssembler {
    pending: Option<PendingRecord>,
}

struct PendingRecord {
    rect: MvsRect,
    total: usize,
    payload: Vec<u8>,
}

impl MvsRecordAssembler {
    pub fn begin(&mut self, rect: MvsRect, total: u32, first: &[u8]) -> Result<Option<MvsRecord>> {
        if self.pending.is_some() {
            self.pending = None;
            return Err(anyhow!("MVS 记录已在重组中"));
        }

        let total = usize::try_from(total).map_err(|_| anyhow!("MVS 记录长度超出平台限制"))?;
        if total == 0 || total > MAX_MVS_RECORD_PAYLOAD || first.len() > total {
            return Err(anyhow!("MVS 首片长度无效"));
        }

        if first.len() == total {
            return Ok(Some(MvsRecord {
                rect,
                payload: first.to_vec(),
            }));
        }

        self.pending = Some(PendingRecord {
            rect,
            total,
            payload: first.to_vec(),
        });
        Ok(None)
    }

    pub fn push_continuation(&mut self, chunk: &[u8]) -> Result<Option<MvsRecord>> {
        let Some(mut pending) = self.pending.take() else {
            return Err(anyhow!("MVS continuation 缺少首片"));
        };

        if chunk.len() > pending.total - pending.payload.len() {
            return Err(anyhow!("MVS continuation 超出记录长度"));
        }
        pending.payload.extend_from_slice(chunk);

        if pending.payload.len() == pending.total {
            Ok(Some(MvsRecord {
                rect: pending.rect,
                payload: pending.payload,
            }))
        } else {
            self.pending = Some(pending);
            Ok(None)
        }
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    #[cfg(any(feature = "media", test))]
    pub fn abort(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        validate_mvs_rect_against_surface, MvsRecordAssembler, MvsRect, MAX_MVS_RECORD_PAYLOAD,
    };

    #[test]
    fn mvs_rectangle_must_fit_the_current_surface() {
        let surface = (1920u16, 1080u16);
        assert!(validate_mvs_rect_against_surface(
            MvsRect {
                x: 100,
                y: 100,
                width: 200,
                height: 300,
            },
            surface.0,
            surface.1,
        )
        .is_ok());
        assert!(validate_mvs_rect_against_surface(
            MvsRect {
                x: 1900,
                y: 0,
                width: 100,
                height: 100,
            },
            surface.0,
            surface.1,
        )
        .is_err());
    }

    #[test]
    fn reassembles_captured_fragment_lengths_without_dropping_bytes() {
        let rect = MvsRect {
            x: 0,
            y: 1256,
            width: 1358,
            height: 1112,
        };
        let first = vec![0x11; 32_748];
        let continuation = vec![0x22; 26_572];
        let mut assembler = MvsRecordAssembler::default();

        assert!(assembler.begin(rect, 59_320, &first).unwrap().is_none());
        let record = assembler.push_continuation(&continuation).unwrap().unwrap();

        assert_eq!(record.rect, rect);
        assert_eq!(record.payload.len(), 59_320);
        assert_eq!(&record.payload[..32_748], &first);
        assert_eq!(&record.payload[32_748..], &continuation);
    }

    #[test]
    fn rejects_first_chunk_overflow_and_clears_pending_state() {
        let mut assembler = MvsRecordAssembler::default();
        let result = assembler.begin(
            MvsRect {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            },
            2,
            &[1, 2, 3],
        );
        assert!(result.is_err());
        assert!(!assembler.is_pending());
    }

    #[test]
    fn rejects_continuation_overflow_and_clears_pending_state() {
        let mut assembler = MvsRecordAssembler::default();
        assert!(assembler
            .begin(
                MvsRect {
                    x: 1,
                    y: 2,
                    width: 3,
                    height: 4
                },
                3,
                &[1, 2]
            )
            .unwrap()
            .is_none());
        assert!(assembler.push_continuation(&[3, 4]).is_err());
        assert!(!assembler.is_pending());
    }

    #[test]
    fn rejects_continuation_without_a_start() {
        let mut assembler = MvsRecordAssembler::default();
        assert!(assembler.push_continuation(&[1]).is_err());
        assert!(!assembler.is_pending());
    }

    #[test]
    fn rejects_second_start_while_pending_and_clears_pending_state() {
        let mut assembler = MvsRecordAssembler::default();
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert!(assembler.begin(rect, 2, &[1]).unwrap().is_none());
        assert!(assembler.begin(rect, 2, &[2]).is_err());
        assert!(!assembler.is_pending());
    }

    #[test]
    fn accepts_protocol_maximum_without_preallocating_the_declared_total() {
        let mut assembler = MvsRecordAssembler::default();
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };

        assert!(assembler
            .begin(rect, MAX_MVS_RECORD_PAYLOAD as u32, &[1])
            .unwrap()
            .is_none());
        assert!(assembler.is_pending());
        assembler.abort();
    }

    #[test]
    fn rejects_over_protocol_maximum_and_clears_pending_state() {
        let mut assembler = MvsRecordAssembler::default();
        let result = assembler.begin(
            MvsRect {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            },
            (MAX_MVS_RECORD_PAYLOAD + 1) as u32,
            &[1],
        );

        assert!(result.is_err());
        assert!(!assembler.is_pending());
    }
}
