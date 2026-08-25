use super::{desktop_audio, AudioOutputSink, AudioOutputSpec, PlatformError, PlatformServices};

pub struct LinuxPlatformServices;

impl PlatformServices for LinuxPlatformServices {
    fn create_audio_output(&self, spec: AudioOutputSpec) -> Result<AudioOutputSink, PlatformError> {
        desktop_audio::open_cpal_audio_output(spec)
    }

    fn set_clipboard_text(&self, _text: &str) -> Result<(), PlatformError> {
        Err(PlatformError::new("clipboard_not_implemented"))
    }

    fn open_external_url(&self, _url: &str) -> Result<(), PlatformError> {
        Err(PlatformError::new("external_url_not_implemented"))
    }
}
