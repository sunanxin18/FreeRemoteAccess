//! ARD 会话捕获代理：本地监听一个端口，把流量原样转发到真实 VNC/ARD 服务器，
//! 同时把两个方向的字节流逐块记录到文件。用于逆向 Apple 私有协议扩展
//!（Apple 账号安全扩展、私有编码、MVS 等）：把 RDM / Screen Sharing 客户端指向
//! 本代理即可获得完整、未经篡改的会话记录。
//!
//! 记录文件格式（conn_NNN.rec，小端）：
//! ```text
//! 每条记录:
//!   [1B  方向: 'C'=客户端→服务器, 'S'=服务器→客户端]
//!   [8B  Unix 毫秒时间戳]
//!   [4B  负载长度]
//!   [NB  负载原始字节]
//! ```
//! 纯转发、不解析、不修改任何字节，保证协议行为与直连完全一致。

use std::fs::File;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// 方向标记字节
const DIR_C2S: u8 = b'C';
const DIR_S2C: u8 = b'S';

/// 启动代理主循环（Ctrl+C 退出；每条记录即时落盘，中断不丢已收数据）
pub fn run(listen: SocketAddr, target: SocketAddr, out_dir: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("创建日志目录 {} 失败", out_dir.display()))?;
    let listener =
        TcpListener::bind(listen).with_context(|| format!("监听 {listen} 失败（端口被占用？）"))?;

    println!("代理已启动: {listen} -> {target}");
    println!("记录目录  : {}", out_dir.display());
    println!("现在请把 ARD 客户端（RDM / Screen Sharing）指向 {listen}，");
    println!("完成一次连接后 Ctrl+C 结束本程序。Ctrl+C 前已写入的数据不会丢失。\n");

    let conn_counter = AtomicUsize::new(0);
    for incoming in listener.incoming() {
        let client = match incoming {
            Ok(s) => s,
            Err(e) => {
                eprintln!("接受连接失败: {e}");
                continue;
            }
        };
        let no = conn_counter.fetch_add(1, Ordering::SeqCst) + 1;
        // 每个连接独立线程处理，允许客户端并发/重连
        let out_dir = out_dir.clone();
        thread::spawn(move || {
            if let Err(e) = handle_conn(no, client, target, &out_dir) {
                eprintln!("[conn {no}] 错误: {e:#}");
            }
        });
    }
    Ok(())
}

/// 处理一条客户端连接：连上游、开日志、起两个泵线程
fn handle_conn(no: usize, client: TcpStream, target: SocketAddr, out_dir: &Path) -> Result<()> {
    let peer = client
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let server = TcpStream::connect_timeout(&target, Duration::from_secs(5))
        .with_context(|| format!("[conn {no}] 连接上游 {target} 失败"))?;
    server.set_nodelay(true).ok();
    client.set_nodelay(true).ok();

    let path = out_dir.join(format!("conn_{no:03}.rec"));
    let log = Arc::new(Mutex::new(
        File::create(&path).with_context(|| format!("创建 {} 失败", path.display()))?,
    ));
    println!("[conn {no}] {peer} -> {target}，记录到 {}", path.display());

    let server_w = server.try_clone().context("复制服务器 socket 失败")?;
    let client_r = client.try_clone().context("复制客户端 socket 失败")?;
    let log_c2s = log.clone();

    thread::scope(|s| {
        // 客户端 → 服务器
        s.spawn(|| pump(no, DIR_C2S, client, server_w, log_c2s));
        // 服务器 → 客户端
        s.spawn(|| pump(no, DIR_S2C, server, client_r, log));
    });

    println!("[conn {no}] 结束");
    Ok(())
}

/// 单方向泵：读到多少记多少、原样转发；任一方 EOF/出错即收尾
fn pump(no: usize, dir: u8, mut src: TcpStream, mut dst: TcpStream, log: Arc<Mutex<File>>) {
    let mut buf = vec![0u8; 65536];
    loop {
        match src.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if write_record(&mut *log.lock().unwrap(), dir, ts, &buf[..n]).is_err() {
                    eprintln!("[conn {no}] 写日志失败，停止记录该方向");
                    break;
                }
                if dst.write_all(&buf[..n]).is_err() {
                    break; // 对端已关闭
                }
            }
            Err(_) => break, // 连接重置/中断
        }
    }
    // 把 EOF 传递给对端，保证半关闭语义与直连一致
    let _ = dst.shutdown(Shutdown::Write);
}

/// 写一条记录（方向 + 时间戳 + 长度 + 负载），并立即 flush
fn write_record(out: &mut impl Write, dir: u8, ts: u64, payload: &[u8]) -> std::io::Result<()> {
    out.write_all(&[dir])?;
    out.write_all(&ts.to_le_bytes())?;
    out.write_all(&(payload.len() as u32).to_le_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 记录编码往返：header 字段应能按定长正确切分还原
    #[test]
    fn record_round_trip() {
        let mut buf: Vec<u8> = Vec::new();
        write_record(&mut buf, DIR_C2S, 0x1122334455667788, b"hello").unwrap();
        assert_eq!(buf[0], b'C');
        assert_eq!(
            u64::from_le_bytes(buf[1..9].try_into().unwrap()),
            0x1122334455667788
        );
        assert_eq!(u32::from_le_bytes(buf[9..13].try_into().unwrap()), 5);
        assert_eq!(&buf[13..], b"hello");

        let mut buf2: Vec<u8> = Vec::new();
        write_record(&mut buf2, DIR_S2C, 1, &[]).unwrap();
        assert_eq!(buf2[0], b'S');
        assert_eq!(u32::from_le_bytes(buf2[9..13].try_into().unwrap()), 0);
        assert_eq!(buf2.len(), 13);
    }
}
