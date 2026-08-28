use std::io::{self, Read, Write};
use std::net::Shutdown;

use ironrdp::pdu::{Action, PduHint};
use ironrdp_blocking::Framed;

pub(crate) struct OrderedRdpWriter<S> {
    framed: Framed<S>,
    writable: bool,
}

impl<S> OrderedRdpWriter<S> {
    pub(crate) fn new(framed: Framed<S>) -> Self {
        Self {
            framed,
            writable: true,
        }
    }

    pub(crate) fn is_writable(&self) -> bool {
        self.writable
    }

    pub(crate) fn stop_writes(&mut self) {
        self.writable = false;
    }

    #[cfg(test)]
    pub(crate) fn into_framed(self) -> Framed<S> {
        self.framed
    }
}

impl<S: Read> OrderedRdpWriter<S> {
    pub(crate) fn read_pdu(&mut self) -> io::Result<(Action, impl AsRef<[u8]>)> {
        self.framed.read_pdu()
    }

    pub(crate) fn read_by_hint(&mut self, hint: &dyn PduHint) -> io::Result<impl AsRef<[u8]>> {
        self.framed.read_by_hint(hint)
    }
}

impl<S: Write> OrderedRdpWriter<S> {
    pub(crate) fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "RDP writer is closed",
            ));
        }
        if let Err(error) = self.framed.write_all(frame) {
            self.writable = false;
            return Err(error);
        }
        Ok(())
    }
}

impl OrderedRdpWriter<crate::tls::TlsStream> {
    pub(crate) fn set_read_timeout(&self, timeout: std::time::Duration) -> io::Result<()> {
        self.framed
            .get_inner()
            .0
            .sock
            .set_read_timeout(Some(timeout))
    }

    pub(crate) fn shutdown(&mut self) {
        self.stop_writes();
        let _ = self.framed.get_inner().0.sock.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};

    use ironrdp_blocking::Framed;

    use super::OrderedRdpWriter;

    #[test]
    fn writer_preserves_exact_frame_order() {
        let framed = Framed::new(RecordingIo::default());
        let mut writer = OrderedRdpWriter::new(framed);

        writer.write_frame(b"first").expect("first frame writes");
        writer.write_frame(b"second").expect("second frame writes");

        assert_eq!(
            writer.into_framed().into_inner_no_leftover().written,
            b"firstsecond"
        );
    }

    #[test]
    fn writer_stops_after_the_first_transport_failure() {
        let framed = Framed::new(FailingIo::default());
        let mut writer = OrderedRdpWriter::new(framed);

        assert!(writer.write_frame(b"first").is_err());
        assert!(!writer.is_writable());
        assert!(writer.write_frame(b"second").is_err());

        let io = writer.into_framed().into_inner_no_leftover();
        assert_eq!(io.attempts, 1);
    }

    #[derive(Default)]
    struct RecordingIo {
        written: Vec<u8>,
    }

    impl Read for RecordingIo {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for RecordingIo {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailingIo {
        attempts: usize,
    }

    impl Read for FailingIo {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for FailingIo {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.attempts += 1;
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "test failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
