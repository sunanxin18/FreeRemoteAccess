//! ARP 局域网发现模块（Windows 实现，基于 iphlpapi 的 SendARP）。
//!
//! ARP（RFC 826）工作在链路层，用户态直接构造/嗅探原始以太网帧需要
//! Npcap 之类的驱动和管理员权限；而 Windows 内核自带的 SendARP API
//! 会代我们发出真实的 ARP 请求（等价于 "arp ping"）并等待应答，
//! 是免驱动、免管理员权限的标准做法。

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, ToSocketAddrs};
use std::str::FromStr;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
#[cfg(target_os = "windows")]
use windows_sys::Win32::NetworkManagement::IpHelper::SendARP;

use crate::vnc::protocol;

/// 单次主动 ARP 扫描允许的最大目标数，避免宽 CIDR 导致巨额分配和网络洪泛。
const MAX_ARP_SCAN_ADDRESSES: u64 = 1 << 16;

/// 扫描发现的主机
#[derive(Debug, Clone)]
pub struct Host {
    pub ip: Ipv4Addr,
    pub mac: [u8; 6],
    pub vendor: &'static str,
    pub vnc_banner: Option<String>,
}

impl Host {
    pub fn mac_str(&self) -> String {
        format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.mac[0], self.mac[1], self.mac[2], self.mac[3], self.mac[4], self.mac[5]
        )
    }
}

/// 常见 Apple OUI（MAC 地址前 3 字节）。非完整数据库，仅作提示；
/// 判断一台设备是否为可连接的 macOS，以 5900 端口的 RFB banner 为准。
#[cfg(target_os = "windows")]
const APPLE_OUIS: &[&str] = &[
    "00:03:93", "00:05:02", "00:0A:27", "00:10:FA", "00:14:51", "00:16:CB", "00:17:F2", "00:19:E3",
    "00:1B:63", "00:1C:B3", "00:1D:4F", "00:1E:52", "00:1F:5B", "00:1F:F3", "00:22:41", "00:23:12",
    "00:23:32", "00:23:6C", "00:23:DF", "00:24:36", "00:25:00", "00:25:4B", "00:25:BC", "00:26:08",
    "00:26:4A", "00:26:B0", "00:26:BB", "04:0C:CE", "04:26:65", "04:D4:C4", "08:00:07", "0C:30:21",
    "0C:74:C2", "10:DD:B1", "14:99:E2", "18:AF:61", "1C:AB:A7", "20:C9:D0", "24:A0:74", "28:E0:2C",
    "30:F9:ED", "34:15:9E", "3C:07:54", "3C:15:C2", "3C:22:FB", "38:C9:86", "38:F9:D3", "40:33:1A",
    "40:3C:FC", "44:FB:42", "48:74:6E", "4C:57:CA", "50:EA:D6", "54:26:96", "58:55:CA", "58:B0:35",
    "5C:59:48", "5C:F9:38", "60:FA:CD", "64:A3:CB", "6C:40:08", "68:A8:6D", "70:DE:E2", "74:E1:B6",
    "78:31:C1", "7C:6D:62", "80:E6:50", "84:38:35", "88:66:A5", "8C:7B:9D", "90:B2:1F", "94:94:26",
    "98:01:A7", "9C:20:7B", "A4:5E:60", "A4:83:E7", "A8:88:08", "AC:87:A3", "AC:BC:32", "B0:CA:68",
    "B4:18:D1", "B8:E8:56", "BC:52:B7", "C8:69:CD", "C8:BC:C8", "CC:08:E0", "D0:E1:40", "D4:9A:20",
    "D8:00:4D", "DC:2B:61", "E0:F8:47", "E4:CE:8F", "E8:80:2E", "F0:18:98", "F8:1E:DF",
];

#[cfg(target_os = "windows")]
fn vendor_of(mac: &[u8; 6]) -> &'static str {
    let key = format!("{:02X}:{:02X}:{:02X}", mac[0], mac[1], mac[2]);
    if APPLE_OUIS.contains(&key.as_str()) {
        "Apple"
    } else {
        "未知"
    }
}

/// 对目标 IP 发送真实 ARP 请求并等待应答（"ARP ping"）。
/// 在线主机返回 MAC；离线主机在内核内部超时后返回 None。
pub fn arp_lookup(ip: Ipv4Addr) -> Option<[u8; 6]> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = ip;
        return None;
    }

    #[cfg(target_os = "windows")]
    {
        // SendARP 要求以网络字节序传递 IPv4：u32 的内存布局即 4 个地址字节
        let dest = u32::from_le_bytes(ip.octets());
        let mut mac_words = [0u32; 2];
        let mut len: u32 = 8;
        let rc = unsafe { SendARP(dest, 0, mac_words.as_mut_ptr().cast(), &mut len) };
        if rc == 0 && len >= 6 {
            let b0 = mac_words[0].to_le_bytes();
            let b1 = mac_words[1].to_le_bytes();
            Some([b0[0], b0[1], b0[2], b0[3], b1[0], b1[1]])
        } else {
            None
        }
    }
}

/// IPv4 网段
#[derive(Debug, Clone, Copy)]
pub struct Ipv4Net {
    pub addr: Ipv4Addr,
    pub prefix: u8,
}

impl Ipv4Net {
    pub fn size(&self) -> u64 {
        if self.prefix == 0 {
            1u64 << 32
        } else {
            1u64 << (32 - self.prefix)
        }
    }

    /// 网段内可分配的主机地址（/31、/32 特殊处理）
    pub fn hosts(&self) -> Result<Vec<Ipv4Addr>> {
        let mask: u32 = if self.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - self.prefix as u32)
        };
        let net = u32::from(self.addr) & mask;
        let total = 1u64 << (32 - self.prefix);
        let (start, n) = if self.prefix >= 31 {
            (net, total.min(2))
        } else {
            (net + 1, total.saturating_sub(2))
        };
        if n > MAX_ARP_SCAN_ADDRESSES {
            bail!(
                "网段包含 {n} 个可扫描地址，超过单次 ARP 扫描上限 {MAX_ARP_SCAN_ADDRESSES}；请缩小 --cidr"
            );
        }
        let capacity = usize::try_from(n).context("ARP 目标数量超过本机表示范围")?;
        let mut hosts = Vec::new();
        hosts
            .try_reserve_exact(capacity)
            .context("无法为 ARP 目标列表分配内存")?;
        for offset in 0..n {
            let address =
                u32::try_from(u64::from(start) + offset).context("ARP 目标地址计算溢出")?;
            hosts.push(Ipv4Addr::from(address));
        }
        Ok(hosts)
    }
}

impl std::fmt::Display for Ipv4Net {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

impl FromStr for Ipv4Net {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (ip, prefix) = match s.split_once('/') {
            Some((ip, p)) => (
                ip,
                p.parse::<u8>()
                    .with_context(|| format!("非法前缀长度: {p}"))?,
            ),
            None => (s, 24),
        };
        if prefix > 32 {
            bail!("前缀长度最大为 32");
        }
        let addr = Ipv4Addr::from_str(ip).with_context(|| format!("非法 IPv4 地址: {ip}"))?;
        Ok(Self { addr, prefix })
    }
}

/// 自动探测本机出口 IPv4（UDP connect 只查路由表，不实际发包）
pub fn local_ipv4() -> Result<Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").context("无法创建 UDP socket")?;
    sock.connect("8.8.8.8:80")
        .context("无法确定本机路由（未联网？请用 --cidr 手动指定网段）")?;
    match sock.local_addr().context("无法获取本地地址")? {
        std::net::SocketAddr::V4(a) => Ok(*a.ip()),
        _ => Err(anyhow!("获取到 IPv6 地址，请用 --cidr 指定 IPv4 网段")),
    }
}

/// 多线程 ARP 扫描一个网段。SendARP 对离线主机会阻塞到内核超时，
/// 因此用线程池并发探测来摊平延迟。
pub fn sweep(net: Ipv4Net, threads: usize) -> Result<Vec<Host>> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (net, threads);
        bail!("当前平台不提供主动 ARP 扫描；请在连接页直接输入主机地址")
    }

    #[cfg(target_os = "windows")]
    {
        let ips = net.hosts()?;
        let threads = threads.clamp(1, ips.len().max(1)).min(256);
        eprintln!(
            "ARP 扫描 {}（{} 个地址，{} 线程）…",
            net,
            ips.len(),
            threads
        );

        let found: std::sync::Mutex<Vec<Host>> = std::sync::Mutex::new(Vec::new());
        let chunk = ips.len().div_ceil(threads);
        thread::scope(|s| {
            for group in ips.chunks(chunk.max(1)) {
                let found = &found;
                s.spawn(move || {
                    for ip in group {
                        let ip = *ip;
                        if let Some(mac) = arp_lookup(ip) {
                            found.lock().unwrap().push(Host {
                                ip,
                                mac,
                                vendor: vendor_of(&mac),
                                vnc_banner: None,
                            });
                        }
                    }
                });
            }
        });

        let mut hosts = found.into_inner().unwrap();
        hosts.sort_by_key(|h| u32::from(h.ip));
        Ok(hosts)
    }
}

/// 对已发现主机并发探测 5900 端口，读取 RFB banner（"RFB 003.008"）
pub fn probe_vnc_banner(hosts: &mut [Host], port: u16, timeout: Duration) {
    if hosts.is_empty() {
        return;
    }
    eprintln!("探测 VNC 端口 {port}…");
    let threads = hosts.len().min(32);
    let chunk = hosts.len().div_ceil(threads);
    thread::scope(|s| {
        for group in hosts.chunks_mut(chunk.max(1)) {
            s.spawn(move || {
                for host in group.iter_mut() {
                    host.vnc_banner = probe_rfb(host.ip, port, timeout);
                }
            });
        }
    });
}

fn probe_rfb(ip: Ipv4Addr, port: u16, timeout: Duration) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::V4(SocketAddrV4::new(ip, port)),
        timeout,
    )
    .ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(800)))
        .ok()?;
    let mut buf = [0u8; protocol::RFB_BANNER_BYTES];
    stream.read_exact(&mut buf).ok()?;
    protocol::parse_rfb_banner(&buf)
        .ok()
        .map(|banner| banner.display)
}

/// 解析 "host" / "host:port" / IPv4 / 域名 到 SocketAddr（带默认端口）
pub fn parse_target(host: &str, default_port: u16) -> Result<std::net::SocketAddr> {
    if let Ok(sa) = std::net::SocketAddr::from_str(host) {
        return Ok(sa);
    }
    if let Ok(ip) = std::net::IpAddr::from_str(host) {
        return Ok(std::net::SocketAddr::new(ip, default_port));
    }
    let with_port = format!("{host}:{default_port}");
    let mut iter = with_port
        .to_socket_addrs()
        .with_context(|| format!("无法解析主机名 {host}"))?;
    iter.next().context("域名解析没有返回结果")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    fn probe_literal_banner(banner: [u8; 12]) -> Option<String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&banner).unwrap();
        });

        let result = probe_rfb(Ipv4Addr::LOCALHOST, port, Duration::from_secs(1));
        server.join().unwrap();
        result
    }

    #[test]
    fn probe_rfb_accepts_valid_literal_banner() {
        assert_eq!(
            probe_literal_banner(*b"RFB 003.008\n"),
            Some("RFB 003.008".to_owned())
        );
    }

    #[test]
    fn probe_rfb_rejects_same_prefix_malformed_literal_banner() {
        assert_eq!(probe_literal_banner(*b"RFB 003-008\n"), None);
    }

    #[test]
    fn host_enumeration_rejects_networks_above_the_scan_budget() {
        let net = Ipv4Net::from_str("10.0.0.0/8").unwrap();
        assert!(net.hosts().is_err());
    }

    #[test]
    fn host_enumeration_preserves_small_network_semantics() {
        let net = Ipv4Net::from_str("192.0.2.0/30").unwrap();
        assert_eq!(
            net.hosts().unwrap(),
            vec![Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(192, 0, 2, 2)]
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn active_arp_scan_fails_explicitly_on_unsupported_hosts() {
        let error = sweep(Ipv4Net::from_str("192.0.2.0/30").unwrap(), 4).unwrap_err();
        assert!(error.to_string().contains("当前平台不提供主动 ARP 扫描"));
    }
}
