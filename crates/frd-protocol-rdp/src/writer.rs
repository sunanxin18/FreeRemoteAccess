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
        let result = self
            .framed
            .write_all(frame)
            .and_then(|()| self.framed.get_inner_mut().0.flush());
        if let Err(error) = result {
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

    pub(crate) fn set_write_timeout(&self, timeout: std::time::Duration) -> io::Result<()> {
        self.framed
            .get_inner()
            .0
            .sock
            .set_write_timeout(Some(timeout))
    }

    pub(crate) fn shutdown(&mut self) {
        self.stop_writes();
        let _ = self.framed.get_inner().0.sock.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};

    use frd_protocol_api::Endpoint;
    use ironrdp_blocking::Framed;
    use rcgen::{generate_simple_self_signed, CertifiedKey};
    use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};

    use super::OrderedRdpWriter;
    use crate::server_identity::{fingerprint_sha256, AcceptedServerIdentity};
    use crate::tls::establish_verified_tls;

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

    #[test]
    fn writer_disarms_when_rustls_style_flush_reports_deferred_failure() {
        let framed = Framed::new(DeferredFailureIo::default());
        let mut writer = OrderedRdpWriter::new(framed);

        let error = writer
            .write_frame(b"accepted plaintext")
            .expect_err("flush must surface the deferred transport failure");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        assert!(!writer.is_writable());
        assert!(writer.write_frame(b"must not be retried").is_err());

        let io = writer.into_framed().into_inner_no_leftover();
        assert_eq!((io.writes, io.flushes), (1, 1));
    }

    #[test]
    fn writer_stalled_tls_peer_times_out_disarms_and_disconnects() {
        let (address, certificate_der, release, server) = spawn_stalled_tls_peer();
        let endpoint = Endpoint::new("localhost", address.port()).expect("valid endpoint");
        let transport = establish_verified_tls(
            TcpStream::connect(address).expect("connect stalled TLS peer"),
            &endpoint,
            &AcceptedServerIdentity::ExactPin {
                fingerprint: fingerprint_sha256(&certificate_der),
            },
        )
        .expect("exact pin establishes TLS");
        let (framed, _) = transport.into_parts();
        let mut writer = OrderedRdpWriter::new(framed);
        writer
            .set_write_timeout(Duration::from_millis(100))
            .expect("configure finite write timeout");

        let payload = vec![0_u8; 32 * 1024 * 1024];
        let started = Instant::now();
        let error = writer
            .write_frame(&payload)
            .expect_err("a non-reading peer must not block the ordered writer forever");
        let elapsed = started.elapsed();

        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        assert!(elapsed < Duration::from_secs(2), "write took {elapsed:?}");
        assert!(!writer.is_writable());
        writer.shutdown();
        release.send(()).expect("release stalled peer");
        server.join().expect("stalled TLS peer exits");
    }

    fn spawn_stalled_tls_peer() -> (
        std::net::SocketAddr,
        Vec<u8>,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec!["localhost".to_owned()])
                .expect("generate runtime-only test certificate");
        let certificate_der = cert.der().to_vec();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate_der.clone())],
                PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
            )
            .expect("valid server TLS config");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind TLS test server");
        let address = listener.local_addr().expect("TLS test server address");
        let (release, released) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut tcp, _) = listener.accept().expect("accept TLS test client");
            let mut connection =
                rustls::ServerConnection::new(Arc::new(config)).expect("server connection");
            connection
                .complete_io(&mut tcp)
                .expect("complete server TLS handshake");
            released
                .recv_timeout(Duration::from_secs(3))
                .expect("client disconnects stalled peer within the test bound");
        });

        (address, certificate_der, release, server)
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

    #[derive(Default)]
    struct DeferredFailureIo {
        writes: usize,
        flushes: usize,
    }

    impl Read for DeferredFailureIo {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for DeferredFailureIo {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "deferred TLS transport failure",
            ))
        }
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
