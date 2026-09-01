use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaDecoderBackend {
    Native,
    Ffmpeg,
}

impl fmt::Display for MediaDecoderBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Native => "native",
            Self::Ffmpeg => "ffmpeg",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaDecoderMode {
    Hardware,
    Software,
}

impl fmt::Display for MediaDecoderMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Hardware => "hardware",
            Self::Software => "software",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaVideoOutput {
    Yuv444p8,
    Nv12,
    Yuv420p8,
    P010,
}

impl fmt::Display for MediaVideoOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Yuv444p8 => "yuv444p8",
            Self::Nv12 => "nv12",
            Self::Yuv420p8 => "yuv420p8",
            Self::P010 => "p010",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaStageDiagnostic {
    Message1ConfigurationWritten {
        generation: u64,
    },
    Message2TransportActive {
        generation: u64,
    },
    AuthenticatedVideoRtp {
        generation: u64,
        stream_id: u32,
    },
    HevcAccessUnitPublished {
        generation: u64,
        stream_id: u32,
        width: u32,
        height: u32,
    },
    DecoderSelected {
        generation: u64,
        stream_id: u32,
        backend: MediaDecoderBackend,
        mode: MediaDecoderMode,
        output: MediaVideoOutput,
    },
    FrameDecoded {
        generation: u64,
        stream_id: u32,
        width: u32,
        height: u32,
    },
    FrameUploaded {
        generation: u64,
        stream_id: u32,
        width: u32,
        height: u32,
    },
    FramePresented {
        generation: u64,
        stream_id: u32,
    },
}

impl MediaStageDiagnostic {
    const fn bit(self) -> u8 {
        match self {
            Self::Message1ConfigurationWritten { .. } => 0,
            Self::Message2TransportActive { .. } => 1,
            Self::AuthenticatedVideoRtp { .. } => 2,
            Self::HevcAccessUnitPublished { .. } => 3,
            Self::DecoderSelected { .. } => 4,
            Self::FrameDecoded { .. } => 5,
            Self::FrameUploaded { .. } => 6,
            Self::FramePresented { .. } => 7,
        }
    }
}

impl fmt::Display for MediaStageDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[frd-media-stage] stage=")?;
        match self {
            Self::Message1ConfigurationWritten { generation } => write!(
                formatter,
                "message1_configuration_written generation={generation}"
            ),
            Self::Message2TransportActive { generation } => {
                write!(formatter, "message2_transport_active generation={generation}")
            }
            Self::AuthenticatedVideoRtp {
                generation,
                stream_id,
            } => write!(
                formatter,
                "authenticated_video_rtp generation={generation} stream={stream_id}"
            ),
            Self::HevcAccessUnitPublished {
                generation,
                stream_id,
                width,
                height,
            } => write!(
                formatter,
                "hevc_access_unit_published generation={generation} stream={stream_id} width={width} height={height}"
            ),
            Self::DecoderSelected {
                generation,
                stream_id,
                backend,
                mode,
                output,
            } => write!(
                formatter,
                "decoder_selected generation={generation} stream={stream_id} backend={backend} mode={mode} output={output}"
            ),
            Self::FrameDecoded {
                generation,
                stream_id,
                width,
                height,
            } => write!(
                formatter,
                "frame_decoded generation={generation} stream={stream_id} width={width} height={height}"
            ),
            Self::FrameUploaded {
                generation,
                stream_id,
                width,
                height,
            } => write!(
                formatter,
                "frame_uploaded generation={generation} stream={stream_id} width={width} height={height}"
            ),
            Self::FramePresented {
                generation,
                stream_id,
            } => write!(
                formatter,
                "frame_presented generation={generation} stream={stream_id}"
            ),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MediaStageTrace {
    observed: u8,
}

impl MediaStageTrace {
    pub fn observe(&mut self, diagnostic: MediaStageDiagnostic) -> bool {
        let bit = 1u8 << diagnostic.bit();
        if self.observed & bit != 0 {
            return false;
        }
        self.observed |= bit;
        #[cfg(debug_assertions)]
        eprintln!("{diagnostic}");
        true
    }
}
