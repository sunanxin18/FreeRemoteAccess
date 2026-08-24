use std::error::Error;
use std::fmt;

const MAX_SESSION_PIXELS: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameUpdate {
    pub generation: u64,
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDisposition {
    Applied,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionFrameError {
    code: &'static str,
}

impl SessionFrameError {
    fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for SessionFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "远程画面状态无效 ({})", self.code)
    }
}

impl Error for SessionFrameError {}

#[derive(Debug)]
pub struct SessionFramebuffer {
    generation: u64,
    width: u16,
    height: u16,
    pixels: Vec<u32>,
}

impl SessionFramebuffer {
    pub fn new(generation: u64, width: u16, height: u16) -> Result<Self, SessionFrameError> {
        let pixel_count = validate_dimensions(width, height)?;
        Ok(Self {
            generation,
            width,
            height,
            pixels: vec![0; pixel_count],
        })
    }

    pub fn begin_generation(
        &mut self,
        generation: u64,
        width: u16,
        height: u16,
    ) -> Result<(), SessionFrameError> {
        let pixel_count = validate_dimensions(width, height)?;
        self.generation = generation;
        self.width = width;
        self.height = height;
        self.pixels = vec![0; pixel_count];
        Ok(())
    }

    pub fn apply_frame(
        &mut self,
        frame: FrameUpdate,
    ) -> Result<FrameDisposition, SessionFrameError> {
        if frame.generation != self.generation {
            return Ok(FrameDisposition::Stale);
        }
        if frame.width != self.width || frame.height != self.height {
            return Err(SessionFrameError::new("frame_dimensions_mismatch"));
        }
        if frame.pixels.len() != self.pixels.len() {
            return Err(SessionFrameError::new("frame_length_mismatch"));
        }

        self.pixels = frame.pixels;
        Ok(FrameDisposition::Applied)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }
}

fn validate_dimensions(width: u16, height: u16) -> Result<usize, SessionFrameError> {
    if width == 0 || height == 0 {
        return Err(SessionFrameError::new("frame_dimensions_invalid"));
    }
    let pixel_count = usize::from(width) * usize::from(height);
    if pixel_count > MAX_SESSION_PIXELS {
        return Err(SessionFrameError::new("frame_dimensions_too_large"));
    }
    Ok(pixel_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_for(generation: u64, width: u16, height: u16, pixel: u32) -> FrameUpdate {
        FrameUpdate {
            generation,
            width,
            height,
            pixels: vec![pixel; usize::from(width) * usize::from(height)],
        }
    }

    #[test]
    fn resize_invalidates_frames_from_the_old_generation() {
        let mut state = SessionFramebuffer::new(1, 2, 2).unwrap();
        state.begin_generation(2, 3, 2).unwrap();

        assert_eq!(
            state.apply_frame(frame_for(1, 2, 2, 0x0011_2233)).unwrap(),
            FrameDisposition::Stale
        );
        assert_eq!(
            state.apply_frame(frame_for(2, 3, 2, 0x0044_5566)).unwrap(),
            FrameDisposition::Applied
        );
        assert_eq!(state.pixels(), &[0x0044_5566; 6]);
    }

    #[test]
    fn new_generation_clears_pixels_before_a_frame_arrives() {
        let mut state = SessionFramebuffer::new(4, 2, 1).unwrap();
        state.apply_frame(frame_for(4, 2, 1, 0x00ff_ffff)).unwrap();

        state.begin_generation(5, 1, 2).unwrap();

        assert_eq!(state.generation(), 5);
        assert_eq!(state.dimensions(), (1, 2));
        assert_eq!(state.pixels(), &[0, 0]);
    }

    #[test]
    fn current_generation_rejects_wrong_dimensions_and_pixel_count() {
        let mut state = SessionFramebuffer::new(7, 2, 2).unwrap();

        let dimension_error = state
            .apply_frame(frame_for(7, 1, 4, 0x0000_0001))
            .unwrap_err();
        assert_eq!(dimension_error.code(), "frame_dimensions_mismatch");

        let length_error = state
            .apply_frame(FrameUpdate {
                generation: 7,
                width: 2,
                height: 2,
                pixels: vec![0; 3],
            })
            .unwrap_err();
        assert_eq!(length_error.code(), "frame_length_mismatch");
    }

    #[test]
    fn zero_dimensions_are_rejected() {
        let error = SessionFramebuffer::new(1, 0, 1080).unwrap_err();

        assert_eq!(error.code(), "frame_dimensions_invalid");
    }
}
