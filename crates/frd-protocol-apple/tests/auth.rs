use frd_core::SecretBuffer;
use frd_protocol_api::{Credentials, ProtocolError};
use frd_protocol_apple::{AppleProtocolFactory, APPLE_SECURITY_TYPE_UNAVAILABLE};

#[test]
fn apple_factory_rejects_vnc_fallback() {
    let mut password = SecretBuffer::new(b"test-password".to_vec());
    let credentials = Credentials {
        username: "test-user".to_owned(),
        password: password.take(),
    };

    let error = AppleProtocolFactory
        .select_security_type(&[2], &credentials)
        .unwrap_err();

    assert_eq!(error.code(), APPLE_SECURITY_TYPE_UNAVAILABLE);
    assert!(matches!(error, ProtocolError::Adapter { .. }));
}
