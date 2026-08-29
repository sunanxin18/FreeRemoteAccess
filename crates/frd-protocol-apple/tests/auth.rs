use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use frd_core::{SecretBuffer, SessionId};
use frd_frame::SurfaceUpdate;
use frd_protocol_api::{
    ConnectRequest, Credentials, Endpoint, ProtocolError, ProtocolExit, ProtocolFactory,
    ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent, SurfacePublisher,
};
use frd_protocol_apple::{
    authenticate_negotiated, select_apple_security_type, AppleConnection, AppleProtocolFactory,
    APPLE_SECURITY_TYPE_UNAVAILABLE,
};

struct AcceptEvents;

impl RuntimeEventSink for AcceptEvents {
    fn publish(&self, _: SessionEvent) -> Result<(), ProtocolError> {
        Ok(())
    }
}

struct AcceptFrames;

impl SurfacePublisher for AcceptFrames {
    fn publish(&self, _: SurfaceUpdate) -> Result<(), ProtocolError> {
        Ok(())
    }
}

struct AcceptWake;

impl RuntimeWake for AcceptWake {
    fn wake(&self) -> Result<(), ProtocolError> {
        Ok(())
    }
}

fn credentials() -> Credentials {
    let mut password = SecretBuffer::new(b"test-password".to_vec());
    Credentials {
        username: "test-user".to_owned(),
        password: password.take(),
    }
}

fn run_factory_offer(offered: Vec<u8>, read_first_byte: bool) -> (ProtocolExit, Vec<u8>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(b"RFB 003.008\n").unwrap();
        let mut echoed_banner = [0_u8; 12];
        stream.read_exact(&mut echoed_banner).unwrap();
        assert_eq!(&echoed_banner, b"RFB 003.008\n");
        stream.write_all(&[offered.len() as u8]).unwrap();
        stream.write_all(&offered).unwrap();

        if read_first_byte {
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte).unwrap();
            byte.to_vec()
        } else {
            let mut trailing = Vec::new();
            stream.read_to_end(&mut trailing).unwrap();
            trailing
        }
    });

    let session_id = SessionId::allocate();
    let runtime = ProtocolRuntime::with_ports(
        session_id,
        Box::new(AcceptEvents),
        Box::new(AcceptFrames),
        Box::new(AcceptWake),
    );
    let request = ConnectRequest {
        session_id,
        endpoint: Endpoint::new(address.ip().to_string(), address.port()).unwrap(),
        protocol_id: frd_core::ProtocolId::apple_hpss_mvs(),
        credentials: Some(credentials()),
        saved_server_pin: None,
    };
    let session = AppleProtocolFactory.create(request, runtime).unwrap();
    let exit = session.run();
    (exit, server.join().unwrap())
}

#[test]
fn strict_negotiated_auth_rejects_vnc_without_writing_selection_or_response() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut received = Vec::new();
        stream.read_to_end(&mut received).unwrap();
        received
    });
    let connection = AppleConnection::new(TcpStream::connect(address).unwrap());

    let error =
        match authenticate_negotiated(connection, (3, 8), vec![2], "test-user", "test-password") {
            Ok(_) => panic!("type 2 must not produce Apple authenticated state"),
            Err(error) => error,
        };

    assert_eq!(error.code(), Some(APPLE_SECURITY_TYPE_UNAVAILABLE));
    assert!(server.join().unwrap().is_empty());
}

#[test]
fn apple_factory_session_rejects_vnc_fallback_without_any_post_offer_bytes() {
    let (exit, post_offer) = run_factory_offer(vec![2], false);

    assert_eq!(
        exit,
        ProtocolExit::Failed(ProtocolError::adapter(
            frd_core::ProtocolId::apple_hpss_mvs(),
            "apple_high_performance_unavailable",
        ))
    );
    assert!(post_offer.is_empty());
}

#[test]
fn generic_apple_selector_preserves_36_then_33_then_30_and_never_selects_35() {
    for (offered, expected) in [
        (vec![30, 33, 36], 36),
        (vec![30, 33], 33),
        (vec![30], 30),
        (vec![35, 30], 30),
    ] {
        assert_eq!(
            select_apple_security_type(&offered, &credentials()).unwrap(),
            expected
        );
    }

    assert_eq!(
        select_apple_security_type(&[35], &credentials())
            .unwrap_err()
            .code(),
        APPLE_SECURITY_TYPE_UNAVAILABLE
    );
}

#[test]
fn apple_product_factory_rejects_legacy_offers_without_writing_selection() {
    for offered in [vec![30, 33], vec![30], vec![35, 30], vec![35]] {
        let (exit, post_offer) = run_factory_offer(offered, false);
        assert_eq!(
            exit,
            ProtocolExit::Failed(ProtocolError::adapter(
                frd_core::ProtocolId::apple_hpss_mvs(),
                "apple_high_performance_unavailable",
            ))
        );
        assert!(post_offer.is_empty());
    }
}
