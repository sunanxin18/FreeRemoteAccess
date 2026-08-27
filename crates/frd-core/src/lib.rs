pub mod geometry;
pub mod secret;
pub mod session;

pub use geometry::{PhysicalViewport, PixelPoint, PixelRect, PixelSize};
pub use secret::{SecretBuffer, SecretBytes};
pub use session::SessionId;

#[cfg(test)]
mod tests {
    use super::{PhysicalViewport, PixelRect, PixelSize, SecretBuffer, SessionId};

    #[test]
    fn geometry_requires_nonzero_sizes() {
        assert!(PixelSize::new(1, 1).is_some());
        assert!(PixelSize::new(0, 1).is_none());
        assert!(PixelSize::new(1, 0).is_none());
    }

    #[test]
    fn rectangle_bounds_are_checked_for_overflow() {
        let rect = PixelRect {
            x: u32::MAX,
            y: 4,
            width: 1,
            height: 2,
        };

        assert_eq!(rect.checked_bounds(), None);
    }

    #[test]
    fn rectangle_bounds_reject_zero_extents() {
        assert_eq!(
            PixelRect {
                x: 1,
                y: 2,
                width: 0,
                height: 3,
            }
            .checked_bounds(),
            None
        );
        assert_eq!(
            PixelRect {
                x: 1,
                y: 2,
                width: 3,
                height: 0,
            }
            .checked_bounds(),
            None
        );
    }

    #[test]
    fn physical_viewport_accepts_content_within_drawable() {
        let drawable = PixelSize::new(1920, 1080).expect("有效 drawable 尺寸");
        let remote = PixelSize::new(1280, 720).expect("有效远端尺寸");
        let content = PixelRect {
            x: 320,
            y: 180,
            width: 1280,
            height: 720,
        };

        assert!(PhysicalViewport::new(drawable, content, remote).is_some());
    }

    #[test]
    fn physical_viewport_rejects_content_outside_drawable() {
        let drawable = PixelSize::new(100, 100).expect("有效 drawable 尺寸");
        let remote = PixelSize::new(100, 100).expect("有效远端尺寸");
        let content = PixelRect {
            x: 50,
            y: 50,
            width: 51,
            height: 50,
        };

        assert!(PhysicalViewport::new(drawable, content, remote).is_none());
    }

    #[test]
    fn session_ids_are_nonzero_and_monotonically_allocated() {
        let first = SessionId::allocate();
        let second = SessionId::allocate();

        assert_ne!(first.get(), 0);
        assert_eq!(second.get(), first.get() + 1);
    }

    #[test]
    fn taking_secret_moves_bytes_and_empties_source() {
        let mut buffer = SecretBuffer::new(vec![0x11, 0x22]);

        let bytes = buffer.take();

        assert!(buffer.is_empty());
        assert_eq!(bytes.expose(), &[0x11, 0x22]);
    }
}
