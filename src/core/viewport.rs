use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RemoteViewportTransform {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
    remote_width: u32,
    remote_height: u32,
    scale_factor: f64,
}

impl RemoteViewportTransform {
    pub fn new(
        host_logical_size: (u32, u32),
        remote_size: (u32, u32),
        scale_factor: f64,
    ) -> Result<Self, ViewportError> {
        if host_logical_size.0 == 0
            || host_logical_size.1 == 0
            || remote_size.0 == 0
            || remote_size.1 == 0
        {
            return Err(ViewportError::new("viewport_dimensions_invalid"));
        }
        if !scale_factor.is_finite() || scale_factor <= 0.0 {
            return Err(ViewportError::new("viewport_scale_invalid"));
        }

        let host_width = f64::from(host_logical_size.0) * scale_factor;
        let host_height = f64::from(host_logical_size.1) * scale_factor;
        let remote_aspect = f64::from(remote_size.0) / f64::from(remote_size.1);
        let host_aspect = host_width / host_height;
        let (width, height) = if host_aspect > remote_aspect {
            (host_height * remote_aspect, host_height)
        } else {
            (host_width, host_width / remote_aspect)
        };

        Ok(Self {
            left: (host_width - width) * 0.5,
            top: (host_height - height) * 0.5,
            width,
            height,
            remote_width: remote_size.0,
            remote_height: remote_size.1,
            scale_factor,
        })
    }

    pub fn remote_point(self, host_logical_point: (f64, f64)) -> Option<(u32, u32)> {
        if !host_logical_point.0.is_finite() || !host_logical_point.1.is_finite() {
            return None;
        }
        let x = host_logical_point.0 * self.scale_factor;
        let y = host_logical_point.1 * self.scale_factor;
        if x < self.left
            || y < self.top
            || x >= self.left + self.width
            || y >= self.top + self.height
        {
            return None;
        }

        let remote_x = (((x - self.left) / self.width) * f64::from(self.remote_width)).floor();
        let remote_y = (((y - self.top) / self.height) * f64::from(self.remote_height)).floor();
        Some((
            (remote_x as u32).min(self.remote_width - 1),
            (remote_y as u32).min(self.remote_height - 1),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportError {
    code: &'static str,
}

impl ViewportError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ViewportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "远程视口无效 ({})", self.code)
    }
}

impl Error for ViewportError {}
