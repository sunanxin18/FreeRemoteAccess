use std::net::{TcpListener, TcpStream};
use std::thread;

use frd_protocol_apple::{AppleConnection, SessionCrypto};

#[test]
fn one_writer_serializes_two_encrypted_messages_in_command_order() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let reader = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut connection = AppleConnection::new(stream);
        connection
            .set_crypto(SessionCrypto::from_key_iv([0x11; 16], [0x22; 16]))
            .unwrap();
        [
            connection.read_app_frame().unwrap(),
            connection.read_app_frame().unwrap(),
        ]
    });

    let mut connection = AppleConnection::new(TcpStream::connect(address).unwrap());
    connection
        .set_crypto(SessionCrypto::from_key_iv([0x11; 16], [0x22; 16]))
        .unwrap();
    let writer = connection.writer_handle().unwrap();
    writer.send_private_message(b"first").unwrap();
    writer.send_private_message(b"second").unwrap();

    assert_eq!(
        reader.join().unwrap(),
        [b"first".to_vec(), b"second".to_vec()]
    );
}
