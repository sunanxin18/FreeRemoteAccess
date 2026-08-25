use std::error::Error;
use std::fmt;
use std::sync::Arc;

use raw_window_handle::{DisplayHandle, WindowHandle};

mod desktop_audio;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub struct SurfaceHandle<'a> {
    pub window: WindowHandle<'a>,
    pub display: DisplayHandle<'a>,
}

pub trait WindowHost {
    fn request_redraw(&self) -> Result<(), PlatformError>;
    fn surface_handle(&self) -> Result<SurfaceHandle<'_>, PlatformError>;
    fn set_fullscreen(&self, enabled: bool) -> Result<(), PlatformError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioOutputSpec {
    sample_rate_hz: u32,
    channels: u16,
}

impl AudioOutputSpec {
    pub const NORMALIZED_SAMPLE_RATE_HZ: u32 = 48_000;
    pub const NORMALIZED_CHANNELS: u16 = 2;

    pub const fn new(sample_rate_hz: u32, channels: u16) -> Result<Self, PlatformError> {
        if sample_rate_hz != Self::NORMALIZED_SAMPLE_RATE_HZ
            || channels != Self::NORMALIZED_CHANNELS
        {
            return Err(PlatformError::new("audio_output_spec_unsupported"));
        }
        Ok(Self {
            sample_rate_hz,
            channels,
        })
    }

    pub const fn normalized() -> Self {
        Self {
            sample_rate_hz: Self::NORMALIZED_SAMPLE_RATE_HZ,
            channels: Self::NORMALIZED_CHANNELS,
        }
    }

    pub const fn sample_rate_hz(self) -> u32 {
        self.sample_rate_hz
    }

    pub const fn channels(self) -> u16 {
        self.channels
    }
}

pub trait AudioOutputBackend: Send {
    fn enqueue_interleaved_i16(&mut self, samples: &[i16]) -> Result<(), PlatformError>;
    fn device_description(&self) -> &str;
}

pub struct AudioOutputSink {
    spec: AudioOutputSpec,
    backend: Box<dyn AudioOutputBackend>,
}

impl AudioOutputSink {
    pub const MAX_INTERLEAVED_I16_SAMPLES_PER_ENQUEUE: usize = 131_072;

    pub fn new(spec: AudioOutputSpec, backend: Box<dyn AudioOutputBackend>) -> Self {
        Self { spec, backend }
    }

    pub fn enqueue_interleaved_i16(&mut self, samples: &[i16]) -> Result<(), PlatformError> {
        if samples.len() > Self::MAX_INTERLEAVED_I16_SAMPLES_PER_ENQUEUE {
            return Err(PlatformError::new("audio_output_pcm_too_large"));
        }
        if !samples
            .len()
            .is_multiple_of(usize::from(self.spec.channels()))
        {
            return Err(PlatformError::new("audio_output_pcm_alignment"));
        }
        self.backend.enqueue_interleaved_i16(samples)
    }

    pub fn device_description(&self) -> &str {
        self.backend.device_description()
    }
}

pub trait PlatformServices: Send + Sync {
    fn create_audio_output(&self, spec: AudioOutputSpec) -> Result<AudioOutputSink, PlatformError>;
    fn set_clipboard_text(&self, text: &str) -> Result<(), PlatformError>;
    fn open_external_url(&self, url: &str) -> Result<(), PlatformError>;
}

pub fn production_platform_services() -> Arc<dyn PlatformServices> {
    #[cfg(target_os = "windows")]
    {
        Arc::new(windows::WindowsPlatformServices)
    }
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacOsPlatformServices)
    }
    #[cfg(target_os = "linux")]
    {
        Arc::new(linux::LinuxPlatformServices)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Arc::new(UnsupportedPlatformServices)
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
struct UnsupportedPlatformServices;

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
impl PlatformServices for UnsupportedPlatformServices {
    fn create_audio_output(
        &self,
        _spec: AudioOutputSpec,
    ) -> Result<AudioOutputSink, PlatformError> {
        Err(PlatformError::new("audio_output_unavailable"))
    }

    fn set_clipboard_text(&self, _text: &str) -> Result<(), PlatformError> {
        Err(PlatformError::new("clipboard_unavailable"))
    }

    fn open_external_url(&self, _url: &str) -> Result<(), PlatformError> {
        Err(PlatformError::new("external_url_unavailable"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformError {
    code: &'static str,
}

impl PlatformError {
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "本地平台操作失败 ({})", self.code)
    }
}

impl Error for PlatformError {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::{
        production_platform_services, AudioOutputBackend, AudioOutputSink, AudioOutputSpec,
        PlatformError,
    };

    struct CountingBackend {
        enqueues: Arc<AtomicUsize>,
    }

    impl AudioOutputBackend for CountingBackend {
        fn enqueue_interleaved_i16(&mut self, _samples: &[i16]) -> Result<(), PlatformError> {
            self.enqueues.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn device_description(&self) -> &str {
            "测试输出设备"
        }
    }

    #[test]
    fn normalized_pcm_spec_is_fixed_to_48khz_stereo_i16() {
        fn assert_send<T: Send>() {}

        assert_send::<AudioOutputSink>();
        let spec = AudioOutputSpec::new(48_000, 2).unwrap();

        assert_eq!(spec, AudioOutputSpec::normalized());
        assert_eq!(spec.sample_rate_hz(), 48_000);
        assert_eq!(spec.channels(), 2);
        for (sample_rate_hz, channels) in [(44_100, 2), (48_000, 1), (48_000, 3)] {
            assert_eq!(
                AudioOutputSpec::new(sample_rate_hz, channels)
                    .unwrap_err()
                    .code(),
                "audio_output_spec_unsupported"
            );
        }
    }

    #[test]
    fn sink_rejects_channel_misalignment_before_backend_enqueue() {
        let enqueues = Arc::new(AtomicUsize::new(0));
        let mut sink = AudioOutputSink::new(
            AudioOutputSpec::normalized(),
            Box::new(CountingBackend {
                enqueues: Arc::clone(&enqueues),
            }),
        );

        assert_eq!(
            sink.enqueue_interleaved_i16(&[1, 2, 3]).unwrap_err().code(),
            "audio_output_pcm_alignment"
        );
        assert_eq!(enqueues.load(Ordering::SeqCst), 0);
        sink.enqueue_interleaved_i16(&[1, 2, 3, 4]).unwrap();
        assert_eq!(enqueues.load(Ordering::SeqCst), 1);
        assert_eq!(sink.device_description(), "测试输出设备");
    }

    #[test]
    fn sink_rejects_an_oversized_pcm_chunk_before_backend_enqueue() {
        let enqueues = Arc::new(AtomicUsize::new(0));
        let mut sink = AudioOutputSink::new(
            AudioOutputSpec::normalized(),
            Box::new(CountingBackend {
                enqueues: Arc::clone(&enqueues),
            }),
        );
        let oversized = vec![
            0;
            AudioOutputSink::MAX_INTERLEAVED_I16_SAMPLES_PER_ENQUEUE
                .checked_add(2)
                .unwrap()
        ];

        assert_eq!(
            sink.enqueue_interleaved_i16(&oversized).unwrap_err().code(),
            "audio_output_pcm_too_large"
        );
        assert_eq!(enqueues.load(Ordering::SeqCst), 0);
    }

    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    #[test]
    fn production_factory_returns_current_desktop_services() {
        let services = production_platform_services();

        assert_eq!(
            services.set_clipboard_text("offline").unwrap_err().code(),
            "clipboard_not_implemented"
        );
        assert_eq!(
            services
                .open_external_url("https://example.invalid")
                .unwrap_err()
                .code(),
            "external_url_not_implemented"
        );
    }

    #[test]
    fn phase_one_platform_sources_implement_the_same_contract() {
        for source in [
            include_str!("windows.rs"),
            include_str!("macos.rs"),
            include_str!("linux.rs"),
        ] {
            assert!(source.contains("impl PlatformServices"));
            assert!(source.contains("create_audio_output"));
            assert!(!source.contains(concat!("default_", "input_device")));
        }
        let module = include_str!("mod.rs");
        for target in ["windows", "macos", "linux"] {
            assert!(module.contains(&format!("target_os = \"{target}\"")));
        }
        assert!(!module.contains(concat!("compile_", "error!")));
        assert!(module.contains("UnsupportedPlatformServices"));
    }
}
