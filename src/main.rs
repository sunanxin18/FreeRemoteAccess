//! FreeRemoteDesk —— ARP 局域网发现 + VNC(RFB) 客户端。
//! 目标场景：在 Windows 上发现并连接局域网内 macOS 的“屏幕共享”
//!（系统内置的 VNC 服务端，TCP 5900）。

mod arp;
mod framebuffer;
mod proxy;
mod vnc;

use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use framebuffer::{Framebuffer, PNG_ALPHA_OPAQUE, PNG_CHANNEL_BYTES};
use vnc::client::{self, VncClient};
use vnc::mvs::{
    MVS_RGB_BLUE_OFFSET, MVS_RGB_CHANNEL_BYTES, MVS_RGB_GREEN_OFFSET, MVS_RGB_RED_OFFSET,
};

const DEFAULT_USERNAME_ENV: &str = "FRD_USERNAME";
const DEFAULT_PASSWORD_ENV: &str = "FRD_PASSWORD";

struct CredentialValues {
    username: Option<String>,
    password: Option<String>,
}

fn read_credential_environment(name: &str, description: &str) -> Result<Option<String>> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic());
    let valid_rest = chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric());
    if !valid_start || !valid_rest {
        anyhow::bail!("{description}环境变量名无效");
    }

    match std::env::var(name) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{description}环境变量 {name} 不是有效 UTF-8")
        }
    }
}

fn load_credentials(username_env: &str, password_env: &str) -> Result<CredentialValues> {
    Ok(CredentialValues {
        username: read_credential_environment(username_env, "用户名")?,
        password: read_credential_environment(password_env, "密码")?,
    })
}

fn require_login(credentials: &CredentialValues) -> Result<(&str, &str)> {
    let username = credentials
        .username
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("缺少用户名：请通过 --username-env 指定的环境变量提供"))?;
    let password = credentials
        .password
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("缺少密码：请通过 --password-env 指定的环境变量提供"))?;
    Ok((username, password))
}

fn parse_display_scale(value: &str) -> std::result::Result<f32, String> {
    let scale = value
        .parse::<f32>()
        .map_err(|_| "显示缩放必须是有限数值，且满足 0 < scale <= 1".to_owned())?;
    if !scale.is_finite() || !(0.0 < scale && scale <= 1.0) {
        return Err("显示缩放必须是有限数值，且满足 0 < scale <= 1".to_owned());
    }
    Ok(scale)
}

fn parse_cold_capture_seconds(value: &str) -> std::result::Result<u32, String> {
    let seconds = value
        .parse::<u32>()
        .map_err(|_| "cold capture seconds 无效".to_owned())?;
    [5, 10, 15, 20, 30]
        .contains(&seconds)
        .then_some(seconds)
        .ok_or_else(|| "cold capture seconds 无效".to_owned())
}

fn parse_cold_capture_record_limit(value: &str) -> std::result::Result<u32, String> {
    let limit = value
        .parse::<u32>()
        .map_err(|_| "cold capture record limit 无效".to_owned())?;
    (1..=4096)
        .contains(&limit)
        .then_some(limit)
        .ok_or_else(|| "cold capture record limit 无效".to_owned())
}

#[derive(Parser)]
#[command(
    name = "freeremotedesk",
    version,
    about = "ARP 局域网发现 + VNC(RFB) 客户端，可连接 macOS 屏幕共享"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 创建严格 cold provenance 的 FRDMVS02 TCP-MVS 捕获
    HpssCaptureV2 {
        /// 从标准输入读取一个 FRDSTD01 凭据帧
        #[arg(long, required = true)]
        credentials_stdin_v1: bool,
        /// 新建的 FRDMVS02 输出路径
        #[arg(long)]
        out: PathBuf,
        /// 绝对捕获期限（秒）
        #[arg(long, value_parser = parse_cold_capture_seconds)]
        seconds: u32,
        /// 最大完整 MVS 记录数
        #[arg(long, value_parser = parse_cold_capture_record_limit)]
        max_records: u32,
    },
    /// 严格验证一个 FRDMVS02 cold capture
    MvsCaptureV2Verify {
        /// 输入 FRDMVS02 路径
        #[arg(long)]
        input: PathBuf,
        /// 要求 strict-cold provenance
        #[arg(long, required = true)]
        strict_cold: bool,
    },
    /// ARP 扫描局域网并探测 VNC(5900) 服务，找出 macOS 设备
    Scan {
        /// 目标网段 CIDR（如 192.168.1.0/24），默认自动检测本机 /24 网段
        #[arg(long)]
        cidr: Option<String>,
        /// 并发线程数
        #[arg(long, default_value_t = 128)]
        threads: usize,
        /// 5900 端口探测超时（毫秒）
        #[arg(long, default_value_t = 400)]
        probe_ms: u64,
        /// 只做 ARP 发现，不探测 5900 端口
        #[arg(long)]
        no_probe: bool,
    },
    /// 查看 VNC 服务器信息（协议版本 / 认证方式 / 桌面名称 / 分辨率）
    Info {
        /// 主机，如 192.168.1.20 或 192.168.1.20:5900
        host: String,
        #[arg(long, default_value_t = 5900)]
        port: u16,
        /// 提供用户名的环境变量名；存在时优先走 Apple 账号认证
        #[arg(long, value_name = "ENV", default_value = DEFAULT_USERNAME_ENV)]
        username_env: String,
        /// 提供 Mac 登录密码或 VNC 密码的环境变量名
        #[arg(long, value_name = "ENV", default_value = DEFAULT_PASSWORD_ENV)]
        password_env: String,
    },
    /// 连接并截取一帧远程屏幕，保存为 PNG
    Shot {
        host: String,
        #[arg(long, default_value_t = 5900)]
        port: u16,
        /// 提供用户名的环境变量名；存在时优先走 Apple 账号认证
        #[arg(long, value_name = "ENV", default_value = DEFAULT_USERNAME_ENV)]
        username_env: String,
        /// 提供 Mac 登录密码或 VNC 密码的环境变量名
        #[arg(long, value_name = "ENV", default_value = DEFAULT_PASSWORD_ENV)]
        password_env: String,
        /// 输出 PNG 路径
        #[arg(short, long, default_value = "screen.png")]
        out: PathBuf,
        /// 等待画面完整的最长时间（毫秒）
        #[arg(long, default_value_t = 3000)]
        wait_ms: u64,
    },
    /// 已迁移：请显式运行 frd-legacy-minifb-lab
    View {
        host: String,
        #[arg(long, default_value_t = 5900)]
        port: u16,
        /// 提供用户名的环境变量名；存在时优先走 Apple 账号认证
        #[arg(long, value_name = "ENV", default_value = DEFAULT_USERNAME_ENV)]
        username_env: String,
        /// 提供 Mac 登录密码或 VNC 密码的环境变量名
        #[arg(long, value_name = "ENV", default_value = DEFAULT_PASSWORD_ENV)]
        password_env: String,
        /// 显示缩放：1.0 原始大小；远程分辨率超过本地屏幕时可设 0.75 / 0.5
        #[arg(long, default_value_t = 1.0, value_parser = parse_display_scale)]
        scale: f32,
    },
    /// 会话捕获代理：本地监听并原样转发到目标，双向字节流落盘（逆向 ARD 用）
    Proxy {
        /// 上游真实服务器，如 192.168.1.5 或 192.168.1.5:5900
        target: String,
        #[arg(short, long, default_value_t = 5900)]
        port: u16,
        /// 本地监听地址（把 ARD 客户端指向它）
        #[arg(short, long, default_value = "127.0.0.1:15900")]
        listen: String,
        /// 记录输出目录
        #[arg(short, long, default_value = "ard_capture")]
        out: PathBuf,
    },
    /// 已迁移：请显式运行 frd-legacy-minifb-lab
    Hpssview {
        /// 主机；与 --credentials-stdin-v1 二选一
        #[arg(
            required_unless_present = "credentials_stdin_v1",
            conflicts_with = "credentials_stdin_v1"
        )]
        host: Option<String>,
        /// 从标准输入读取一个 FRDSTD01 凭据帧，避免 HOST 和凭据进入 argv
        #[arg(long)]
        credentials_stdin_v1: bool,
        #[arg(long, default_value_t = 5900, conflicts_with = "credentials_stdin_v1")]
        port: u16,
        /// 提供 Mac 用户名的环境变量名
        #[arg(
            long,
            value_name = "ENV",
            default_value = DEFAULT_USERNAME_ENV,
            conflicts_with = "credentials_stdin_v1"
        )]
        username_env: String,
        /// 提供 Mac 登录密码的环境变量名
        #[arg(
            long,
            value_name = "ENV",
            default_value = DEFAULT_PASSWORD_ENV,
            conflicts_with = "credentials_stdin_v1"
        )]
        password_env: String,
        /// 显示缩放
        #[arg(long, default_value_t = 1.0, value_parser = parse_display_scale)]
        scale: f32,
        /// 虚拟显示器名称
        #[arg(long, default_value = "屏幕共享虚拟显示器")]
        display_name: String,
        /// 启用实验性的确认驱动动态分辨率（默认关闭）
        #[arg(long)]
        dynamic_resolution: bool,
        /// 启用实验性的 UDP MediaStream 协商（P4，默认保持 TCP MVS）
        #[arg(long)]
        udp_media: bool,
        /// 已禁用：stock Apple Audio Chat 需要 IDS/Apple ID，用户名/密码 HPSS 不支持
        #[arg(long, requires = "udp_media")]
        udp_audio_input: bool,
    },
    /// HPSS 高性能屏幕共享：虚拟显示器协商 + MVS 媒体流接收（流可落盘）
    Hpss {
        host: String,
        #[arg(long, default_value_t = 5900)]
        port: u16,
        /// 提供 Mac 用户名的环境变量名
        #[arg(long, value_name = "ENV", default_value = DEFAULT_USERNAME_ENV)]
        username_env: String,
        /// 提供 Mac 登录密码的环境变量名
        #[arg(long, value_name = "ENV", default_value = DEFAULT_PASSWORD_ENV)]
        password_env: String,
        /// 接收媒体流的秒数
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// MVS 流落盘路径（FRDMVS01 版本化格式：完整矩形 x/y/w/h + payload）
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// 解码首帧为 PNG 输出路径（需同时指定 --out）
        #[arg(long)]
        png: Option<std::path::PathBuf>,
        /// 保存服务器 MediaStream Message 2 原始帧（诊断用途，不含客户端密钥）
        #[arg(long)]
        media_answer_out: Option<std::path::PathBuf>,
        /// 保存通过 SRTP 认证的已解密音频 RTP 流（FRDRTP01 诊断格式）
        #[arg(long)]
        media_audio_rtp_out: Option<std::path::PathBuf>,
        /// 保存通过 SRTP 认证的已解密视频 RTP 流（FRDVTP01 诊断格式）
        #[arg(long, requires = "udp_media")]
        media_video_rtp_out: Option<std::path::PathBuf>,
        /// 虚拟显示器名称（0x1d 携带）
        #[arg(long, default_value = "FreeRemoteDesk 虚拟显示器")]
        display_name: String,
        /// 启用实验性的 UDP MediaStream 协商（P4，默认保持 TCP MVS）
        #[arg(long)]
        udp_media: bool,
    },
    /// 加密会话验证（类型 36 SRP + Apple 会话加密层）：建链后解密并校验服务器帧
    Esess {
        host: String,
        #[arg(long, default_value_t = 5900)]
        port: u16,
        /// 提供 Mac 用户名的环境变量名
        #[arg(long, value_name = "ENV", default_value = DEFAULT_USERNAME_ENV)]
        username_env: String,
        /// 提供 Mac 登录密码的环境变量名
        #[arg(long, value_name = "ENV", default_value = DEFAULT_PASSWORD_ENV)]
        password_env: String,
        /// 持续接收解密帧的秒数
        #[arg(long, default_value_t = 8)]
        seconds: u64,
    },
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("错误: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::HpssCaptureV2 {
            credentials_stdin_v1,
            out,
            seconds,
            max_records,
        } => cmd_hpss_capture_v2(credentials_stdin_v1, &out, seconds, max_records),
        Cmd::MvsCaptureV2Verify { input, strict_cold } => {
            cmd_mvs_capture_v2_verify(&input, strict_cold)
        }
        Cmd::Scan {
            cidr,
            threads,
            probe_ms,
            no_probe,
        } => cmd_scan(cidr, threads, probe_ms, no_probe),
        Cmd::Info {
            host,
            port,
            username_env,
            password_env,
        } => {
            let credentials = load_credentials(&username_env, &password_env)?;
            cmd_info(
                &host,
                port,
                credentials.username.as_deref(),
                credentials.password.as_deref(),
            )
        }
        Cmd::Shot {
            host,
            port,
            username_env,
            password_env,
            out,
            wait_ms,
        } => {
            let credentials = load_credentials(&username_env, &password_env)?;
            cmd_shot(
                &host,
                port,
                credentials.username.as_deref(),
                credentials.password.as_deref(),
                &out,
                wait_ms,
            )
        }
        Cmd::View { .. } => anyhow::bail!(
            "旧实时查看器已移至开发工具 frd-legacy-minifb-lab；请显式运行该二进制，root CLI 不会自动启动它"
        ),
        Cmd::Proxy {
            target,
            port,
            listen,
            out,
        } => {
            let target_addr = arp::parse_target(&target, port)?;
            let listen_addr: std::net::SocketAddr = listen
                .parse()
                .map_err(|e| anyhow::anyhow!("非法监听地址 {listen}: {e}"))?;
            proxy::run(listen_addr, target_addr, out)
        }
        Cmd::Hpssview { .. } => anyhow::bail!(
            "旧实时查看器已移至开发工具 frd-legacy-minifb-lab；请显式运行该二进制，root CLI 不会自动启动它"
        ),
        Cmd::Hpss {
            host,
            port,
            username_env,
            password_env,
            seconds,
            out,
            png,
            media_answer_out,
            media_audio_rtp_out,
            media_video_rtp_out,
            display_name,
            udp_media,
        } => {
            let credentials = load_credentials(&username_env, &password_env)?;
            let (username, password) = require_login(&credentials)?;
            cmd_hpss(
                &host,
                port,
                username,
                password,
                seconds,
                out.as_deref(),
                png.as_deref(),
                media_answer_out.as_deref(),
                media_audio_rtp_out.as_deref(),
                media_video_rtp_out.as_deref(),
                &display_name,
                udp_media,
            )
        }
        Cmd::Esess {
            host,
            port,
            username_env,
            password_env,
            seconds,
        } => {
            let credentials = load_credentials(&username_env, &password_env)?;
            let (username, password) = require_login(&credentials)?;
            cmd_esess(&host, port, username, password, seconds)
        }
    }
}

fn create_cold_writer_then<T, E>(
    out: &std::path::Path,
    seconds: u32,
    max_records: u32,
    provider: impl FnOnce(
        &mut vnc::mvs_capture_v2_writer::MvsCaptureV2Writer,
    ) -> std::result::Result<T, E>,
) -> Result<(
    vnc::mvs_capture_v2_writer::MvsCaptureV2Writer,
    std::result::Result<T, E>,
)> {
    let mut writer = vnc::mvs_capture_v2_writer::MvsCaptureV2Writer::create_new(
        out,
        vnc::mvs_capture_v2_writer::CreatedConfig {
            deadline_ms: seconds * 1000,
            record_limit: max_records,
        },
    )
    .map_err(|_| anyhow::anyhow!("cold capture output"))?;
    let provider_result = provider(&mut writer);
    Ok((writer, provider_result))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ColdConnectionFailure {
    Input,
    Connect,
    Authentication,
    Deadline,
}

fn classify_cold_connection_error(error: &anyhow::Error) -> ColdConnectionFailure {
    if vnc::client::is_cold_deadline_error(error) {
        ColdConnectionFailure::Deadline
    } else if error.to_string() == "cold connect" {
        ColdConnectionFailure::Connect
    } else {
        ColdConnectionFailure::Authentication
    }
}

fn finish_cold_connection_failure(
    writer: &mut vnc::mvs_capture_v2_writer::MvsCaptureV2Writer,
    failure: ColdConnectionFailure,
) -> Result<()> {
    match failure {
        ColdConnectionFailure::Deadline => {
            let _ = writer.pre_trigger_deadline_failed();
            anyhow::bail!("cold deadline");
        }
        ColdConnectionFailure::Connect => {
            let _ = writer.connect_failed();
            anyhow::bail!("cold connect");
        }
        ColdConnectionFailure::Input | ColdConnectionFailure::Authentication => {
            let _ = writer.authentication_failed();
            anyhow::bail!("cold authentication");
        }
    }
}

fn cmd_hpss_capture_v2(
    credentials_stdin_v1: bool,
    out: &std::path::Path,
    seconds: u32,
    max_records: u32,
) -> Result<()> {
    if !credentials_stdin_v1
        || ![5, 10, 15, 20, 30].contains(&seconds)
        || !(1..=4096).contains(&max_records)
    {
        anyhow::bail!("cold capture arguments");
    }
    let (mut writer, guarded_result) = create_cold_writer_then(out, seconds, max_records, |_| {
        vnc::cold_credentials::GuardedCredentialFrame::read_process_stdin_v1()
    })?;
    let deadline = writer
        .absolute_deadline()
        .map_err(|_| anyhow::anyhow!("cold capture deadline"))?;

    let mut guarded = match guarded_result {
        Ok(guarded) => guarded,
        Err(_) => {
            let _ = writer.authentication_failed();
            anyhow::bail!("cold credential input");
        }
    };
    if !matches!(
        writer
            .pre_trigger_checkpoint()
            .map_err(|_| anyhow::anyhow!("cold capture output"))?,
        vnc::mvs_capture_v2_writer::WriterDecision::Continue
    ) {
        anyhow::bail!("cold capture deadline");
    }

    let connected = guarded.with_slices(|credentials| {
        let ip = credentials
            .host
            .parse::<std::net::IpAddr>()
            .map_err(|_| ColdConnectionFailure::Input)?;
        let address = std::net::SocketAddr::new(ip, credentials.port);
        vnc::cold_hpss::connect_deadline_opts(
            &address,
            deadline,
            credentials.username,
            credentials.password,
            vnc::session::SessionEncodingProfile::AppleTcpMvs,
        )
        .map_err(|error| classify_cold_connection_error(&error))
    });
    guarded.clear();

    let mut client = match connected {
        Ok(client) => client,
        Err(failure) => return finish_cold_connection_failure(&mut writer, failure),
    };
    if !matches!(
        writer
            .pre_trigger_checkpoint()
            .map_err(|_| anyhow::anyhow!("cold capture output"))?,
        vnc::mvs_capture_v2_writer::WriterDecision::Continue
    ) {
        anyhow::bail!("cold capture deadline");
    }
    let geometry = vnc::mvs_capture_v2::MvsCaptureV2Geometry {
        width: client.width,
        height: client.height,
    };
    vnc::cold_hpss::run_authenticated_cold_session(&mut client.conn, &mut writer, geometry)
        .map_err(|_| anyhow::anyhow!("cold session"))
}

fn cmd_mvs_capture_v2_verify(input: &std::path::Path, strict_cold: bool) -> Result<()> {
    if !strict_cold {
        anyhow::bail!("cold verify arguments");
    }
    let (structural, strict) = read_cold_capture_structural_then_strict(
        || std::fs::File::open(input).map_err(|_| anyhow::anyhow!("cold verify input")),
        |file| {
            vnc::mvs_capture_v2::read_mvs_capture_v2_structural(file)
                .map_err(|_| anyhow::anyhow!("cold verify structural"))
        },
        |file| {
            vnc::mvs_capture_v2::read_mvs_capture_v2_strict_cold(file)
                .map_err(|_| anyhow::anyhow!("cold verify strict"))
        },
    )?;
    let duration_milliseconds = strict.terminal.timestamp_us / 1000;
    println!(
        "{}",
        format_cold_verify_json(
            strict.committed_surface.width,
            strict.committed_surface.height,
            strict.terminal.record_count,
            strict.terminal.type2_count,
            strict.terminal.type0_count,
            strict.terminal.type1_count,
            strict.terminal.source_mvs_frame_count,
            structural.terminal.gap_count,
            duration_milliseconds,
        )
    );
    Ok(())
}

fn read_cold_capture_structural_then_strict<R, S, T, E>(
    mut open: impl FnMut() -> std::result::Result<R, E>,
    structural: impl FnOnce(&mut R) -> std::result::Result<S, E>,
    strict: impl FnOnce(&mut R) -> std::result::Result<T, E>,
) -> std::result::Result<(S, T), E> {
    let structural_result = {
        let mut reader = open()?;
        structural(&mut reader)?
    };
    let strict_result = {
        let mut reader = open()?;
        strict(&mut reader)?
    };
    Ok((structural_result, strict_result))
}

#[allow(clippy::too_many_arguments)]
fn format_cold_verify_json(
    width: u16,
    height: u16,
    record_count: u64,
    type_2_count: u64,
    type_0_count: u64,
    type_1_count: u64,
    source_frame_count: u64,
    gap_count: u32,
    duration_milliseconds: u64,
) -> String {
    format!(
        "{{\"status\":\"Clean\",\"terminal_category\":\"Clean\",\"width\":{width},\"height\":{height},\"record_count\":{record_count},\"type_2_count\":{type_2_count},\"type_0_count\":{type_0_count},\"type_1_count\":{type_1_count},\"source_frame_count\":{source_frame_count},\"gap_count\":{gap_count},\"duration_milliseconds\":{duration_milliseconds}}}"
    )
}

fn cmd_scan(cidr: Option<String>, threads: usize, probe_ms: u64, no_probe: bool) -> Result<()> {
    let local = arp::local_ipv4()?;
    let net = match cidr {
        Some(s) => arp::Ipv4Net::from_str(&s)?,
        None => arp::Ipv4Net {
            addr: local,
            prefix: 24,
        },
    };
    if net.prefix < 20 {
        println!("注意: {} 约含 {} 个地址，扫描可能较慢", net, net.size());
    }
    println!("本机 IP: {local}，目标网段: {net}\n");

    let mut hosts = arp::sweep(net, threads)?;
    if !no_probe {
        arp::probe_vnc_banner(&mut hosts, 5900, Duration::from_millis(probe_ms));
    }

    let self_mac = arp::arp_lookup(local);
    println!("发现 {} 台在线主机:", hosts.len());
    println!(
        "{:<16} {:<18} {:<6} VNC(5900)",
        "IP 地址", "MAC 地址", "厂商"
    );
    println!("{}", "-".repeat(76));
    for h in &hosts {
        let me = if self_mac == Some(h.mac) {
            "  <- 本机"
        } else {
            ""
        };
        println!(
            "{:<16} {:<18} {:<6} {}{}",
            h.ip,
            h.mac_str(),
            h.vendor,
            h.vnc_banner.as_deref().unwrap_or("-"),
            me
        );
    }

    if let Some(t) = hosts.iter().find(|h| h.vnc_banner.is_some()) {
        println!("\n检测到 VNC 服务器 {}，下一步：", t.ip);
        println!("  freeremotedesk.exe info {}", t.ip);
        println!("  先在进程环境中设置 FRD_PASSWORD；Apple 账号认证还需 FRD_USERNAME");
        println!("  freeremotedesk.exe shot {} -o mac.png", t.ip);
        println!("  freeremotedesk.exe view {}", t.ip);
    }
    Ok(())
}

fn cmd_info(host: &str, port: u16, username: Option<&str>, password: Option<&str>) -> Result<()> {
    let addr = arp::parse_target(host, port)?;
    let neg = client::negotiate(&addr, Duration::from_secs(5))?;

    println!("目标: {addr}");
    println!(
        "服务器协议: {}（本次会话使用 {:03}.{:03}）",
        neg.banner, neg.version.0, neg.version.1
    );
    println!("安全类型:");
    for t in &neg.security_types {
        println!("  [{}] {}", t, vnc::protocol::security_type_name(*t));
    }

    match (username, password) {
        (u, Some(p)) => {
            let c = client::authenticate(neg, u, Some(p))?;
            println!(
                "认证: 成功（类型 {} {}）",
                c.used_security,
                vnc::protocol::security_type_name(c.used_security)
            );
            println!("桌面名称: {}", c.name);
            println!("分辨率: {}x{}", c.width, c.height);
            println!(
                "服务器像素格式: bpp={} depth={} {} 大端, 真彩色={}, RGB max={}/{}/{} shift={}/{}/{}",
                c.server_pf.bits_per_pixel,
                c.server_pf.depth,
                if c.server_pf.big_endian != 0 { "是" } else { "否" },
                c.server_pf.true_colour != 0,
                c.server_pf.red_max, c.server_pf.green_max, c.server_pf.blue_max,
                c.server_pf.red_shift, c.server_pf.green_shift, c.server_pf.blue_shift
            );
        }
        (_, None) => println!(
            "\n提示: 通过 FRD_USERNAME/FRD_PASSWORD 环境变量提供 Apple 登录凭据，或仅通过 FRD_PASSWORD 提供 VNC 密码"
        ),
    }
    Ok(())
}

fn cmd_shot(
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
    out: &std::path::Path,
    wait_ms: u64,
) -> Result<()> {
    let addr = arp::parse_target(host, port)?;
    let mut c = VncClient::connect_timeout(&addr, Duration::from_secs(5), username, password)?;
    println!("已连接 {addr} — {}（{}x{}）", c.name, c.width, c.height);

    c.init_session()?;
    let mut fb = Framebuffer::new(c.width as usize, c.height as usize)?;

    // 请求全量更新，持续接收直到总时限（期间读超时只说明暂时无数据，继续等——
    // ARD 认证的会话首帧可能要等服务器端 agent 预热数秒）
    c.request_update(false)?;
    c.conn.set_read_timeout(Some(Duration::from_millis(500)))?;
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    let mut rounds = 0usize;
    while Instant::now() < deadline {
        match client::read_server_message(&mut c.conn) {
            Ok(client::ServerEvent::Update(ops)) => {
                fb.apply(&ops);
                rounds += 1;
                c.request_update(true)?; // 收集迟到的局部更新
            }
            Ok(client::ServerEvent::ServerCutText(text)) => {
                eprintln!("[远程剪贴板] {text}");
            }
            Ok(_) => {}
            Err(e) if client::is_timeout(&e) => continue,
            Err(e) => {
                // 已收到画面时，服务器主动断开（加密会话的会话层 ACK 未回应即断）
                // 视为完成；否则才是真错误
                if rounds == 0 {
                    return Err(e);
                }
                break;
            }
        }
    }

    fb.save_png(out)?;
    println!(
        "已保存 {}（{}x{}，{rounds} 轮更新）",
        out.display(),
        fb.width,
        fb.height
    );
    Ok(())
}

fn connect_apple_timeout(
    addr: &std::net::SocketAddr,
    timeout: Duration,
    username: &str,
    password: &str,
    profile: vnc::session::SessionEncodingProfile,
) -> Result<VncClient> {
    let negotiated = client::negotiate(addr, timeout)?;
    let authenticated = frd_protocol_apple::authenticate_negotiated(
        negotiated.conn,
        negotiated.version,
        negotiated.security_types,
        username,
        password,
    )
    .map_err(frd_protocol_apple::AppleHandshakeError::into_anyhow)?;
    let established = frd_protocol_apple::finish_authenticated_session(authenticated, profile)
        .map_err(frd_protocol_apple::AppleHandshakeError::into_anyhow)?;
    client::from_apple_session(established)
}

/// esess：加密会话端到端验证。
/// 建立 SRP-36 + Apple 会话加密层后，在加密帧内跑标准 RFB 消息，
/// 逐帧校验 SHA1 并统计消息类型，最后汇总解密结果。
fn cmd_esess(host: &str, port: u16, username: &str, password: &str, seconds: u64) -> Result<()> {
    let addr = arp::parse_target(host, port)?;
    let mut c = connect_apple_timeout(
        &addr,
        Duration::from_secs(5),
        username,
        password,
        vnc::session::SessionEncodingProfile::Raw,
    )?;
    println!(
        "已连接 {addr} — {}（{}x{}，认证类型 {}）",
        c.name, c.width, c.height, c.used_security
    );
    if !c.conn.is_encrypted() {
        anyhow::bail!("加密会话未建立（服务器未走类型 36？）");
    }
    println!("加密会话已挂载：后续读写全部走 EncryptOneMessage 帧（SHA1 校验）");

    // establish() 已等待服务器状态迁移
    c.request_update(false)?;
    println!("已发送 FramebufferUpdateRequest（加密帧）");

    c.conn.set_read_timeout(Some(Duration::from_millis(500)))?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let (mut updates, mut ops, mut others) = (0usize, 0usize, 0usize);
    while Instant::now() < deadline {
        match client::read_server_message(&mut c.conn) {
            Ok(client::ServerEvent::Update(o)) => {
                updates += 1;
                ops += o.len();
            }
            Ok(_) => others += 1,
            Err(e) if client::is_timeout(&e) => continue,
            Err(e) => {
                if updates + others > 0 {
                    println!("服务器结束会话（会话层消息未应答时服务器会主动断开）");
                } else {
                    eprintln!("读取失败: {e:#}");
                }
                break;
            }
        }
    }
    println!(
        "解密统计: {updates} 轮帧更新（{ops} 个矩形），{others} 条其他消息——全部通过 SHA1 校验"
    );
    if updates > 0 {
        println!("✓ 加密会话全链路验证成功（密钥派生 → EncryptionInfo → 帧解密/校验 → Raw 矩形）");
    }
    Ok(())
}

/// hpss：HPSS 高性能屏幕共享——0x1d 虚拟显示器协商 + MVS 媒体流接收统计。
use std::io::Write as _;

#[derive(Debug, Default, Eq, PartialEq)]
struct OfflineMvsStats {
    tables: usize,
    type_zero: usize,
    type_one: usize,
    state_applied: usize,
    malformed: usize,
    stale: usize,
    applied: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct OfflineMvsReplay {
    width: u16,
    height: u16,
    rgb: Vec<u8>,
    stats: OfflineMvsStats,
    applied_rects: Vec<crate::vnc::mvs_stream::MvsRect>,
}

fn apply_offline_rgb_rect(
    surface: &mut [u8],
    surface_width: u16,
    surface_height: u16,
    rect: crate::vnc::mvs_stream::MvsRect,
    rgb: &[u8],
) -> Result<()> {
    use crate::vnc::mvs;

    mvs::validate_decoded_rgb_layout(rect.width, rect.height, rgb.len())?;
    let right = rect
        .x
        .checked_add(rect.width)
        .context("离线 MVS 矩形水平边界溢出")?;
    let bottom = rect
        .y
        .checked_add(rect.height)
        .context("离线 MVS 矩形垂直边界溢出")?;
    if rect.width == 0 || rect.height == 0 || right > surface_width || bottom > surface_height {
        anyhow::bail!("离线 MVS 矩形超出协商 surface");
    }
    let row_bytes = usize::from(rect.width)
        .checked_mul(MVS_RGB_CHANNEL_BYTES)
        .context("离线 MVS 行字节数溢出")?;
    for row in 0..usize::from(rect.height) {
        let source = row * row_bytes;
        let destination = ((usize::from(rect.y) + row) * usize::from(surface_width)
            + usize::from(rect.x))
            * MVS_RGB_CHANNEL_BYTES;
        surface[destination..destination + row_bytes]
            .copy_from_slice(&rgb[source..source + row_bytes]);
    }
    Ok(())
}

fn apply_offline_partial_pixels(
    surface: &mut [u8],
    surface_width: u16,
    surface_height: u16,
    partial: &crate::vnc::mvs_full::PreparedPartialPixels,
) -> Result<()> {
    use crate::vnc::mvs_full::PartialPixelOperation;

    for operation in &partial.operations {
        match operation {
            PartialPixelOperation::Replace {
                x,
                y,
                width,
                height,
                rgb,
            } => apply_offline_rgb_rect(
                surface,
                surface_width,
                surface_height,
                crate::vnc::mvs_stream::MvsRect {
                    x: u16::try_from(*x).context("离线 MVS partial x 超出 u16")?,
                    y: u16::try_from(*y).context("离线 MVS partial y 超出 u16")?,
                    width: u16::try_from(*width).context("离线 MVS partial width 超出 u16")?,
                    height: u16::try_from(*height).context("离线 MVS partial height 超出 u16")?,
                },
                rgb,
            )?,
            PartialPixelOperation::Copy {
                source_x,
                source_y,
                destination_x,
                destination_y,
                width,
                height,
            } => {
                let surface_width = usize::from(surface_width);
                let surface_height = usize::from(surface_height);
                if source_x
                    .checked_add(*width)
                    .is_none_or(|right| right > surface_width)
                    || source_y
                        .checked_add(*height)
                        .is_none_or(|bottom| bottom > surface_height)
                    || destination_x
                        .checked_add(*width)
                        .is_none_or(|right| right > surface_width)
                    || destination_y
                        .checked_add(*height)
                        .is_none_or(|bottom| bottom > surface_height)
                {
                    anyhow::bail!("离线 MVS partial copy 超出 surface");
                }
                let row_bytes = width
                    .checked_mul(MVS_RGB_CHANNEL_BYTES)
                    .context("离线 MVS partial copy 行字节溢出")?;
                let copied_len = row_bytes
                    .checked_mul(*height)
                    .context("离线 MVS partial copy staging 长度溢出")?;
                let mut copied = Vec::new();
                copied
                    .try_reserve_exact(copied_len)
                    .context("离线 MVS partial copy staging 分配失败")?;
                for row in 0..*height {
                    let source =
                        ((*source_y + row) * surface_width + *source_x) * MVS_RGB_CHANNEL_BYTES;
                    copied.extend_from_slice(&surface[source..source + row_bytes]);
                }
                for row in 0..*height {
                    let destination = ((*destination_y + row) * surface_width + *destination_x)
                        * MVS_RGB_CHANNEL_BYTES;
                    let source = row * row_bytes;
                    surface[destination..destination + row_bytes]
                        .copy_from_slice(&copied[source..source + row_bytes]);
                }
            }
        }
    }
    Ok(())
}

fn replay_offline_mvs_records<'a, I>(
    surface_size: (u16, u16),
    generation: u64,
    records: I,
) -> Result<OfflineMvsReplay>
where
    I: IntoIterator<Item = (u64, &'a crate::vnc::mvs_stream::MvsRecord)>,
{
    use crate::vnc::mvs;

    let (width, height) = surface_size;
    let surface_len = usize::from(width)
        .checked_mul(usize::from(height))
        .and_then(|pixels| pixels.checked_mul(MVS_RGB_CHANNEL_BYTES))
        .context("离线 MVS surface 大小溢出")?;
    mvs::validate_decoded_rgb_layout(width, height, surface_len)?;
    let mut rgb = Vec::new();
    rgb.try_reserve_exact(surface_len)
        .context("无法为离线 MVS surface 预留内存")?;
    rgb.resize(surface_len, 0);

    let mut decoder = mvs::MvsDecodeState::new(generation);
    let mut stats = OfflineMvsStats::default();
    let mut applied_rects = Vec::new();
    for (record_generation, record) in records {
        if record_generation != generation {
            stats.stale += 1;
            continue;
        }
        let kind = match mvs::classify_mvs_record(record.rect, &record.payload) {
            Ok(kind) => kind,
            Err(_) => {
                stats.malformed += 1;
                decoder.request_full(generation)?;
                continue;
            }
        };
        match kind {
            mvs::MvsRecordKind::Tables(payload) => {
                stats.tables += 1;
                if decoder.install_tables(generation, payload).is_err() {
                    stats.malformed += 1;
                    decoder.request_full(generation)?;
                }
            }
            mvs::MvsRecordKind::Frame(payload) if payload.first() == Some(&1) => {
                stats.type_one += 1;
                match decoder.prepare_rect(generation, payload, record.rect, width, height) {
                    Ok(mvs::MvsDecodeDecision::PreparedOpaque(prepared)) => {
                        let has_pixels = !prepared.partial_pixels().operations.is_empty();
                        let mut staged = rgb.clone();
                        if apply_offline_partial_pixels(
                            &mut staged,
                            width,
                            height,
                            prepared.partial_pixels(),
                        )
                        .is_err()
                        {
                            stats.malformed += 1;
                            decoder.request_full(generation)?;
                            continue;
                        }
                        if decoder.commit_opaque(prepared).is_err() {
                            stats.malformed += 1;
                            decoder.request_full(generation)?;
                            continue;
                        }
                        rgb = staged;
                        stats.state_applied += 1;
                        if has_pixels {
                            stats.applied += 1;
                            applied_rects.push(record.rect);
                        }
                    }
                    Ok(mvs::MvsDecodeDecision::IgnoreStale) => stats.stale += 1,
                    Ok(mvs::MvsDecodeDecision::RequestFull(_))
                    | Ok(mvs::MvsDecodeDecision::Prepared(_))
                    | Err(_) => {
                        stats.malformed += 1;
                        decoder.request_full(generation)?;
                    }
                }
            }
            mvs::MvsRecordKind::Frame(payload) => {
                stats.type_zero += 1;
                let right = record.rect.x.checked_add(record.rect.width);
                let bottom = record.rect.y.checked_add(record.rect.height);
                if record.rect.width == 0
                    || record.rect.height == 0
                    || right.is_none_or(|right| right > width)
                    || bottom.is_none_or(|bottom| bottom > height)
                {
                    stats.malformed += 1;
                    decoder.request_full(generation)?;
                    continue;
                }
                let prepared =
                    match decoder.prepare_rect(generation, payload, record.rect, width, height) {
                        Ok(mvs::MvsDecodeDecision::Prepared(prepared)) => prepared,
                        Ok(mvs::MvsDecodeDecision::IgnoreStale) => {
                            stats.stale += 1;
                            continue;
                        }
                        Ok(mvs::MvsDecodeDecision::RequestFull(_))
                        | Ok(mvs::MvsDecodeDecision::PreparedOpaque(_))
                        | Err(_) => {
                            stats.malformed += 1;
                            decoder.request_full(generation)?;
                            continue;
                        }
                    };
                let mut staged = Vec::new();
                staged
                    .try_reserve_exact(rgb.len())
                    .context("无法为离线 MVS 事务 staging surface 预留内存")?;
                staged.extend_from_slice(&rgb);
                if apply_offline_rgb_rect(
                    &mut staged,
                    width,
                    height,
                    record.rect,
                    &prepared.decoded().rgb,
                )
                .is_err()
                {
                    stats.malformed += 1;
                    decoder.request_full(generation)?;
                    continue;
                }
                if decoder.commit(prepared).is_err() {
                    stats.malformed += 1;
                    decoder.request_full(generation)?;
                    continue;
                }
                rgb = staged;
                stats.applied += 1;
                applied_rects.push(record.rect);
            }
        }
    }
    Ok(OfflineMvsReplay {
        width,
        height,
        rgb,
        stats,
        applied_rects,
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "CLI 子命令边界显式传递用户选项，内部协议状态不依赖全局配置"
)]
fn cmd_hpss(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    seconds: u64,
    out: Option<&std::path::Path>,
    png_out: Option<&std::path::Path>,
    media_answer_out: Option<&std::path::Path>,
    media_audio_rtp_out: Option<&std::path::Path>,
    media_video_rtp_out: Option<&std::path::Path>,
    display_name: &str,
    udp_media: bool,
) -> Result<()> {
    let addr = arp::parse_target(host, port)?;
    let encoding_profile = if udp_media {
        vnc::session::SessionEncodingProfile::AppleUdpMedia
    } else {
        vnc::session::SessionEncodingProfile::AppleTcpMvs
    };
    let mut c = connect_apple_timeout(
        &addr,
        Duration::from_secs(5),
        username,
        password,
        encoding_profile,
    )?;
    println!(
        "已连接 {addr} — {}（{}x{}，认证类型 {}）",
        c.name, c.width, c.height, c.used_security
    );
    if !c.conn.is_encrypted() {
        anyhow::bail!("加密会话未建立（HPSS 依赖类型 36 加密会话）");
    }
    println!("开始 HPSS 协商（显示器名「{display_name}」，收流 {seconds}s）…");

    let mut sink = out
        .map(|p| {
            let f = std::fs::File::create(p)
                .map_err(|e| anyhow::anyhow!("创建 {} 失败: {e}", p.display()))?;
            Ok::<_, anyhow::Error>(std::io::BufWriter::new(f))
        })
        .transpose()?;

    let sess = crate::vnc::hpss::run(
        &mut c.conn,
        display_name,
        seconds,
        c.width,
        c.height,
        sink.as_mut().map(|s| s as &mut dyn std::io::Write),
        media_video_rtp_out.is_some(),
    )?;

    if let Some(mut s) = sink {
        s.flush().context("刷新 MVS 捕获文件失败")?;
    }
    require_hpss_success(&sess.stats, udp_media)?;
    if let Some(path) = media_answer_out {
        let frame = sess
            .media_answer_frame
            .as_ref()
            .context("会话未收到 MediaStream Message 2，无法保存诊断帧")?;
        std::fs::write(path, frame)
            .with_context(|| format!("保存 MediaStream Message 2 到 {} 失败", path.display()))?;
    }
    if let Some(path) = media_audio_rtp_out {
        if sess.audio_rtp_capture.is_empty() {
            anyhow::bail!("会话未收到通过 SRTP 认证的音频 RTP 包，无法保存诊断流");
        }
        std::fs::write(path, sess.audio_rtp_capture.as_bytes())
            .with_context(|| format!("保存已解密音频 RTP 流到 {} 失败", path.display()))?;
    }
    if let Some(path) = media_video_rtp_out {
        if sess.video_rtp_capture.is_empty() {
            anyhow::bail!("会话未收到通过 SRTP 认证的视频 RTP 包，无法保存诊断流");
        }
        std::fs::write(path, sess.video_rtp_capture.as_bytes())
            .with_context(|| format!("保存已解密视频 RTP 流到 {} 失败", path.display()))?;
    }
    // 尝试解码 MVS 流为 PNG（若提供了 --png）
    if let Some(png_path) = png_out {
        if let Some(sink_path) = out {
            let raw = std::fs::read(sink_path).context("读取 MVS 捕获文件失败")?;
            let records = crate::vnc::hpss::read_mvs_capture(&raw).context("MVS 捕获格式无效")?;
            let display = sess
                .display
                .context("会话未确认协商 surface，无法安全回放 MVS 捕获")?;
            let replay =
                replay_offline_mvs_records(display, 0, records.iter().map(|record| (0, record)))?;
            println!(
                "  MVS 顺序回放: tables={} type0={} type1={} partial_state_applied={} malformed={} stale={} pixel_applied={}",
                replay.stats.tables,
                replay.stats.type_zero,
                replay.stats.type_one,
                replay.stats.state_applied,
                replay.stats.malformed,
                replay.stats.stale,
                replay.stats.applied,
            );
            if replay.stats.applied == 0 {
                eprintln!("  MVS 捕获没有成功应用的 type-0 矩形，跳过 PNG 输出");
            } else {
                let rgba = rgb_to_png_rgba(&replay.rgb, replay.width, replay.height);
                let file = std::fs::File::create(png_path)
                    .with_context(|| format!("创建 PNG {} 失败", png_path.display()))?;
                let wtr = std::io::BufWriter::new(file);
                let mut encoder =
                    png::Encoder::new(wtr, u32::from(replay.width), u32::from(replay.height));
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().context("写 MVS PNG 头失败")?;
                writer
                    .write_image_data(&rgba)
                    .context("写 MVS PNG 像素失败")?;
                writer.finish().context("收尾 MVS PNG 失败")?;
                println!("  MVS 顺序回放已解码 → {}", png_path.display());
            }
        }
    }
    println!("HPSS 结果:");
    if let Some((w, h)) = sess.display {
        println!("  虚拟显示器: {w}x{h}");
    } else {
        println!("  虚拟显示器: 未确认");
    }
    let st = &sess.stats;
    println!(
        "  MVS 帧块: {}（{} 字节，表初始化 {}）",
        st.mvs_frames, st.mvs_bytes, st.table_inits
    );
    println!(
        "  光标帧: {}，状态消息: {}",
        st.cursor_frames, st.state_messages
    );
    println!(
        "  已认证 UDP 媒体: 音频 RTP {} 包 / {} 载荷字节，视频 RTP {} 包 / {} 载荷字节，RTCP {} 包",
        st.authenticated_audio_rtp_packets,
        st.authenticated_audio_rtp_payload_bytes,
        st.authenticated_video_rtp_packets,
        st.authenticated_video_rtp_payload_bytes,
        st.authenticated_rtcp_packets
    );
    if !st.unknown.is_empty() {
        println!("  未识别消息首字节: {:02x?}", st.unknown);
    }
    if udp_media {
        println!("✓ HPSS 严格 UDP 视频媒体接收成功（已认证视频 RTP）");
    } else if st.mvs_frames > 0 {
        println!("✓ HPSS 媒体流接收成功（0x1d 协商 → 0x3f3 MVS 流）");
        if let Some(p) = out {
            println!("  MVS 流已保存 {}", p.display());
        }
    }
    Ok(())
}

fn require_hpss_success(stats: &crate::vnc::hpss::HpssStats, udp_media: bool) -> Result<()> {
    if udp_media && stats.authenticated_video_rtp_packets == 0 {
        anyhow::bail!(
            "严格 HP UDP 诊断未收到已认证视频 RTP（MVS={}，Message 2={}，RTCP={}）；协商、认证控制流或 MVS 均不能替代视频数据面成功证据",
            stats.mvs_frames,
            stats.media_stream_answers,
            stats.authenticated_rtcp_packets,
        );
    }
    Ok(())
}

/// RGB u8 → PNG RGBA 像素（png crate 格式）
fn rgb_to_png_rgba(rgb: &[u8], width: u16, height: u16) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * PNG_CHANNEL_BYTES);
    let npixels = width as usize * height as usize;
    for i in 0..npixels {
        let off = i * MVS_RGB_CHANNEL_BYTES;
        if off + MVS_RGB_BLUE_OFFSET < rgb.len() {
            rgba.extend_from_slice(&[
                rgb[off + MVS_RGB_RED_OFFSET],
                rgb[off + MVS_RGB_GREEN_OFFSET],
                rgb[off + MVS_RGB_BLUE_OFFSET],
                PNG_ALPHA_OPAQUE,
            ]);
        } else {
            rgba.extend_from_slice(&[0, 0, 0, PNG_ALPHA_OPAQUE]);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::{
        classify_cold_connection_error, cmd_hpss_capture_v2, cmd_mvs_capture_v2_verify,
        create_cold_writer_then, finish_cold_connection_failure, format_cold_verify_json,
        read_cold_capture_structural_then_strict, replay_offline_mvs_records, require_hpss_success,
        rgb_to_png_rgba, Cli, Cmd, ColdConnectionFailure, DEFAULT_PASSWORD_ENV,
        DEFAULT_USERNAME_ENV,
    };
    use crate::vnc::hpss::{HpssStats, MvsCaptureWriter};
    use crate::vnc::mvs_stream::{MvsRecord, MvsRect};
    use clap::{CommandFactory, Parser};

    struct TestBitWriter {
        bytes: Vec<u8>,
        bit_len: usize,
    }

    impl TestBitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bit_len: 0,
            }
        }

        fn write_bits(&mut self, value: u32, count: u8) {
            for shift in (0..count).rev() {
                if self.bit_len % 8 == 0 {
                    self.bytes.push(0);
                }
                let bit = ((value >> shift) & 1) as u8;
                self.bytes[self.bit_len / 8] |= bit << (7 - self.bit_len % 8);
                self.bit_len += 1;
            }
        }

        fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }

    fn native_type_zero_payload(mode: u8, repeat: usize, data: Vec<u8>) -> Vec<u8> {
        let mut modes = TestBitWriter::new();
        modes.write_bits(0, 1);
        modes.write_bits(u32::from(mode), 3);
        if repeat == 0 {
            modes.write_bits(0, 1);
        } else {
            modes.write_bits(1, 1);
            modes.write_bits(u32::try_from(repeat - 1).unwrap(), 4);
        }
        modes.write_bits(0x6d, 8);
        let modes = modes.finish();
        let data_offset = 6 + modes.len();
        let mut payload = vec![
            0,
            0,
            0,
            u8::try_from(data_offset >> 16).unwrap(),
            u8::try_from((data_offset >> 8) & 0xff).unwrap(),
            u8::try_from(data_offset & 0xff).unwrap(),
        ];
        payload.extend_from_slice(&modes);
        payload.extend_from_slice(&data);
        payload
    }

    fn terminal_data() -> Vec<u8> {
        let mut data = TestBitWriter::new();
        data.write_bits(0x6d, 8);
        data.finish()
    }

    fn red_mode_four_data() -> Vec<u8> {
        let mut data = TestBitWriter::new();
        data.write_bits(0, 1);
        data.write_bits(0, 1);
        data.write_bits(100, 8);
        data.write_bits(16, 6);
        data.write_bits(48, 6);
        data.write_bits(0x6d, 8);
        data.finish()
    }

    fn native_mode_five_seed_data() -> Vec<u8> {
        let mut data = TestBitWriter::new();
        data.write_bits(0, 3);
        data.write_bits(0, 2);
        data.write_bits(0, 2);
        data.write_bits(0, 2);
        data.write_bits(0b0010, 4);
        data.write_bits(0x6d, 8);
        data.finish()
    }

    fn native_opcode_zero_partial_payload() -> Vec<u8> {
        let mut bits = TestBitWriter::new();
        bits.write_bits(0, 2);
        bits.write_bits(0x6d, 8);
        bits.write_bits(0x76, 8);
        bits.write_bits(0x73, 8);
        let mut payload = vec![1, 0, 0];
        payload.extend(bits.finish());
        payload
    }

    fn native_opcode_one_partial_payload() -> Vec<u8> {
        let mut bits = TestBitWriter::new();
        bits.write_bits(1, 2);
        bits.write_bits(0, 6);
        bits.write_bits(0, 1);
        bits.write_bits(0b1010, 4);
        bits.write_bits(0, 1);
        bits.write_bits(0b1010, 4);
        bits.write_bits(0x6d, 8);
        bits.write_bits(0x76, 8);
        bits.write_bits(0x73, 8);
        let mut payload = vec![1, 1, 1];
        payload.extend(bits.finish());
        payload
    }

    fn native_mode_six_data(index: u16) -> Vec<u8> {
        let mut data = TestBitWriter::new();
        data.write_bits(u32::from(index >> 8), 8);
        data.write_bits(u32::from(index & 0xff), 8);
        data.write_bits(0x6d, 8);
        data.finish()
    }

    #[test]
    fn offline_mvs_replay_preserves_capture_order_coordinates_and_prior_pixels() {
        let tables = {
            let mut payload = vec![0; 129];
            payload[0] = 2;
            payload
        };
        let records = vec![
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                payload: tables,
            },
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 8,
                },
                payload: native_type_zero_payload(0, 1, terminal_data()),
            },
            MvsRecord {
                rect: MvsRect {
                    x: 8,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                payload: native_type_zero_payload(4, 0, red_mode_four_data()),
            },
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                payload: native_opcode_zero_partial_payload(),
            },
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                payload: vec![0],
            },
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                payload: native_type_zero_payload(0, 0, terminal_data()),
            },
        ];
        let mut capture = Vec::new();
        {
            let mut writer = MvsCaptureWriter::new(&mut capture).unwrap();
            for record in &records {
                writer.write_record(record).unwrap();
            }
        }
        let parsed = crate::vnc::hpss::read_mvs_capture(&capture).unwrap();
        let generations = [0, 0, 0, 0, 0, 1];

        let replay =
            replay_offline_mvs_records((16, 8), 0, generations.into_iter().zip(parsed.iter()))
                .unwrap();

        assert_eq!(replay.stats.tables, 1);
        assert_eq!(replay.stats.type_zero, 2);
        assert_eq!(replay.stats.type_one, 1);
        assert_eq!(replay.stats.state_applied, 1);
        assert_eq!(replay.stats.malformed, 1);
        assert_eq!(replay.stats.stale, 1);
        assert_eq!(replay.stats.applied, 2);
        assert_eq!(replay.applied_rects, vec![records[1].rect, records[2].rect]);
        for row in 0..8 {
            let left = row * 16 * 3;
            assert!(replay.rgb[left..left + 8 * 3]
                .chunks_exact(3)
                .all(|pixel| pixel == [0xff, 0xff, 0xff]));
            let start = (row * 16 + 8) * 3;
            assert!(replay.rgb[start..start + 8 * 3]
                .chunks_exact(3)
                .all(|pixel| pixel == [190, 76, 0]));
        }
    }

    #[test]
    fn slice_d_offline_opaque_commit_preserves_pixels_and_applied_rectangles() {
        let baseline_records = offline_valid_surface_records();
        let baseline = replay_offline_mvs_records(
            (16, 8),
            0,
            baseline_records.iter().map(|record| (0, record)),
        )
        .unwrap();
        let mut with_opaque_records = baseline_records.clone();
        with_opaque_records.push(MvsRecord {
            rect: MvsRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            payload: native_opcode_zero_partial_payload(),
        });
        let with_opaque = replay_offline_mvs_records(
            (16, 8),
            0,
            with_opaque_records.iter().map(|record| (0, record)),
        )
        .unwrap();

        assert_eq!(with_opaque.stats.type_one, 1);
        assert_eq!(with_opaque.stats.state_applied, 1);
        assert_eq!(with_opaque.stats.applied, baseline.stats.applied);
        assert_eq!(with_opaque.applied_rects, baseline.applied_rects);
        assert_eq!(with_opaque.rgb, baseline.rgb);
    }

    #[test]
    fn offline_partial_pixels_and_cache_are_consumed_by_later_mode_six() {
        let mut tables = vec![1; 129];
        tables[0] = 2;
        let records = vec![
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                payload: tables,
            },
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                payload: native_type_zero_payload(5, 0, native_mode_five_seed_data()),
            },
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                payload: native_opcode_one_partial_payload(),
            },
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                payload: native_type_zero_payload(6, 0, native_mode_six_data(1)),
            },
        ];

        let replay =
            replay_offline_mvs_records((8, 8), 0, records.iter().map(|record| (0, record)))
                .unwrap();
        assert_eq!(replay.stats.tables, 1);
        assert_eq!(replay.stats.type_zero, 2);
        assert_eq!(replay.stats.type_one, 1);
        assert_eq!(replay.stats.state_applied, 1);
        assert_eq!(replay.stats.malformed, 0);
        assert_eq!(replay.stats.applied, 3);
        assert_eq!(
            replay.applied_rects,
            vec![records[1].rect, records[2].rect, records[3].rect]
        );
    }

    #[test]
    fn offline_mvs_replay_keeps_legacy_capture_rejection() {
        assert!(crate::vnc::hpss::read_mvs_capture(&[0, 0, 0, 1, 0]).is_err());
    }

    fn offline_valid_surface_records() -> Vec<MvsRecord> {
        let mut tables = vec![0; 129];
        tables[0] = 2;
        vec![
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                payload: tables,
            },
            MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 8,
                },
                payload: native_type_zero_payload(0, 1, terminal_data()),
            },
        ]
    }

    fn replay_with_invalid_offline_rect(
        invalid_rect: MvsRect,
    ) -> (super::OfflineMvsReplay, super::OfflineMvsReplay) {
        let mut records = offline_valid_surface_records();
        let baseline =
            replay_offline_mvs_records((16, 8), 0, records.iter().map(|record| (0, record)))
                .unwrap();
        records.push(MvsRecord {
            rect: invalid_rect,
            payload: native_type_zero_payload(4, 0, red_mode_four_data()),
        });
        let with_invalid =
            replay_offline_mvs_records((16, 8), 0, records.iter().map(|record| (0, record)))
                .unwrap();
        (baseline, with_invalid)
    }

    #[test]
    fn offline_out_of_bounds_record_is_malformed_and_preserves_prior_surface() {
        let (baseline, with_invalid) = replay_with_invalid_offline_rect(MvsRect {
            x: 12,
            y: 0,
            width: 8,
            height: 8,
        });

        assert_eq!(with_invalid.stats.malformed, baseline.stats.malformed + 1);
        assert_eq!(with_invalid.stats.applied, baseline.stats.applied);
        assert_eq!(with_invalid.rgb, baseline.rgb);
    }

    #[test]
    fn offline_checked_coordinate_overflow_is_malformed_and_preserves_prior_surface() {
        let (baseline, with_invalid) = replay_with_invalid_offline_rect(MvsRect {
            x: u16::MAX,
            y: 0,
            width: 8,
            height: 8,
        });

        assert_eq!(with_invalid.stats.malformed, baseline.stats.malformed + 1);
        assert_eq!(with_invalid.stats.applied, baseline.stats.applied);
        assert_eq!(with_invalid.rgb, baseline.rgb);
    }

    fn external_capture_surface_size(records: &[MvsRecord]) -> anyhow::Result<(u16, u16)> {
        let mut width = 0u16;
        let mut height = 0u16;
        for record in records
            .iter()
            .filter(|record| record.rect.width != 0 && record.rect.height != 0)
        {
            width = width.max(
                record
                    .rect
                    .x
                    .checked_add(record.rect.width)
                    .ok_or_else(|| anyhow::anyhow!("捕获矩形水平边界溢出"))?,
            );
            height = height.max(
                record
                    .rect
                    .y
                    .checked_add(record.rect.height)
                    .ok_or_else(|| anyhow::anyhow!("捕获矩形垂直边界溢出"))?,
            );
        }
        anyhow::ensure!(width != 0 && height != 0, "捕获不包含非零 MVS 矩形");
        anyhow::ensure!(
            records.iter().any(|record| {
                record.rect
                    == (MvsRect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    })
            }),
            "捕获没有可独立确认 surface 的完整矩形"
        );
        Ok((width, height))
    }

    struct ExternalMvsFailure {
        record_index: usize,
        rect: MvsRect,
        category: &'static str,
        reason: String,
    }

    fn diagnose_first_external_mvs_failure(
        records: &[MvsRecord],
        surface_size: (u16, u16),
    ) -> Option<ExternalMvsFailure> {
        use crate::vnc::mvs::{self, MvsDecodeDecision, MvsRecordKind};

        fn failure(
            record_index: usize,
            rect: MvsRect,
            category: &'static str,
            reason: impl std::fmt::Display,
        ) -> ExternalMvsFailure {
            ExternalMvsFailure {
                record_index,
                rect,
                category,
                reason: reason.to_string(),
            }
        }

        let mut decoder = mvs::MvsDecodeState::new(0);
        for (record_index, record) in records.iter().enumerate() {
            let kind = match mvs::classify_mvs_record(record.rect, &record.payload) {
                Ok(kind) => kind,
                Err(error) => {
                    return Some(failure(
                        record_index,
                        record.rect,
                        "wire-classification",
                        error,
                    ));
                }
            };
            match kind {
                MvsRecordKind::Tables(payload) => {
                    if let Err(error) = decoder.install_tables(0, payload) {
                        return Some(failure(record_index, record.rect, "table-install", error));
                    }
                }
                MvsRecordKind::Frame(payload) if payload.first() == Some(&1) => {
                    match decoder.prepare_rect(
                        0,
                        payload,
                        record.rect,
                        surface_size.0,
                        surface_size.1,
                    ) {
                        Ok(MvsDecodeDecision::PreparedOpaque(prepared)) => {
                            if let Err(error) = decoder.commit_opaque(prepared) {
                                return Some(failure(
                                    record_index,
                                    record.rect,
                                    "type1-commit",
                                    error,
                                ));
                            }
                        }
                        Ok(MvsDecodeDecision::RequestFull(reason)) => {
                            return Some(failure(
                                record_index,
                                record.rect,
                                "type1-request-full",
                                format!("{reason:?}"),
                            ));
                        }
                        Ok(MvsDecodeDecision::IgnoreStale) => {
                            return Some(failure(
                                record_index,
                                record.rect,
                                "unexpected-stale",
                                "generation 0 record was ignored",
                            ));
                        }
                        Ok(MvsDecodeDecision::Prepared(_)) => {
                            return Some(failure(
                                record_index,
                                record.rect,
                                "type1-decision",
                                "type-1 unexpectedly produced pixels",
                            ));
                        }
                        Err(error) => {
                            return Some(failure(
                                record_index,
                                record.rect,
                                "type1-prepare",
                                format!("{error:#}"),
                            ));
                        }
                    }
                }
                MvsRecordKind::Frame(payload) => {
                    if let Err(error) = crate::vnc::mvs_stream::validate_mvs_rect_against_surface(
                        record.rect,
                        surface_size.0,
                        surface_size.1,
                    ) {
                        return Some(failure(record_index, record.rect, "geometry", error));
                    }
                    let prepared = match decoder.prepare_rect(
                        0,
                        payload,
                        record.rect,
                        surface_size.0,
                        surface_size.1,
                    ) {
                        Ok(MvsDecodeDecision::Prepared(prepared)) => prepared,
                        Ok(MvsDecodeDecision::RequestFull(reason)) => {
                            return Some(failure(
                                record_index,
                                record.rect,
                                "type0-request-full",
                                format!("{reason:?}"),
                            ));
                        }
                        Ok(MvsDecodeDecision::IgnoreStale) => {
                            return Some(failure(
                                record_index,
                                record.rect,
                                "unexpected-stale",
                                "generation 0 record was ignored",
                            ));
                        }
                        Ok(MvsDecodeDecision::PreparedOpaque(_)) => {
                            return Some(failure(
                                record_index,
                                record.rect,
                                "type0-decision",
                                "type-0 unexpectedly produced opaque state",
                            ));
                        }
                        Err(error) => {
                            return Some(failure(
                                record_index,
                                record.rect,
                                "type0-prepare",
                                format!("{error:#}"),
                            ));
                        }
                    };
                    if let Err(error) = decoder.commit(prepared) {
                        return Some(failure(record_index, record.rect, "type0-commit", error));
                    }
                }
            }
        }
        None
    }

    fn write_external_mvs_png(
        path: &std::path::Path,
        width: u16,
        height: u16,
        rgb: &[u8],
    ) -> anyhow::Result<()> {
        let rgba = rgb_to_png_rgba(rgb, width, height);
        let file = std::fs::File::create(path)
            .map_err(|error| anyhow::anyhow!("创建 caller-supplied PNG 失败: {error}"))?;
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(file),
            u32::from(width),
            u32::from(height),
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&rgba)?;
        writer.finish()?;
        Ok(())
    }

    #[test]
    #[ignore = "only runs for an explicitly caller-supplied strict cold FRDMVS02 capture"]
    fn replay_external_cold_mvs_v2_capture() {
        let (Some(capture_path), Some(png_path)) = (
            std::env::var_os("FRD_MVS_CAPTURE"),
            std::env::var_os("FRD_MVS_PNG"),
        ) else {
            panic!("FRD_MVS_CAPTURE and FRD_MVS_PNG must be caller supplied");
        };
        let capture_path = std::path::PathBuf::from(capture_path);
        let png_path = std::path::PathBuf::from(png_path);
        assert!(
            capture_path != png_path,
            "caller-supplied PNG path must not replace capture input"
        );

        let mut capture = std::fs::File::open(&capture_path)
            .expect("failed to fresh-open caller-supplied FRDMVS02 capture");
        let strict = crate::vnc::mvs_capture_v2::read_mvs_capture_v2_strict_cold(&mut capture)
            .expect("caller-supplied capture failed strict cold V2 validation");
        assert_eq!(
            strict.terminal.kind,
            crate::vnc::mvs_capture_v2::MvsCaptureV2TerminalKind::Clean,
            "strict cold replay requires a Clean terminal"
        );
        assert_eq!(
            strict.terminal.gap_count, 0,
            "strict cold replay rejects diagnostic gaps"
        );
        assert!(
            strict.records.iter().all(|record| record.generation == 0),
            "strict cold replay requires generation zero"
        );
        assert!(
            strict.terminal.type2_count >= 1,
            "strict cold replay requires at least one type-2 record"
        );
        assert!(
            strict.terminal.type0_count >= 1,
            "strict cold replay requires at least one type-0 record"
        );
        assert!(
            strict.terminal.type1_count >= 1,
            "strict cold replay requires at least one type-1 record"
        );

        let surface_size = (
            strict.committed_surface.width,
            strict.committed_surface.height,
        );
        let replay = replay_offline_mvs_records(
            surface_size,
            0,
            strict
                .records
                .iter()
                .map(|record| (record.generation, &record.record)),
        )
        .expect("strict cold production replay failed");

        let first_failure = if replay.stats.malformed == 0 {
            "none".to_owned()
        } else {
            let records: Vec<_> = strict
                .records
                .iter()
                .map(|record| record.record.clone())
                .collect();
            diagnose_first_external_mvs_failure(&records, surface_size)
                .map(|failure| {
                    format!(
                        "record={} rect={:?} category={} reason={}",
                        failure.record_index, failure.rect, failure.category, failure.reason
                    )
                })
                .unwrap_or_else(|| "category=replay-summary reason=unknown".to_owned())
        };

        eprintln!(
            "Task7 strict-cold dimensions={}x{} counters: records={} type2={} type0={} type1={} partial_state={} malformed={} stale={} pixel_applied={} first_failure={}",
            replay.width,
            replay.height,
            strict.records.len(),
            replay.stats.tables,
            replay.stats.type_zero,
            replay.stats.type_one,
            replay.stats.state_applied,
            replay.stats.malformed,
            replay.stats.stale,
            replay.stats.applied,
            first_failure,
        );
        assert_eq!(
            replay.stats.tables as u64, strict.terminal.type2_count,
            "production replay type-2 count must match strict footer"
        );
        assert_eq!(
            replay.stats.type_zero as u64, strict.terminal.type0_count,
            "production replay type-0 count must match strict footer"
        );
        assert_eq!(
            replay.stats.type_one as u64, strict.terminal.type1_count,
            "production replay type-1 count must match strict footer"
        );
        assert_eq!(
            replay.stats.malformed, 0,
            "production replay rejected a record: {first_failure}"
        );
        assert_eq!(
            replay.stats.stale, 0,
            "strict generation-zero records must not be stale"
        );
        assert!(
            replay.stats.state_applied <= replay.stats.type_one,
            "type-1 records may commit only validated incremental pixel/cache state"
        );
        assert_eq!(
            replay.applied_rects.len(),
            replay.stats.applied,
            "decoded-pixel count must track committed type-0/type-1 rectangles"
        );
        assert!(
            replay.stats.applied <= replay.stats.type_zero + replay.stats.state_applied,
            "pixel application cannot exceed committed type-0 plus type-1 records"
        );
        assert!(
            replay.stats.applied >= 1,
            "PNG requires at least one successfully applied pixel record"
        );
        assert!(
            replay.rgb.chunks_exact(3).any(|pixel| pixel != [0, 0, 0]),
            "PNG requires a nonblack RGB surface"
        );
        let first_pixel = replay
            .rgb
            .chunks_exact(3)
            .next()
            .expect("strict cold replay surface must not be empty");
        assert!(
            replay.rgb.chunks_exact(3).any(|pixel| pixel != first_pixel),
            "PNG requires at least two distinct RGB colors"
        );

        write_external_mvs_png(&png_path, replay.width, replay.height, &replay.rgb)
            .expect("failed to write caller-supplied PNG");
    }

    #[test]
    #[ignore = "仅在显式提供 FRD_MVS_CAPTURE 与 FRD_MVS_PNG 时运行本地授权捕获验收"]
    fn replay_external_mvs_capture() {
        let capture_path = std::path::PathBuf::from(
            std::env::var_os("FRD_MVS_CAPTURE").expect("显式运行时必须提供 FRD_MVS_CAPTURE"),
        );
        let png_path = std::path::PathBuf::from(
            std::env::var_os("FRD_MVS_PNG").expect("显式运行时必须提供 FRD_MVS_PNG"),
        );
        assert_ne!(capture_path, png_path, "PNG 输出不得覆盖授权捕获输入");

        let raw = std::fs::read(&capture_path).expect("读取 caller-supplied MVS 捕获失败");
        let records =
            crate::vnc::hpss::read_mvs_capture(&raw).expect("caller-supplied MVS 捕获格式无效");
        let surface_size =
            external_capture_surface_size(&records).expect("无法从完整矩形确认 surface 尺寸");
        let replay =
            replay_offline_mvs_records(surface_size, 0, records.iter().map(|record| (0, record)))
                .expect("Task 6-8 顺序 replay 失败");
        let input_type_one = records
            .iter()
            .filter(|record| record.payload.first() == Some(&1))
            .count();
        let first_failure = (replay.stats.malformed != 0)
            .then(|| diagnose_first_external_mvs_failure(&records, surface_size))
            .flatten();
        let failure_summary = first_failure.as_ref().map_or_else(
            || "none".to_owned(),
            |failure| {
                format!(
                    "record={} rect={:?} category={} reason={}",
                    failure.record_index, failure.rect, failure.category, failure.reason
                )
            },
        );

        eprintln!(
            "Task9 dimensions={}x{} counters: records={} tables={} type0={} type1={} partial_state_applied={} malformed={} stale={} pixel_applied={} error_categories={}",
            replay.width,
            replay.height,
            records.len(),
            replay.stats.tables,
            replay.stats.type_zero,
            replay.stats.type_one,
            replay.stats.state_applied,
            replay.stats.malformed,
            replay.stats.stale,
            replay.stats.applied,
            failure_summary,
        );
        assert!(replay.stats.tables >= 1, "捕获必须包含至少一个 type-2 表");
        assert_eq!(
            replay.stats.malformed, 0,
            "捕获包含原生 decoder 未接受的记录，首个失败: {failure_summary}"
        );
        assert!(replay.stats.type_zero >= 1, "捕获必须包含 type-0 记录");
        assert!(
            replay.stats.applied >= 1,
            "捕获必须成功应用至少一个 type-0 记录"
        );
        assert_eq!(
            replay.stats.type_one, input_type_one,
            "type-1 输入计数必须保持完整"
        );
        assert!(
            replay.stats.applied <= replay.stats.type_zero + replay.stats.state_applied,
            "pixel_applied 不得超过已提交的 type-0/type-1 记录"
        );

        let mut pixels = replay.rgb.chunks_exact(3);
        let first = pixels.next().expect("顺序 replay surface 不能为空");
        assert!(
            replay.rgb.chunks_exact(3).any(|pixel| pixel != [0, 0, 0]),
            "顺序 replay 输出不能全黑"
        );
        assert!(
            pixels.any(|pixel| pixel != first),
            "顺序 replay 输出必须至少包含两种 RGB 颜色"
        );

        write_external_mvs_png(&png_path, replay.width, replay.height, &replay.rgb)
            .expect("写 caller-supplied PNG 失败");
    }

    #[test]
    fn rgb_channel_png_conversion_preserves_offsets_and_short_input_zero_fill() {
        assert_eq!(
            [
                crate::vnc::mvs::MVS_RGB_RED_OFFSET,
                crate::vnc::mvs::MVS_RGB_GREEN_OFFSET,
                crate::vnc::mvs::MVS_RGB_BLUE_OFFSET,
            ],
            [0, 1, 2]
        );
        assert_eq!(
            rgb_to_png_rgba(&[0x12, 0x34, 0x56, 0x78], 2, 1),
            [0x12, 0x34, 0x56, 0xff, 0, 0, 0, 0xff]
        );
    }

    #[test]
    fn udp_audio_input_without_udp_media_is_rejected_by_cli_before_dispatch() {
        let error = match Cli::try_parse_from([
            "freeremotedesk",
            "hpssview",
            "example.invalid",
            "--udp-audio-input",
        ]) {
            Ok(_) => panic!("--udp-audio-input 必须在 CLI 分派前要求 --udp-media"),
            Err(error) => error,
        };

        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn hpssview_credentials_stdin_v1_is_accepted_without_positional_host() {
        let cli = Cli::try_parse_from(["freeremotedesk", "hpssview", "--credentials-stdin-v1"])
            .expect("stdin credential mode must not put HOST in process arguments");

        let Cmd::Hpssview {
            host,
            credentials_stdin_v1,
            ..
        } = cli.cmd
        else {
            panic!("应解析为 hpssview 子命令");
        };
        assert_eq!(host, None);
        assert!(credentials_stdin_v1);
    }

    #[test]
    fn hpss_cli_accepts_video_rtp_capture_output() {
        let cli = Cli::try_parse_from([
            "freeremotedesk",
            "hpss",
            "example.invalid",
            "--media-video-rtp-out",
            "video.rtp",
            "--udp-media",
        ])
        .expect("视频 RTP 诊断落盘参数必须可解析");

        let Cmd::Hpss {
            media_video_rtp_out,
            ..
        } = cli.cmd
        else {
            panic!("应解析为 hpss 子命令");
        };
        assert_eq!(
            media_video_rtp_out,
            Some(std::path::PathBuf::from("video.rtp"))
        );

        let without_udp = Cli::try_parse_from([
            "freeremotedesk",
            "hpss",
            "example.invalid",
            "--media-video-rtp-out",
            "video.rtp",
        ]);
        assert!(without_udp.is_err());
    }

    #[test]
    fn strict_udp_hpss_rejects_mvs_and_control_evidence_without_video_rtp() {
        let stats = HpssStats {
            mvs_frames: 1,
            media_stream_answers: 1,
            authenticated_rtcp_packets: 1,
            ..HpssStats::default()
        };

        let error = require_hpss_success(&stats, true).unwrap_err();

        assert!(format!("{error:#}").contains("已认证视频 RTP"));
    }

    #[test]
    fn strict_udp_hpss_accepts_authenticated_video_rtp() {
        let stats = HpssStats {
            authenticated_video_rtp_packets: 1,
            ..HpssStats::default()
        };

        require_hpss_success(&stats, true).unwrap();
    }

    #[test]
    fn non_strict_hpss_keeps_the_mvs_diagnostic_path() {
        let stats = HpssStats {
            mvs_frames: 1,
            ..HpssStats::default()
        };

        require_hpss_success(&stats, false).unwrap();
    }

    #[test]
    fn hpssview_cli_requires_exactly_one_target_source() {
        let missing = match Cli::try_parse_from(["freeremotedesk", "hpssview"]) {
            Ok(_) => panic!("hpssview must require HOST or --credentials-stdin-v1"),
            Err(error) => error,
        };
        assert_eq!(
            missing.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );

        let conflict = match Cli::try_parse_from([
            "freeremotedesk",
            "hpssview",
            "example.invalid",
            "--credentials-stdin-v1",
        ]) {
            Ok(_) => panic!("stdin credential mode and positional HOST must be mutually exclusive"),
            Err(error) => error,
        };
        assert_eq!(conflict.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn hpssview_stdin_mode_rejects_explicit_legacy_provider_options() {
        for (option, value) in [
            ("--port", "5901"),
            ("--username-env", "FAKE_USER_ENV"),
            ("--password-env", "FAKE_PASSWORD_ENV"),
        ] {
            let error = match Cli::try_parse_from([
                "freeremotedesk",
                "hpssview",
                "--credentials-stdin-v1",
                option,
                value,
            ]) {
                Ok(_) => panic!("stdin mode must reject explicit legacy option {option}"),
                Err(error) => error,
            };
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "unexpected error kind for {option}"
            );
        }
    }

    #[test]
    fn hpssview_legacy_host_mode_remains_available() {
        let cli = Cli::try_parse_from(["freeremotedesk", "hpssview", "example.invalid"])
            .expect("legacy HOST plus environment credential mode must remain compatible");

        let Cmd::Hpssview {
            host,
            credentials_stdin_v1,
            ..
        } = cli.cmd
        else {
            panic!("应解析为 hpssview 子命令");
        };
        assert_eq!(host.as_deref(), Some("example.invalid"));
        assert!(!credentials_stdin_v1);
    }

    #[test]
    fn process_stdin_credential_dispatch_never_uses_the_global_buffered_reader() {
        let production = include_str!("main.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .expect("production source prefix");
        let forbidden = ["stdin()", ".lock()"].concat();

        assert!(
            !production.contains(&forbidden),
            "FRDSTD01 生产入口不得把凭据预读进全局 Stdin BufReader"
        );
        assert_eq!(
            production.matches("read_process_stdin_v1").count(),
            1,
            "仅 cold capture 保留无额外用户态缓冲的进程 stdin 入口"
        );
    }

    #[test]
    fn viewer_scale_is_rejected_by_cli_before_credentials_or_connection() {
        for command in ["view", "hpssview"] {
            for scale in ["NaN", "inf", "0", "-0.25", "1.01"] {
                assert!(
                    Cli::try_parse_from([
                        "freeremotedesk",
                        command,
                        "example.invalid",
                        "--scale",
                        scale,
                    ])
                    .is_err(),
                    "{command} --scale {scale} 必须在读取凭据或连接前被拒绝"
                );
            }
        }
    }

    #[test]
    fn root_viewer_commands_direct_to_the_explicit_lab_before_credential_access() {
        let expected = "旧实时查看器已移至开发工具 frd-legacy-minifb-lab；请显式运行该二进制，root CLI 不会自动启动它";
        for command in ["view", "hpssview"] {
            let cli = Cli::try_parse_from([
                "freeremotedesk",
                command,
                "example.invalid",
                "--username-env",
                "9-invalid-provider-name",
            ])
            .unwrap();

            assert_eq!(super::run(cli).unwrap_err().to_string(), expected);
        }
    }

    #[test]
    fn cli_rejects_credentials_in_process_arguments() {
        assert!(Cli::try_parse_from([
            "freeremotedesk",
            "hpss",
            "example.invalid",
            "--username",
            "private-user",
            "--password",
            "private-password",
        ])
        .is_err());
    }

    #[test]
    fn hpss_defaults_to_named_credential_environment_sources() {
        let cli = Cli::try_parse_from(["freeremotedesk", "hpss", "example.invalid"]).unwrap();
        let Cmd::Hpss {
            username_env,
            password_env,
            ..
        } = cli.cmd
        else {
            panic!("应解析为 hpss 子命令");
        };
        assert_eq!(username_env, DEFAULT_USERNAME_ENV);
        assert_eq!(password_env, DEFAULT_PASSWORD_ENV);
    }

    #[test]
    fn cold_capture_v2_cli_shape_is_exact() {
        let capture = Cli::try_parse_from([
            "freeremotedesk",
            "hpss-capture-v2",
            "--credentials-stdin-v1",
            "--out",
            "capture.mvs",
            "--seconds",
            "30",
            "--max-records",
            "4096",
        ])
        .unwrap();
        let Cmd::HpssCaptureV2 {
            credentials_stdin_v1,
            out,
            seconds,
            max_records,
        } = capture.cmd
        else {
            panic!("应解析为 hpss-capture-v2 子命令");
        };
        assert!(credentials_stdin_v1);
        assert_eq!(out, std::path::PathBuf::from("capture.mvs"));
        assert_eq!(seconds, 30);
        assert_eq!(max_records, 4096);

        let verify = Cli::try_parse_from([
            "freeremotedesk",
            "mvs-capture-v2-verify",
            "--input",
            "capture.mvs",
            "--strict-cold",
        ])
        .unwrap();
        let Cmd::MvsCaptureV2Verify { input, strict_cold } = verify.cmd else {
            panic!("应解析为 mvs-capture-v2-verify 子命令");
        };
        assert_eq!(input, std::path::PathBuf::from("capture.mvs"));
        assert!(strict_cold);
    }

    #[test]
    fn cold_capture_v2_cli_rejects_unapproved_or_out_of_range_inputs() {
        for seconds in ["4", "6", "31"] {
            assert!(Cli::try_parse_from([
                "freeremotedesk",
                "hpss-capture-v2",
                "--credentials-stdin-v1",
                "--out",
                "capture.mvs",
                "--seconds",
                seconds,
                "--max-records",
                "1",
            ])
            .is_err());
        }
        for max_records in ["0", "4097"] {
            assert!(Cli::try_parse_from([
                "freeremotedesk",
                "hpss-capture-v2",
                "--credentials-stdin-v1",
                "--out",
                "capture.mvs",
                "--seconds",
                "5",
                "--max-records",
                max_records,
            ])
            .is_err());
        }
        assert!(Cli::try_parse_from([
            "freeremotedesk",
            "hpss-capture-v2",
            "example.invalid",
            "--credentials-stdin-v1",
            "--out",
            "capture.mvs",
            "--seconds",
            "5",
            "--max-records",
            "1",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "freeremotedesk",
            "mvs-capture-v2-verify",
            "--input",
            "capture.mvs",
        ])
        .is_err());

        for forbidden in [
            "--username-env",
            "--password-env",
            "--udp-media",
            "--audio",
            "--display-name",
            "--dynamic-resolution",
            "--apple-id",
            "--remote",
            "--gui",
        ] {
            let error = match Cli::try_parse_from([
                "freeremotedesk",
                "hpss-capture-v2",
                "--credentials-stdin-v1",
                "--out",
                "capture.mvs",
                "--seconds",
                "5",
                "--max-records",
                "1",
                forbidden,
                "secret-canary",
            ]) {
                Ok(_) => panic!("forbidden cold option must be rejected"),
                Err(error) => error,
            };
            assert!(!error.to_string().contains("secret-canary"));
        }
    }

    #[test]
    fn cold_capture_v2_help_exposes_only_the_approved_fields() {
        let command = Cli::command();
        let capture = command
            .find_subcommand("hpss-capture-v2")
            .expect("capture subcommand");
        let capture_ids: std::collections::BTreeSet<_> = capture
            .get_arguments()
            .map(|arg| arg.get_id().as_str())
            .collect();
        assert_eq!(
            capture_ids,
            ["credentials_stdin_v1", "max_records", "out", "seconds",]
                .into_iter()
                .collect()
        );
        let verify = command
            .find_subcommand("mvs-capture-v2-verify")
            .expect("verify subcommand");
        let verify_ids: std::collections::BTreeSet<_> = verify
            .get_arguments()
            .map(|arg| arg.get_id().as_str())
            .collect();
        assert_eq!(verify_ids, ["input", "strict_cold"].into_iter().collect());
    }

    #[test]
    fn cold_capture_v2_invalid_limits_fail_before_output_or_stdin() {
        let path = std::env::temp_dir().join(format!(
            "freeremotedesk-cold-invalid-{}.mvs",
            std::process::id()
        ));
        assert!(!path.exists(), "test output path must start absent");
        assert!(cmd_hpss_capture_v2(true, &path, 6, 1).is_err());
        assert!(!path.exists(), "invalid limits must not create output");
        assert!(cmd_hpss_capture_v2(true, &path, 5, 0).is_err());
        assert!(
            !path.exists(),
            "invalid limits must not read stdin or create output"
        );
    }

    #[test]
    fn cold_capture_v2_created_is_flushed_before_provider_callback() {
        let path = std::env::temp_dir().join(format!(
            "freeremotedesk-cold-order-{}.mvs",
            std::process::id()
        ));
        assert!(!path.exists(), "test output path must start absent");
        let (mut writer, provider_result) = create_cold_writer_then(&path, 5, 1, |_| {
            assert_eq!(std::fs::metadata(&path).unwrap().len(), 80);
            Ok::<_, ()>("provider-called")
        })
        .unwrap();
        assert_eq!(provider_result.unwrap(), "provider-called");
        writer.authentication_failed().unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        let structural =
            crate::vnc::mvs_capture_v2::read_mvs_capture_v2_structural(&mut file).unwrap();
        assert_eq!(
            structural.terminal.reason,
            crate::vnc::mvs_capture_v2::MvsCaptureV2TerminalReason::CredentialOrAuthenticationFailure
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cold_capture_v2_child_file_errors_are_category_only() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let collision = std::env::temp_dir().join(format!(
            "fake-target-canary-fake-credential-canary-{}-{unique}.mvs",
            std::process::id()
        ));
        std::fs::write(&collision, b"collision").unwrap();

        let capture_error = cmd_hpss_capture_v2(true, &collision, 5, 1).unwrap_err();
        assert_eq!(capture_error.to_string(), "cold capture output");
        assert!(!format!("{capture_error:#}").contains("fake-target-canary"));
        assert!(!format!("{capture_error:#}").contains("fake-credential-canary"));
        std::fs::remove_file(&collision).unwrap();

        let verify_error = cmd_mvs_capture_v2_verify(&collision, true).unwrap_err();
        assert_eq!(verify_error.to_string(), "cold verify input");
        assert!(!format!("{verify_error:#}").contains("fake-target-canary"));
        assert!(!format!("{verify_error:#}").contains("fake-credential-canary"));
    }

    #[test]
    fn cold_capture_v2_deadline_error_routes_to_pre_trigger_terminal_267_before_equality() {
        let error = match crate::vnc::cold_hpss::connect_deadline_opts(
            &std::net::SocketAddr::from(([127, 0, 0, 1], 9)),
            std::time::Instant::now(),
            "u",
            "p",
            crate::vnc::session::SessionEncodingProfile::AppleTcpMvs,
        ) {
            Ok(_) => panic!("equality deadline must fail before connect"),
            Err(error) => error,
        };
        assert_eq!(
            classify_cold_connection_error(&error),
            ColdConnectionFailure::Deadline
        );

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "freeremotedesk-cold-deadline-{}-{unique}.mvs",
            std::process::id()
        ));
        let mut writer = crate::vnc::mvs_capture_v2_writer::MvsCaptureV2Writer::create_new(
            &path,
            crate::vnc::mvs_capture_v2_writer::CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();
        let outward = finish_cold_connection_failure(&mut writer, ColdConnectionFailure::Deadline)
            .unwrap_err();
        assert_eq!(outward.to_string(), "cold deadline");
        let structural = crate::vnc::mvs_capture_v2::read_mvs_capture_v2_structural(
            &mut std::fs::File::open(&path).unwrap(),
        )
        .unwrap();
        assert_eq!(
            structural.terminal.reason,
            crate::vnc::mvs_capture_v2::MvsCaptureV2TerminalReason::PreTriggerDeadline
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cold_capture_v2_verifier_schema_has_only_the_frozen_keys() {
        assert_eq!(
            format_cold_verify_json(640, 480, 3, 1, 1, 1, 4, 0, 5000),
            "{\"status\":\"Clean\",\"terminal_category\":\"Clean\",\"width\":640,\"height\":480,\"record_count\":3,\"type_2_count\":1,\"type_0_count\":1,\"type_1_count\":1,\"source_frame_count\":4,\"gap_count\":0,\"duration_milliseconds\":5000}"
        );
    }

    #[test]
    fn cold_capture_v2_verifier_reopens_after_structural_validation() {
        let mut opens = 0_u8;
        let phases = std::cell::RefCell::new(Vec::new());
        let (structural, strict) = read_cold_capture_structural_then_strict(
            || {
                opens += 1;
                Ok::<_, anyhow::Error>(opens)
            },
            |reader| {
                phases.borrow_mut().push("structural");
                assert_eq!(*reader, 1);
                Ok::<_, anyhow::Error>("structural-result")
            },
            |reader| {
                assert_eq!(*phases.borrow(), ["structural"]);
                phases.borrow_mut().push("strict");
                assert_eq!(*reader, 2);
                Ok::<_, anyhow::Error>("strict-result")
            },
        )
        .unwrap();
        assert_eq!((structural, strict), ("structural-result", "strict-result"));
        assert_eq!(opens, 2);
        assert_eq!(*phases.borrow(), ["structural", "strict"]);
    }

    mod main {
        mod tests {
            use super::super::{Cli, Cmd};
            use clap::Parser;

            #[test]
            fn cold_capture_v2_filter_runs_exact_cli_contract() {
                let cli = Cli::try_parse_from([
                    "freeremotedesk",
                    "hpss-capture-v2",
                    "--credentials-stdin-v1",
                    "--out",
                    "capture.mvs",
                    "--seconds",
                    "5",
                    "--max-records",
                    "1",
                ])
                .unwrap();
                assert!(matches!(cli.cmd, Cmd::HpssCaptureV2 { .. }));
            }
        }
    }
}
