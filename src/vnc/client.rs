//! VNC 客户端核心：TCP 连接、RFB 版本握手、安全类型协商、VNC DES 认证、
//! ClientInit/ServerInit，以及服务器消息的解析。
//! 读取走内部缓冲（RfbConn），避免逐字段系统调用；写入直通 socket。

use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};
use std::{error, fmt};

use anyhow::{bail, ensure, Context, Result};
use frd_wire_rfb::{
    self, decode_rectangle_header, decode_security_types, decode_security_types_header,
    decode_server_init, decode_server_init_header, SERVER_INIT_HEADER_BYTES,
};

pub use frd_protocol_apple::AppleConnection as RfbConn;
#[cfg(feature = "viewer")]
pub use frd_protocol_apple::AppleWriterHandle;

use super::auth;
use super::hpss;
use super::protocol::{self, PixelFormat};

#[derive(Debug)]
struct ColdDeadlineError;

impl fmt::Display for ColdDeadlineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("cold deadline")
    }
}

impl error::Error for ColdDeadlineError {}

fn cold_deadline_error() -> anyhow::Error {
    ColdDeadlineError.into()
}

pub(crate) fn is_cold_deadline_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<ColdDeadlineError>())
        || frd_protocol_apple::is_cold_deadline_error(error)
}

/// 判断错误链中是否为读超时（Windows 报 TimedOut，Unix 报 WouldBlock）
pub fn is_timeout(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<std::io::Error>())
        .any(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
        })
}

/// 判断加密会话的 TCP 对端是否发送了有序 EOF。
pub fn is_peer_closed(e: &anyhow::Error) -> bool {
    frd_protocol_apple::is_peer_closed(e)
}

// ---------- 版本握手与安全类型枚举 ----------

/// 已完成版本握手、拿到服务器安全类型列表（尚未发送密码）
pub struct Negotiated {
    pub conn: RfbConn,
    /// 服务器原始 banner（如 "RFB 003.889"，macOS 用非标准版本号）
    pub banner: String,
    /// 本次会话实际使用的协议版本
    pub version: (u8, u8),
    pub security_types: Vec<u8>,
}

/// 连接 + RFB 版本握手 + 枚举安全类型（不含认证）
pub fn negotiate(addr: &SocketAddr, connect_timeout: Duration) -> Result<Negotiated> {
    let stream = TcpStream::connect_timeout(addr, connect_timeout)
        .with_context(|| format!("连接 {addr} 失败"))?;
    negotiate_connected(stream, None, Some(addr))
}

pub fn negotiate_deadline(addr: &SocketAddr, deadline: Instant) -> Result<Negotiated> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(cold_deadline_error)?;
    let stream = TcpStream::connect_timeout(addr, remaining).context("cold tcp connect")?;
    negotiate_connected(stream, Some(deadline), None)
}

fn negotiate_connected(
    stream: TcpStream,
    absolute_deadline: Option<Instant>,
    diagnostic_addr: Option<&SocketAddr>,
) -> Result<Negotiated> {
    stream.set_nodelay(true).ok();
    // 仅 legacy 相对超时路径保留 10 秒兜底；cold 路径由绝对 deadline 独占。
    if absolute_deadline.is_none() {
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    }
    let mut conn = match absolute_deadline {
        Some(deadline) => RfbConn::new_with_deadline(stream, deadline),
        None => RfbConn::new(stream),
    };

    // 1. 版本握手：服务器发固定 ASCII banner，客户端回一个双方都支持的版本。
    let raw_banner = conn.read_vec(protocol::RFB_BANNER_BYTES)?;
    let banner = frd_wire_rfb::decode_banner(&raw_banner).with_context(|| {
        diagnostic_addr.map_or_else(
            || "服务器返回了非法 RFB banner".to_owned(),
            |addr| format!("{addr} 返回了非法 RFB banner"),
        )
    })?;
    let parsed_version = (
        banner.major.min(u16::from(u8::MAX)) as u8,
        banner.minor.min(u16::from(u8::MAX)) as u8,
    );
    // macOS 的 banner 是非标准版本号（如 003.889，minor 超出 u8）。Apple 私有会话层
    // （ClientInit 0xC1 + SelectSession → 加密会话）是版本门控的：必须原样回显服务器
    // banner 才开放；标准 3.3/3.7 按各自格式协商，其余未知版本回退 3.8。
    let version = match banner.minor {
        m if m >= u16::from(protocol::RFB_VERSION_3_8.1) => {
            // 原样回显（保持 889 之类的非标准 minor 不被降级）
            conn.write_all(&banner.wire)?;
            protocol::RFB_VERSION_3_8 // 内部逻辑按 3.8 走（安全列表 u8 计数、SecurityResult 带原因）
        }
        _ => {
            let v = match parsed_version.1 {
                minor if minor == protocol::RFB_VERSION_3_3.1 => parsed_version,
                minor if minor == protocol::RFB_VERSION_3_7.1 => parsed_version,
                _ => protocol::RFB_VERSION_3_8,
            };
            let reply = protocol::encode_rfb_banner(u16::from(v.0), u16::from(v.1))?;
            conn.write_all(&reply)?;
            v
        }
    };

    // 2. 安全类型列表。3.3 没有选择机制（服务器直接指定），
    //    3.7 用 u16 计数、3.8 用 u8 计数；计数为 0 表示失败并附带原因。
    let security_types = match version.1 {
        minor if minor == protocol::RFB_VERSION_3_3.1 => {
            let types = decode_security_types(3, &conn.read_vec(size_of::<u32>())?)?;
            if types.is_empty() {
                bail!("服务器拒绝连接: {}", read_reason(&mut conn)?);
            }
            types
        }
        minor if minor == protocol::RFB_VERSION_3_7.1 => {
            let prefix = conn.read_vec(size_of::<u16>())?;
            let (_, count) = decode_security_types_header(7, &prefix)?;
            if count == 0 {
                bail!("服务器拒绝连接: {}", read_reason(&mut conn)?);
            }
            let mut bytes = prefix;
            bytes.extend_from_slice(&conn.read_vec(count)?);
            decode_security_types(7, &bytes)?
        }
        _ => {
            let prefix = conn.read_vec(size_of::<u8>())?;
            let (_, count) = decode_security_types_header(8, &prefix)?;
            if count == 0 {
                bail!("服务器拒绝连接: {}", read_reason(&mut conn)?);
            }
            let mut bytes = prefix;
            bytes.extend_from_slice(&conn.read_vec(count)?);
            decode_security_types(8, &bytes)?
        }
    };

    Ok(Negotiated {
        conn,
        banner: banner.display,
        version,
        security_types,
    })
}

fn read_reason(conn: &mut RfbConn) -> Result<String> {
    let len = usize::try_from(conn.read_u32()?).context("RFB 安全失败原因长度无法转换为 usize")?;
    ensure!(
        len <= protocol::SECURITY_FAILURE_REASON_MAX_BYTES,
        "RFB 安全失败原因长度超过资源预算: {len}"
    );
    Ok(String::from_utf8_lossy(&conn.read_vec(len)?).into_owned())
}

const fn security_result_is_ok(result: u32) -> bool {
    result == protocol::RFB_SECURITY_RESULT_OK
}

// ---------- 认证与会话初始化 ----------

/// 选择标准 RFB 安全类型并完成认证 + ClientInit / ServerInit。
pub fn authenticate(
    neg: Negotiated,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<VncClient> {
    finish_authenticated_session(authenticate_security(neg, username, password)?)
}

/// 已完成安全类型认证、但尚未发送 ClientInit 的拥有型连接状态。
///
/// 此类型不带生命周期参数，也不保存用户名或密码；构造成功即表示调用方可以清除
/// 原始凭据，再进入可能等待 ServerInit / EncryptionInfo 的会话初始化阶段。
pub struct AuthenticatedSecurity {
    conn: RfbConn,
    choice: u8,
}

/// 选择安全类型并完成所有需要用户名/密码的认证交互。
pub fn authenticate_security(
    neg: Negotiated,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<AuthenticatedSecurity> {
    let Negotiated {
        mut conn,
        version,
        security_types,
        ..
    } = neg;

    let choice = pick_standard_security(&security_types, username, password)?;
    if version.1 != protocol::RFB_VERSION_3_3.1 {
        conn.write_all(&[choice])?;
    }

    match choice {
        protocol::security::VNC_AUTH => {
            // DES 挑战-响应
            let challenge: [u8; protocol::VNC_AUTH_CHALLENGE_BYTES] = conn
                .read_vec(protocol::VNC_AUTH_CHALLENGE_BYTES)?
                .as_slice()
                .try_into()
                .unwrap();
            let response = auth::vnc_des_challenge_response(&challenge, password.unwrap_or(""));
            conn.write_all(&response)?;
            let result = conn.read_u32()?;
            if !security_result_is_ok(result) {
                let reason = if version.1 >= protocol::RFB_VERSION_3_8.1 {
                    read_reason(&mut conn)?
                } else {
                    "密码错误".to_string()
                };
                bail!("VNC 认证失败: {reason}（注意：标准 VNC 认证只使用密码的前 8 个字符）");
            }
        }
        _ => {}
    }

    Ok(AuthenticatedSecurity { conn, choice })
}

/// 在凭据已可清除后完成标准 RFB ClientInit 与 ServerInit。
pub fn finish_authenticated_session(authenticated: AuthenticatedSecurity) -> Result<VncClient> {
    let AuthenticatedSecurity { mut conn, choice } = authenticated;

    conn.write_all(&[protocol::apple_session::SHARED_CLIENT_INIT])?;

    // ServerInit：帧缓冲尺寸 + 服务器像素格式 + 桌面名称
    let header_bytes = conn.read_vec(SERVER_INIT_HEADER_BYTES)?;
    let server_init_header = decode_server_init_header(&header_bytes)?;
    let framebuffer_width = usize::try_from(server_init_header.size.width)
        .context("ServerInit 宽度无法转换为 usize")?;
    let framebuffer_height = usize::try_from(server_init_header.size.height)
        .context("ServerInit 高度无法转换为 usize")?;
    crate::framebuffer::validate_framebuffer_geometry(framebuffer_width, framebuffer_height)
        .with_context(|| {
            format!(
                "服务器返回了无效分辨率 {}x{}",
                server_init_header.size.width, server_init_header.size.height
            )
        })?;
    let mut server_init_bytes = header_bytes;
    server_init_bytes.extend_from_slice(&conn.read_vec(server_init_header.name_length)?);
    let server_init = decode_server_init(&server_init_bytes)?;
    let width = u16::try_from(server_init.size.width).context("ServerInit 宽度超出 u16")?;
    let height = u16::try_from(server_init.size.height).context("ServerInit 高度超出 u16")?;
    let server_pf = server_init.pixel_format;
    let name = server_init.name;

    // 进入会话：清除握手阶段的兜底超时（会话读超时由调用方自行管理）
    conn.set_read_timeout(None).ok();

    Ok(VncClient {
        conn,
        used_security: choice,
        width,
        height,
        server_pf,
        name,
    })
}

fn pick_standard_security(
    types: &[u8],
    _username: Option<&str>,
    password: Option<&str>,
) -> Result<u8> {
    let has = |t: u8| types.contains(&t);
    if password.is_some() && has(protocol::security::VNC_AUTH) {
        Ok(protocol::security::VNC_AUTH)
    } else if has(protocol::security::NONE) {
        Ok(protocol::security::NONE)
    } else if has(protocol::security::VNC_AUTH) {
        bail!("服务器需要标准 VNC 密码，请通过 FRD_PASSWORD 环境变量提供")
    } else {
        bail!("标准 RFB 客户端没有可用的认证方式；Apple 私有认证必须使用 Apple 协议适配器")
    }
}

// ---------- 会话 ----------

pub struct VncClient {
    pub conn: RfbConn,
    pub used_security: u8,
    pub width: u16,
    pub height: u16,
    pub server_pf: PixelFormat,
    pub name: String,
}

pub(crate) fn sanitize_cold_connect_error(error: anyhow::Error) -> anyhow::Error {
    if is_cold_deadline_error(&error) || is_timeout(&error) {
        cold_deadline_error()
    } else {
        anyhow::anyhow!("cold connect")
    }
}

pub(crate) fn sanitize_cold_authentication_error(error: anyhow::Error) -> anyhow::Error {
    if let Some(stable) = stable_apple_security_type_error(&error) {
        stable
    } else if is_cold_deadline_error(&error) || is_timeout(&error) {
        cold_deadline_error()
    } else {
        anyhow::anyhow!("cold authentication")
    }
}

pub(crate) fn stable_apple_security_type_error(error: &anyhow::Error) -> Option<anyhow::Error> {
    (error.to_string() == frd_protocol_apple::APPLE_SECURITY_TYPE_UNAVAILABLE)
        .then(|| anyhow::anyhow!(frd_protocol_apple::APPLE_SECURITY_TYPE_UNAVAILABLE))
}

impl VncClient {
    pub fn connect_timeout(
        addr: &SocketAddr,
        timeout: Duration,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self> {
        authenticate(negotiate(addr, timeout)?, username, password)
    }

    /// 设置客户端像素格式与帧编码，必须在第一次 UpdateRequest 之前调用
    pub fn init_session(&mut self) -> Result<()> {
        if self.conn.is_encrypted() {
            // 加密会话（Apple 会话层）内：编码表已随 cmd=1 协商消息下发，
            // 再发 SetPixelFormat/SetEncodings 会被服务器拒绝；像素格式本就匹配 OURS
            return Ok(());
        }
        self.conn
            .write_all(&protocol::msg_set_pixel_format(&PixelFormat::OURS))?;
        self.conn
            .write_all(&protocol::msg_set_encodings(protocol::SUPPORTED_ENCODINGS)?)?;
        Ok(())
    }

    pub fn request_update(&mut self, incremental: bool) -> Result<()> {
        self.conn.write_all(&protocol::msg_fb_update_request(
            incremental,
            0,
            0,
            self.width,
            self.height,
        )?)
    }
}

pub fn from_apple_session(
    established: frd_protocol_apple::EstablishedAppleSession,
) -> Result<VncClient> {
    let frd_protocol_apple::EstablishedAppleSession {
        connection,
        metadata,
    } = established;
    let width = usize::try_from(metadata.size.width).context("ServerInit 宽度无法转换为 usize")?;
    let height =
        usize::try_from(metadata.size.height).context("ServerInit 高度无法转换为 usize")?;
    crate::framebuffer::validate_framebuffer_geometry(width, height).with_context(|| {
        format!(
            "服务器返回了无效分辨率 {}x{}",
            metadata.size.width, metadata.size.height
        )
    })?;
    Ok(VncClient {
        conn: connection,
        used_security: metadata.security_type,
        width: u16::try_from(metadata.size.width).context("ServerInit 宽度超出 u16")?,
        height: u16::try_from(metadata.size.height).context("ServerInit 高度超出 u16")?,
        server_pf: metadata.pixel_format,
        name: metadata.name,
    })
}

// ---------- 服务器消息 ----------

/// 一条 FramebufferUpdate 中携带的矩形操作
pub enum RectOp {
    Raw {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        /// 已按 OURS 像素格式（小端 32bpp）解码为 0x00RRGGBB 的像素
        pixels: Vec<u32>,
    },
    Copy {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        sx: usize,
        sy: usize,
    },
}

pub enum ServerEvent {
    Update(Vec<RectOp>),
    Bell,
    ServerCutText(String),
    Ignored,
}

fn validate_update_rectangle_count(count: usize) -> Result<()> {
    ensure!(
        count <= protocol::limits::MAX_RECTS_PER_UPDATE,
        "FramebufferUpdate 矩形数量超过资源预算: {count}"
    );
    Ok(())
}

fn checked_rectangle_pixel_count(width: usize, height: usize) -> Result<usize> {
    let pixels = width.checked_mul(height).context("矩形像素数量溢出")?;
    ensure!(
        pixels <= protocol::limits::MAX_FRAMEBUFFER_PIXELS,
        "矩形尺寸超过资源预算: {width}x{height}"
    );
    Ok(pixels)
}

fn checked_update_raw_bytes(
    current_bytes: usize,
    pixel_count: usize,
    bytes_per_pixel: usize,
) -> Result<usize> {
    let rectangle_bytes = pixel_count
        .checked_mul(bytes_per_pixel)
        .context("Raw 矩形字节数溢出")?;
    let total = current_bytes
        .checked_add(rectangle_bytes)
        .context("FramebufferUpdate 累计 Raw 字节数溢出")?;
    ensure!(
        total <= protocol::limits::MAX_UPDATE_RAW_BYTES,
        "FramebufferUpdate 累计 Raw 数据超过资源预算: {total} 字节"
    );
    Ok(total)
}

fn decode_raw_pixels(raw: &[u8]) -> Result<Vec<u32>> {
    let mut chunks = raw.chunks_exact(protocol::limits::RFB_BYTES_PER_PIXEL);
    ensure!(
        chunks.remainder().is_empty(),
        "Raw 像素字节数不是完整像素宽度"
    );
    let mut pixels = Vec::with_capacity(chunks.len());
    for chunk in &mut chunks {
        let pixel: [u8; protocol::limits::RFB_BYTES_PER_PIXEL] = chunk
            .try_into()
            .map_err(|_| anyhow::anyhow!("Raw 像素宽度不匹配"))?;
        pixels.push(u32::from_le_bytes(pixel));
    }
    Ok(pixels)
}

/// 阻塞读取一条服务器消息。
/// 依赖此前已通过 SetPixelFormat 声明 OURS 格式（小端 32bpp），Raw 矩形
/// 的每个像素 4 字节，可整体按小端 u32 读入。
pub fn read_server_message(conn: &mut RfbConn) -> Result<ServerEvent> {
    let message_type = conn.read_u8()?;
    if message_type == protocol::apple_session::SERVER_KEEPALIVE_MESSAGE_TYPE {
        // 0x14 是服务端单向保活通知，不是请求/响应心跳。Apple 客户端捕获未回写；
        // 真机服务端会把回写的 0x14 记为 unknown command 20 并关闭连接。
        let mut keepalive = [0u8; protocol::apple_session::SERVER_KEEPALIVE_MESSAGE_LEN];
        keepalive[0] = protocol::apple_session::SERVER_KEEPALIVE_MESSAGE_TYPE;
        conn.read_exact_bytes(
            &mut keepalive[protocol::apple_session::SERVER_KEEPALIVE_TYPE_FIELD_LEN..],
        )?;
        return Ok(ServerEvent::Ignored);
    }

    match protocol::RfbServerMessageType::try_from(message_type)? {
        protocol::RfbServerMessageType::FramebufferUpdate => {
            // FramebufferUpdate
            let _pad = conn.read_u8()?;
            let n = usize::from(conn.read_u16()?);
            validate_update_rectangle_count(n)?;
            let mut ops = Vec::with_capacity(n);
            let mut update_raw_bytes = 0usize;
            for _ in 0..n {
                let header = decode_rectangle_header(&conn.read_vec(12)?)?;
                let x = usize::from(header.x);
                let y = usize::from(header.y);
                let w = usize::from(header.width);
                let h = usize::from(header.height);
                match header.encoding {
                    protocol::RAW => {
                        let count = checked_rectangle_pixel_count(w, h)?;
                        update_raw_bytes = checked_update_raw_bytes(
                            update_raw_bytes,
                            count,
                            protocol::limits::RFB_BYTES_PER_PIXEL,
                        )?;
                        let raw_len = count * protocol::limits::RFB_BYTES_PER_PIXEL;
                        let raw = conn.read_vec(raw_len)?;
                        let pixels = decode_raw_pixels(&raw)?;
                        ensure!(pixels.len() == count, "Raw 像素数量与矩形尺寸不匹配");
                        ops.push(RectOp::Raw { x, y, w, h, pixels });
                    }
                    protocol::COPYRECT => {
                        checked_rectangle_pixel_count(w, h)?;
                        let sx = conn.read_u16()? as usize;
                        let sy = conn.read_u16()? as usize;
                        ops.push(RectOp::Copy { x, y, w, h, sx, sy });
                    }
                    hpss::encoding::MVS => {
                        bail!("流式 RFB 解析器收到 MVS；HPSS 必须使用加密应用帧模式")
                    }
                    hpss::encoding::CURSOR => {
                        // 光标：[u32 0x3e8][u32 尺寸][zlib]（尺寸含 zlib 数据）
                        let magic = conn.read_u32()?;
                        if magic != hpss::cursor::PAYLOAD_MAGIC {
                            bail!("光标 payload magic 非法: 0x{magic:08x}");
                        }
                        let zlen = usize::try_from(conn.read_u32()?)
                            .context("光标 payload 长度无法转换为 usize")?;
                        if zlen > hpss::cursor::MAX_PAYLOAD_BYTES {
                            bail!("光标 payload 超过资源预算: {zlen}");
                        }
                        conn.read_vec(zlen)?;
                    }
                    e if (hpss::encoding::APPLE_STATE_MIN..=hpss::encoding::APPLE_STATE_MAX)
                        .contains(&e) =>
                    {
                        bail!("流式 RFB 解析器收到 Apple 私有状态编码 0x{e:x}，无法安全确定边界")
                    }
                    enc => bail!("收到未请求过的帧编码 {enc}（本客户端仅请求 Raw/CopyRect）"),
                }
            }
            Ok(ServerEvent::Update(ops))
        }
        protocol::RfbServerMessageType::SetColourMapEntries => {
            // SetColourMapEntries：真彩色会话不应出现，按协议跳过
            conn.read_vec(protocol::SERVER_COLOUR_MAP_PADDING_BYTES)?;
            let n = conn.read_u16()?;
            let entries_len = usize::from(n)
                .checked_mul(protocol::SERVER_COLOUR_MAP_ENTRY_WIDTH_BYTES)
                .context("SetColourMapEntries 条目字节数溢出")?;
            conn.read_vec(entries_len)?;
            Ok(ServerEvent::Ignored)
        }
        protocol::RfbServerMessageType::Bell => Ok(ServerEvent::Bell),
        protocol::RfbServerMessageType::ServerCutText => {
            // ServerCutText
            conn.read_vec(protocol::SERVER_CUT_TEXT_PADDING_BYTES)?;
            let len =
                usize::try_from(conn.read_u32()?).context("ServerCutText 长度无法转换为 usize")?;
            if len > protocol::SERVER_CUT_TEXT_MAX_BYTES {
                bail!("ServerCutText 长度异常: {len}");
            }
            let text = String::from_utf8_lossy(&conn.read_vec(len)?).into_owned();
            Ok(ServerEvent::ServerCutText(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::Framebuffer;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn raw_pixel_decoder_preserves_exact_little_endian_bgrx_and_rejects_remainder() {
        assert_eq!(
            decode_raw_pixels(&[0x56, 0x34, 0x12, 0x00, 0xc0, 0xb0, 0xa0, 0x00]).unwrap(),
            [0x0012_3456, 0x00a0_b0c0]
        );
        assert!(decode_raw_pixels(&[0x56, 0x34, 0x12]).is_err());
    }

    #[test]
    fn security_result_owner_accepts_only_literal_success_fixture() {
        assert!(security_result_is_ok(u32::from_be_bytes([0, 0, 0, 0])));
        assert!(!security_result_is_ok(u32::from_be_bytes([0, 0, 0, 1])));
    }

    #[test]
    fn authentication_source_exposes_a_credential_free_owned_phase_boundary() {
        let production = include_str!("client.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .expect("production source prefix");
        let owned_type = ["pub struct Authenticated", "Security"].concat();
        let first_phase = ["pub fn authenticate_", "security"].concat();
        let second_phase = ["pub fn finish_", "authenticated_session"].concat();

        assert!(production.contains(&owned_type));
        assert!(production.contains(&first_phase));
        assert!(production.contains(&second_phase));
    }

    #[test]
    fn security_phase_returns_before_client_init_and_does_not_borrow_password() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (client_init_absent_tx, client_init_absent_rx) = std::sync::mpsc::channel();
        let challenge: [u8; protocol::VNC_AUTH_CHALLENGE_BYTES] =
            core::array::from_fn(|index| (index as u8).wrapping_mul(11).wrapping_add(5));

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"RFB 003.008\n").unwrap();
            let mut banner = [0_u8; protocol::RFB_BANNER_BYTES];
            stream.read_exact(&mut banner).unwrap();
            stream
                .write_all(&[1, protocol::security::VNC_AUTH])
                .unwrap();
            let mut choice = [0_u8; 1];
            stream.read_exact(&mut choice).unwrap();
            assert_eq!(choice[0], protocol::security::VNC_AUTH);
            stream.write_all(&challenge).unwrap();
            let mut response = [0_u8; protocol::VNC_AUTH_CHALLENGE_BYTES];
            stream.read_exact(&mut response).unwrap();
            assert_eq!(
                response,
                auth::vnc_des_challenge_response(&challenge, "phase-password")
            );
            stream.write_all(&0_u32.to_be_bytes()).unwrap();

            stream
                .set_read_timeout(Some(Duration::from_millis(150)))
                .unwrap();
            let mut client_init = [0_u8; 1];
            let error = stream.read_exact(&mut client_init).unwrap_err();
            assert!(matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ));
            client_init_absent_tx.send(()).unwrap();

            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream.read_exact(&mut client_init).unwrap();
            assert_eq!(client_init[0], protocol::apple_session::SHARED_CLIENT_INIT);
            let mut server_init = Vec::new();
            server_init.extend_from_slice(&8_u16.to_be_bytes());
            server_init.extend_from_slice(&4_u16.to_be_bytes());
            server_init
                .extend_from_slice(&[32, 24, 1, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
            server_init.extend_from_slice(&0_u32.to_be_bytes());
            stream.write_all(&server_init).unwrap();
        });

        let negotiated = negotiate(&address, Duration::from_secs(1)).unwrap();
        let mut password = String::from("phase-password");
        let authenticated = authenticate_security(negotiated, None, Some(&password)).unwrap();
        password.clear();
        client_init_absent_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("security phase must return without sending ClientInit");

        let client = finish_authenticated_session(authenticated).unwrap();
        assert_eq!((client.width, client.height), (8, 4));
        server.join().unwrap();
    }

    #[test]
    fn rfb_33_rejects_security_type_that_does_not_fit_u8() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"RFB 003.003\n").unwrap();
            let mut client_banner = [0u8; 12];
            stream.read_exact(&mut client_banner).unwrap();
            stream.write_all(&257u32.to_be_bytes()).unwrap();
        });

        let error = match negotiate(&address, Duration::from_secs(1)) {
            Ok(_) => panic!("RFB 3.3 security type 257 must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("超出 u8"), "{error:#}");
        server.join().unwrap();
    }

    #[test]
    fn rfb_negotiation_replies_use_literal_standard_fallback_and_apple_echo_fixtures() {
        for (server_banner, expected_reply) in [
            (*b"RFB 003.003\n", *b"RFB 003.003\n"),
            (*b"RFB 003.007\n", *b"RFB 003.007\n"),
            (*b"RFB 003.006\n", *b"RFB 003.008\n"),
            (*b"RFB 003.889\n", *b"RFB 003.889\n"),
            (*b"RFB 003.999\n", *b"RFB 003.999\n"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.write_all(&server_banner).unwrap();
                let mut reply = [0u8; 12];
                stream.read_exact(&mut reply).unwrap();
                assert_eq!(reply, expected_reply);
            });

            assert!(negotiate(&address, Duration::from_secs(1)).is_err());
            server.join().unwrap();
        }
    }

    #[test]
    fn standard_selector_directs_apple_only_offers_to_apple_adapter() {
        for security_type in [30u8, 33, 35, 36] {
            let error = pick_standard_security(&[security_type], Some("user"), Some("password"))
                .unwrap_err();
            assert!(error.to_string().contains("Apple 协议适配器"), "{error:#}");
        }
    }

    #[test]
    fn framebuffer_update_limits_reject_excessive_counts_and_bytes() {
        assert!(
            validate_update_rectangle_count(protocol::limits::MAX_RECTS_PER_UPDATE + 1).is_err()
        );
        assert!(checked_update_raw_bytes(
            protocol::limits::MAX_UPDATE_RAW_BYTES,
            1,
            protocol::limits::RFB_BYTES_PER_PIXEL,
        )
        .is_err());
    }

    #[test]
    fn server_keepalive_is_consumed_without_requesting_a_reply() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let heartbeat = [
            protocol::apple_session::SERVER_KEEPALIVE_MESSAGE_TYPE,
            1,
            2,
            3,
            4,
            5,
            6,
            7,
        ];
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&heartbeat).unwrap();
        });

        let stream = TcpStream::connect(address).unwrap();
        let mut connection = RfbConn::new(stream);
        assert!(matches!(
            read_server_message(&mut connection).unwrap(),
            ServerEvent::Ignored
        ));
        server.join().unwrap();
    }

    /// 端到端集成测试：内置一个模拟 macOS 行为的 RFB 服务器线程，
    /// 完整走一遍 版本握手 → 安全协商 → DES 认证 → ServerInit →
    /// SetPixelFormat/SetEncodings → 更新循环（Raw + CopyRect）→ PNG 导出。
    /// 这样无需真实密码即可验证认证之后的全部客户端逻辑。
    #[test]
    fn end_to_end_with_mock_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // 兜底超时：即使客户端异常退出，服务器线程也不会永久阻塞
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            // 禁用 Nagle，避免回环上小消息被拆段后与延迟 ACK 交互卡住
            s.set_nodelay(true).unwrap();
            // 1. 版本握手：RFB 规定服务器先主动发 banner，客户端再回应
            s.write_all(b"RFB 003.008\n").unwrap();
            let mut b = [0u8; 12];
            s.read_exact(&mut b).unwrap();
            assert_eq!(&b, b"RFB 003.008\n");
            // 2. 安全类型（模拟 macOS 的 [30, 2, 35] 组合）
            s.write_all(&[3, 30, 2, 35]).unwrap();
            let mut choice = [0u8; 1];
            s.read_exact(&mut choice).unwrap();
            assert_eq!(choice[0], 2);
            // 3. DES 挑战-响应
            let challenge: [u8; 16] =
                core::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(3));
            s.write_all(&challenge).unwrap();
            let mut resp = [0u8; 16];
            s.read_exact(&mut resp).unwrap();
            assert_eq!(
                resp,
                auth::vnc_des_challenge_response(&challenge, "test1234")
            );
            s.write_all(&0u32.to_be_bytes()).unwrap();
            // 4. ClientInit / ServerInit（8x4 桌面，名为 Mock Mac）
            let mut ci = [0u8; 1];
            s.read_exact(&mut ci).unwrap();
            assert_eq!(ci[0], 1);
            let mut si = Vec::new();
            si.extend_from_slice(&8u16.to_be_bytes());
            si.extend_from_slice(&4u16.to_be_bytes());
            si.extend_from_slice(&[32, 24, 1, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
            si.extend_from_slice(&8u32.to_be_bytes());
            si.extend_from_slice(b"Mock Mac");
            s.write_all(&si).unwrap();
            // 5. SetPixelFormat：客户端应声明 32bpp 小端真彩色
            let mut spf = [0u8; 20];
            s.read_exact(&mut spf).unwrap();
            assert_eq!(spf[0], 0);
            assert_eq!(spf[4], 32);
            assert_eq!(spf[6], 0); // big-endian 标志 = 0（小端）
                                   // 6. SetEncodings：应包含 Raw
            let mut hdr = [0u8; 4];
            s.read_exact(&mut hdr).unwrap();
            assert_eq!(hdr[0], 2);
            let n = u16::from_be_bytes([hdr[2], hdr[3]]) as usize;
            let mut encs = vec![0u8; 4 * n];
            s.read_exact(&mut encs).unwrap();
            assert!(encs
                .chunks(4)
                .any(|c| i32::from_be_bytes(c.try_into().unwrap()) == protocol::RAW));
            // 7. 首个全量更新请求
            let mut req = [0u8; 10];
            s.read_exact(&mut req).unwrap();
            assert_eq!(req[0], 3);
            assert_eq!(req[1], 0);
            // 8. 回一帧：Raw(0,0,4,2) 上排红下排绿 + CopyRect (0,0)→(4,0)
            let mut upd: Vec<u8> = vec![0, 0];
            upd.extend_from_slice(&2u16.to_be_bytes());
            upd.extend_from_slice(&0u16.to_be_bytes());
            upd.extend_from_slice(&0u16.to_be_bytes());
            upd.extend_from_slice(&4u16.to_be_bytes());
            upd.extend_from_slice(&2u16.to_be_bytes());
            upd.extend_from_slice(&protocol::RAW.to_be_bytes());
            for i in 0..8u32 {
                let px: u32 = if i < 4 { 0x00FF_0000 } else { 0x0000_FF00 };
                upd.extend_from_slice(&px.to_le_bytes());
            }
            upd.extend_from_slice(&4u16.to_be_bytes());
            upd.extend_from_slice(&0u16.to_be_bytes());
            upd.extend_from_slice(&4u16.to_be_bytes());
            upd.extend_from_slice(&2u16.to_be_bytes());
            upd.extend_from_slice(&protocol::COPYRECT.to_be_bytes());
            upd.extend_from_slice(&0u16.to_be_bytes());
            upd.extend_from_slice(&0u16.to_be_bytes());
            s.write_all(&upd).unwrap();
            // 9. 客户端每轮更新后应发增量请求；随后回第二个小更新（蓝色 1 像素）
            let mut req2 = [0u8; 10];
            s.read_exact(&mut req2).unwrap();
            assert_eq!(req2[0], 3);
            assert_eq!(req2[1], 1);
            let mut upd2: Vec<u8> = vec![0, 0];
            upd2.extend_from_slice(&1u16.to_be_bytes());
            upd2.extend_from_slice(&0u16.to_be_bytes());
            upd2.extend_from_slice(&0u16.to_be_bytes());
            upd2.extend_from_slice(&1u16.to_be_bytes());
            upd2.extend_from_slice(&1u16.to_be_bytes());
            upd2.extend_from_slice(&protocol::RAW.to_be_bytes());
            upd2.extend_from_slice(&0x0000_00FFu32.to_le_bytes());
            s.write_all(&upd2).unwrap();
            // 10. 收到第三次增量请求后关闭连接
            let mut req3 = [0u8; 10];
            s.read_exact(&mut req3).unwrap();
        });

        // ---- 客户端侧 ----
        let mut c =
            VncClient::connect_timeout(&addr, Duration::from_secs(5), None, Some("test1234"))
                .unwrap();
        assert_eq!((c.width, c.height, c.name.as_str()), (8, 4, "Mock Mac"));

        c.init_session().unwrap();
        let mut fb = Framebuffer::new(8, 4).unwrap();
        c.request_update(false).unwrap();
        c.conn
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();

        let mut rounds = 0usize;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match read_server_message(&mut c.conn) {
                Ok(ServerEvent::Update(ops)) => {
                    fb.apply(&ops);
                    rounds += 1;
                    c.request_update(true).unwrap();
                }
                Ok(_) => {}
                Err(_) => break, // 服务器关闭或读超时，画面已收集完毕
            }
        }
        server.join().unwrap();
        assert_eq!(rounds, 2);

        let px = fb.pixels();
        assert_eq!(px[0], 0x0000_00FF); // (0,0) 第二轮被更新为蓝色
        assert_eq!(px[3], 0x00FF_0000); // (3,0) 红
        assert_eq!(px[7], 0x00FF_0000); // (7,0) CopyRect 从 (0,0) 区域搬来的红
        assert_eq!(px[8], 0x0000_FF00); // (0,1) 绿
        assert_eq!(px[15], 0x0000_FF00); // (7,1) CopyRect 搬来的绿

        // PNG 导出（校验文件头与 IHDR 尺寸）
        let path = std::env::temp_dir().join("freeremotedesk_test.png");
        fb.save_png(&path).unwrap();
        let mut file_bytes = Vec::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_end(&mut file_bytes)
            .unwrap();
        assert_eq!(&file_bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            u32::from_be_bytes(file_bytes[16..20].try_into().unwrap()),
            8
        );
        assert_eq!(
            u32::from_be_bytes(file_bytes[20..24].try_into().unwrap()),
            4
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn absolute_deadline_updates_both_socket_timeouts_before_blocking_io() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).unwrap();
            byte[0]
        });
        let stream = TcpStream::connect(address).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut connection = RfbConn::new_with_deadline(stream, deadline);

        connection.write_all(&[0x5a]).unwrap();

        let read_timeout = connection.read_timeout().unwrap().unwrap();
        let write_timeout = connection.write_timeout().unwrap().unwrap();
        assert!(read_timeout > Duration::ZERO && read_timeout <= Duration::from_secs(2));
        assert!(write_timeout > Duration::ZERO && write_timeout <= Duration::from_secs(2));
        assert_eq!(server.join().unwrap(), 0x5a);
    }

    #[test]
    fn absolute_deadline_refreshes_both_socket_timeouts_before_each_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&[0x5a]).unwrap();
        });
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut connection = RfbConn::new_with_deadline(stream, deadline);

        assert_eq!(connection.read_u8().unwrap(), 0x5a);

        let read_timeout = connection.read_timeout().unwrap().unwrap();
        let write_timeout = connection.write_timeout().unwrap().unwrap();
        assert!(read_timeout > Duration::ZERO && read_timeout <= Duration::from_secs(2));
        assert!(write_timeout > Duration::ZERO && write_timeout <= Duration::from_secs(2));
        server.join().unwrap();
    }

    #[test]
    fn cold_negotiation_does_not_retain_the_legacy_ten_second_timeout() {
        for seconds in [15_u64, 20, 30] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.write_all(b"RFB 003.008\n").unwrap();
                let mut reply = [0_u8; 12];
                stream.read_exact(&mut reply).unwrap();
                stream.write_all(&[1, 2]).unwrap();
            });
            let stream = TcpStream::connect(address).unwrap();
            let negotiated = negotiate_connected(
                stream,
                Some(Instant::now() + Duration::from_secs(seconds)),
                None,
            )
            .unwrap();

            let read_timeout = negotiated.conn.read_timeout().unwrap().unwrap();
            let write_timeout = negotiated.conn.write_timeout().unwrap().unwrap();
            assert!(read_timeout > Duration::from_secs(10));
            assert!(write_timeout > Duration::from_secs(10));
            assert!(read_timeout <= Duration::from_secs(seconds));
            assert!(write_timeout <= Duration::from_secs(seconds));
            server.join().unwrap();
        }
    }

    #[test]
    fn legacy_negotiation_retains_its_ten_second_read_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"RFB 003.008\n").unwrap();
            let mut reply = [0_u8; 12];
            stream.read_exact(&mut reply).unwrap();
            stream.write_all(&[1, 2]).unwrap();
        });
        let stream = TcpStream::connect(address).unwrap();
        let negotiated = negotiate_connected(stream, None, Some(&address)).unwrap();

        assert_eq!(
            negotiated.conn.read_timeout().unwrap(),
            Some(Duration::from_secs(10))
        );
        assert_eq!(negotiated.conn.write_timeout().unwrap(), None);
        server.join().unwrap();
    }

    #[test]
    fn cold_connection_errors_discard_inner_target_and_credential_detail() {
        let connect = sanitize_cold_connect_error(anyhow::anyhow!(
            "fake-target-canary fake-username-canary fake-password-canary"
        ));
        let authentication = sanitize_cold_authentication_error(anyhow::anyhow!(
            "fake-target-canary fake-username-canary fake-password-canary"
        ));

        assert_eq!(connect.to_string(), "cold connect");
        assert_eq!(authentication.to_string(), "cold authentication");
        for error in [connect, authentication] {
            let output = format!("{error:#}");
            assert!(!output.contains("fake-target-canary"));
            assert!(!output.contains("fake-username-canary"));
            assert!(!output.contains("fake-password-canary"));
        }
    }

    #[test]
    fn cold_deadline_owned_timeout_kinds_survive_sanitization_as_typed_errors() {
        type Sanitizer = fn(anyhow::Error) -> anyhow::Error;
        for sanitizer in [
            sanitize_cold_connect_error as Sanitizer,
            sanitize_cold_authentication_error as Sanitizer,
        ] {
            for kind in [std::io::ErrorKind::TimedOut, std::io::ErrorKind::WouldBlock] {
                let error = sanitizer(std::io::Error::from(kind).into());
                assert_eq!(error.to_string(), "cold deadline");
                assert!(is_cold_deadline_error(&error));
                assert_eq!(error.chain().count(), 1);
            }
        }
    }

    #[test]
    fn absolute_deadline_equality_expires_before_socket_io() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let mut byte = [0u8; 1];
            stream.read(&mut byte).unwrap_err().kind()
        });
        let stream = TcpStream::connect(address).unwrap();
        let mut connection = RfbConn::new_with_deadline(stream, Instant::now());

        assert!(connection.write_all(&[0x5a]).is_err());
        assert!(matches!(
            server.join().unwrap(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn legacy_connection_keeps_no_absolute_deadline_or_forced_timeouts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).unwrap();
        });
        let stream = TcpStream::connect(address).unwrap();
        let mut connection = RfbConn::new(stream);

        connection.write_all(&[0x5a]).unwrap();

        assert!(connection.read_timeout().unwrap().is_none());
        assert!(connection.write_timeout().unwrap().is_none());
        server.join().unwrap();
    }
}
