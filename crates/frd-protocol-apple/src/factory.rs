use std::mem::size_of;
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{Context, Result};
use frd_protocol_api::{
    ConnectRequest, ConnectionStage, Credentials, ProtocolDescriptor, ProtocolError, ProtocolExit,
    ProtocolFactory, ProtocolRuntime, ProtocolSession, SessionEvent,
};
use frd_wire_rfb::{
    decode_banner, decode_security_types, decode_security_types_header, decode_server_init,
    decode_server_init_header, encode_banner, SERVER_INIT_HEADER_BYTES,
};

use crate::auth::{select_apple_security_type, APPLE_CREDENTIALS_REQUIRED};
use crate::connection::AppleConnection;
use crate::protocol::{self, security};
use crate::session::{self, SessionEncodingProfile};
use crate::{ard, rsa_srp, srp};

const APPLE_CONNECTION_FAILED: &str = "apple_connection_failed";
const APPLE_AUTHENTICATION_FAILED: &str = "apple_authentication_failed";
const APPLE_PROTOCOL_MISMATCH: &str = "apple_protocol_mismatch";
const SECURITY_FAILURE_REASON_MAX_BYTES: usize = 4096;

fn apple_error(code: &'static str) -> ProtocolError {
    ProtocolError::adapter(frd_core::ProtocolId::apple_hpss_mvs(), code)
}

pub struct AppleProtocolFactory;

impl AppleProtocolFactory {
    pub fn select_security_type(
        &self,
        offered: &[u8],
        credentials: &Credentials,
    ) -> Result<u8, ProtocolError> {
        select_apple_security_type(offered, credentials)
    }
}

impl ProtocolFactory for AppleProtocolFactory {
    fn descriptor(&self) -> ProtocolDescriptor {
        ProtocolDescriptor::from(frd_core::ProtocolId::apple_hpss_mvs())
    }

    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
        if request.protocol_id != frd_core::ProtocolId::apple_hpss_mvs() {
            return Err(apple_error(APPLE_PROTOCOL_MISMATCH));
        }
        let credentials = request
            .credentials
            .as_ref()
            .ok_or_else(|| apple_error(APPLE_CREDENTIALS_REQUIRED))?;
        if credentials.username.is_empty() || credentials.password.expose().is_empty() {
            return Err(apple_error(APPLE_CREDENTIALS_REQUIRED));
        }
        Ok(Box::new(AppleProtocolSession { request, runtime }))
    }
}

pub struct AppleProtocolSession {
    request: ConnectRequest,
    runtime: ProtocolRuntime,
}

impl ProtocolSession for AppleProtocolSession {
    fn run(mut self: Box<Self>) -> ProtocolExit {
        if let Err(error) = self
            .runtime
            .publish_event(SessionEvent::StageChanged(ConnectionStage::Connecting))
        {
            return ProtocolExit::Failed(error);
        }
        match connect_authenticated(&self.request) {
            Ok(_connection) => {
                if let Err(error) = self
                    .runtime
                    .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))
                {
                    return ProtocolExit::Failed(error);
                }
                ProtocolExit::Closed
            }
            Err(error) => ProtocolExit::Failed(error),
        }
    }
}

fn connect_authenticated(request: &ConnectRequest) -> Result<AppleConnection, ProtocolError> {
    let stream = TcpStream::connect((request.endpoint.host(), request.endpoint.port()))
        .map_err(|_| apple_error(APPLE_CONNECTION_FAILED))?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut connection = AppleConnection::new(stream);
    let (version, offered) =
        negotiate(&mut connection).map_err(|_| apple_error(APPLE_CONNECTION_FAILED))?;
    let credentials = request
        .credentials
        .as_ref()
        .ok_or_else(|| apple_error(APPLE_CREDENTIALS_REQUIRED))?;
    let security_type = AppleProtocolFactory.select_security_type(&offered, credentials)?;
    authenticate(&mut connection, version, security_type, credentials)
        .map_err(|_| apple_error(APPLE_AUTHENTICATION_FAILED))?;
    Ok(connection)
}

fn negotiate(connection: &mut AppleConnection) -> Result<((u8, u8), Vec<u8>)> {
    let raw_banner = connection.read_vec(frd_wire_rfb::RFB_BANNER_BYTES)?;
    let banner = decode_banner(&raw_banner)?;
    let parsed = (
        banner.major.min(u16::from(u8::MAX)) as u8,
        banner.minor.min(u16::from(u8::MAX)) as u8,
    );
    let version = if banner.minor >= 8 {
        connection.write_all(&banner.wire)?;
        (3, 8)
    } else {
        let selected = match parsed.1 {
            3 | 7 => parsed,
            _ => (3, 8),
        };
        connection.write_all(&encode_banner(selected.0.into(), selected.1.into())?)?;
        selected
    };
    let security_types = match version.1 {
        3 => {
            let types = decode_security_types(3, &connection.read_vec(size_of::<u32>())?)?;
            if types.is_empty() {
                read_reason(connection)?;
                anyhow::bail!("Apple 服务端拒绝连接");
            }
            types
        }
        7 => {
            let prefix = connection.read_vec(size_of::<u16>())?;
            let (_, count) = decode_security_types_header(7, &prefix)?;
            if count == 0 {
                read_reason(connection)?;
                anyhow::bail!("Apple 服务端拒绝连接");
            }
            let mut bytes = prefix;
            bytes.extend_from_slice(&connection.read_vec(count)?);
            decode_security_types(7, &bytes)?
        }
        _ => {
            let prefix = connection.read_vec(size_of::<u8>())?;
            let (_, count) = decode_security_types_header(8, &prefix)?;
            if count == 0 {
                read_reason(connection)?;
                anyhow::bail!("Apple 服务端拒绝连接");
            }
            let mut bytes = prefix;
            bytes.extend_from_slice(&connection.read_vec(count)?);
            decode_security_types(8, &bytes)?
        }
    };
    Ok((version, security_types))
}

fn read_reason(connection: &mut AppleConnection) -> Result<()> {
    let length = usize::try_from(connection.read_u32()?).context("安全失败原因长度无效")?;
    if length > SECURITY_FAILURE_REASON_MAX_BYTES {
        anyhow::bail!("安全失败原因超出资源预算");
    }
    let _redacted = connection.read_vec(length)?;
    Ok(())
}

fn authenticate(
    connection: &mut AppleConnection,
    version: (u8, u8),
    security_type: u8,
    credentials: &Credentials,
) -> Result<()> {
    if version.1 != 3 && security_type != security::APPLE_RSA_SRP {
        connection.write_all(&[security_type])?;
    }
    let username = credentials.username.as_str();
    let password =
        std::str::from_utf8(credentials.password.expose()).context("Apple 密码不是 UTF-8")?;
    let srp_key = match security_type {
        security::APPLE_ARD => {
            ard::authenticate(connection, username, password)?;
            None
        }
        security::APPLE_RSA_SRP => {
            rsa_srp::authenticate(connection, username, password)?;
            None
        }
        security::APPLE_SRP => Some(srp::authenticate(connection, username, password)?),
        _ => anyhow::bail!("Apple 安全类型未实现"),
    };

    let encrypted = srp_key.is_some();
    connection.write_all(&[if encrypted {
        protocol::apple_session::ENCRYPTED_SESSION_CLIENT_INIT
    } else {
        protocol::apple_session::SHARED_CLIENT_INIT
    }])?;
    let header = connection.read_vec(SERVER_INIT_HEADER_BYTES)?;
    let parsed = decode_server_init_header(&header)?;
    let mut server_init = header;
    server_init.extend_from_slice(&connection.read_vec(parsed.name_length)?);
    let _ = decode_server_init(&server_init)?;
    connection.set_read_timeout(None).ok();

    if let Some(srp_key) = srp_key {
        let crypto = session::establish_with_table(
            connection,
            &srp_key,
            SessionEncodingProfile::AppleTcpMvs,
        )?;
        connection.set_crypto(crypto)?;
    }
    Ok(())
}
