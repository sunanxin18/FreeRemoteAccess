pub mod geometry;
pub mod secret;
pub mod session;

pub use geometry::{PhysicalViewport, PixelPoint, PixelRect, PixelSize};
pub use secret::{SecretBuffer, SecretBytes};
pub use session::SessionId;

#[cfg(test)]
mod tests {
    use super::{PixelRect, PixelSize, SecretBuffer, SessionId};

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
