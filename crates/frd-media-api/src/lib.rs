//! 协议和平台之间传递媒体数据的中立契约。

/// 可移动的协议无关媒体载荷。所有具体解码器和设备后端都在此边界之外。
#[derive(Debug)]
pub enum MediaFrame {
    Pcm {
        sample_rate_hz: u32,
        channels: u8,
        samples: Box<[i16]>,
    },
    EncodedVideo {
        timestamp_us: u64,
        bytes: Box<[u8]>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaPublishError {
    Closed,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioOutputError {
    Unavailable,
    UnsupportedFormat,
    Closed,
}

/// 平台音频输出只消费协议已经解码出的 PCM；实现不得打开输入设备。
pub trait AudioOutput: Send {
    fn enqueue_pcm(
        &mut self,
        sample_rate_hz: u32,
        channels: u8,
        samples: Box<[i16]>,
    ) -> Result<(), AudioOutputError>;
}

pub trait MediaPublisher: Send {
    fn publish(&self, frame: MediaFrame) -> Result<(), MediaPublishError>;
}
