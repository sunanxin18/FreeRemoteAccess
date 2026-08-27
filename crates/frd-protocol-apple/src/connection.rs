//! Apple 连接、接收缓冲与单一加密 writer 的所有权边界。

use std::cell::Cell;
use std::error;
use std::fmt;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::session::{
    take_wire_ciphertext_frame, InboundSessionCrypto, OutboundSessionCrypto, SessionCrypto,
};

#[derive(Debug)]
struct PeerClosed;

impl fmt::Display for PeerClosed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("连接已被服务器关闭（EOF）")
    }
}

impl error::Error for PeerClosed {}

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

pub fn is_cold_deadline_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<ColdDeadlineError>())
}

pub fn is_peer_closed(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<PeerClosed>())
}

enum WriterCommand {
    Message {
        plaintext: Vec<u8>,
        result: SyncSender<Result<()>>,
    },
    Shutdown {
        complete: SyncSender<()>,
    },
}

struct WriterControl {
    commands: Sender<WriterCommand>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct AppleWriterHandle {
    control: Arc<WriterControl>,
}

impl AppleWriterHandle {
    pub fn send_private_message(&self, plaintext: &[u8]) -> Result<()> {
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        self.control
            .commands
            .send(WriterCommand::Message {
                plaintext: plaintext.to_vec(),
                result: result_tx,
            })
            .map_err(|_| anyhow!("Apple writer 已关闭"))?;
        result_rx
            .recv()
            .map_err(|_| anyhow!("Apple writer 未返回发送结果"))?
    }

    pub fn shutdown(&self) -> Result<()> {
        let (complete_tx, complete_rx) = mpsc::sync_channel(1);
        if self
            .control
            .commands
            .send(WriterCommand::Shutdown {
                complete: complete_tx,
            })
            .is_ok()
        {
            let _ = complete_rx.recv();
        }
        let worker = self
            .control
            .worker
            .lock()
            .map_err(|_| anyhow!("Apple writer join 状态已损坏"))?
            .take();
        if let Some(worker) = worker {
            worker
                .join()
                .map_err(|_| anyhow!("Apple writer 线程异常退出"))?;
        }
        Ok(())
    }
}

fn spawn_writer(
    mut stream: TcpStream,
    absolute_deadline: Option<Instant>,
    mut crypto: Option<OutboundSessionCrypto>,
) -> AppleWriterHandle {
    let (commands_tx, commands_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        writer_loop(&mut stream, absolute_deadline, &mut crypto, commands_rx)
    });
    AppleWriterHandle {
        control: Arc::new(WriterControl {
            commands: commands_tx,
            worker: Mutex::new(Some(worker)),
        }),
    }
}

fn writer_loop(
    stream: &mut TcpStream,
    absolute_deadline: Option<Instant>,
    crypto: &mut Option<OutboundSessionCrypto>,
    commands: Receiver<WriterCommand>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            WriterCommand::Message { plaintext, result } => {
                let send_result = (|| {
                    if let Some(deadline) = absolute_deadline {
                        let remaining = deadline
                            .checked_duration_since(Instant::now())
                            .filter(|duration| !duration.is_zero())
                            .ok_or_else(cold_deadline_error)?;
                        stream
                            .set_write_timeout(Some(remaining))
                            .context("设置 deadline 写超时失败")?;
                    }
                    let wire = match crypto {
                        Some(crypto) => crypto.seal(&plaintext)?,
                        None => plaintext,
                    };
                    stream.write_all(&wire).context("写入失败（连接中断？）")
                })();
                let failed = send_result.is_err();
                let _ = result.send(send_result);
                if failed {
                    let _ = stream.shutdown(Shutdown::Both);
                    break;
                }
            }
            WriterCommand::Shutdown { complete } => {
                let _ = stream.shutdown(Shutdown::Both);
                let _ = complete.send(());
                break;
            }
        }
    }
}

pub struct AppleConnection {
    stream: TcpStream,
    absolute_deadline: Option<Instant>,
    read_timeout_cap: Cell<Option<Duration>>,
    buffer: Vec<u8>,
    position: usize,
    end: usize,
    inbound_crypto: Option<InboundSessionCrypto>,
    outbound_crypto: Option<OutboundSessionCrypto>,
    wire_pending: Vec<u8>,
    writer: Option<AppleWriterHandle>,
}

impl AppleConnection {
    pub fn new(stream: TcpStream) -> Self {
        Self::new_with_optional_deadline(stream, None)
    }

    pub fn new_with_deadline(stream: TcpStream, deadline: Instant) -> Self {
        Self::new_with_optional_deadline(stream, Some(deadline))
    }

    fn new_with_optional_deadline(stream: TcpStream, absolute_deadline: Option<Instant>) -> Self {
        Self {
            stream,
            absolute_deadline,
            read_timeout_cap: Cell::new(None),
            buffer: vec![0; 16384],
            position: 0,
            end: 0,
            inbound_crypto: None,
            outbound_crypto: None,
            wire_pending: Vec::new(),
            writer: None,
        }
    }

    fn apply_deadline_timeouts(&self) -> Result<()> {
        let Some(deadline) = self.absolute_deadline else {
            return Ok(());
        };
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(cold_deadline_error)?;
        let read_timeout = self
            .read_timeout_cap
            .get()
            .map(|current| current.min(remaining))
            .unwrap_or(remaining);
        self.stream
            .set_read_timeout(Some(read_timeout))
            .context("设置 deadline 读超时失败")?;
        self.stream
            .set_write_timeout(Some(remaining))
            .context("设置 deadline 写超时失败")?;
        Ok(())
    }

    pub fn set_read_timeout(&self, duration: Option<Duration>) -> Result<()> {
        self.stream
            .set_read_timeout(duration)
            .context("设置读超时失败")?;
        self.read_timeout_cap.set(duration);
        Ok(())
    }

    pub fn read_timeout(&self) -> std::io::Result<Option<Duration>> {
        self.stream.read_timeout()
    }

    pub fn write_timeout(&self) -> std::io::Result<Option<Duration>> {
        self.stream.write_timeout()
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        self.stream.peer_addr().context("读取远端 socket 地址失败")
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.stream.local_addr().context("读取本地 socket 地址失败")
    }

    pub fn set_crypto(&mut self, crypto: SessionCrypto) -> Result<()> {
        if self.writer.is_some() || self.inbound_crypto.is_some() || self.outbound_crypto.is_some()
        {
            bail!("Apple 加密状态已经安装");
        }
        self.wire_pending = self.buffer[self.position..self.end].to_vec();
        self.position = 0;
        self.end = 0;
        let (inbound, outbound) = crypto.split();
        self.inbound_crypto = Some(inbound);
        self.outbound_crypto = Some(outbound);
        Ok(())
    }

    pub fn is_encrypted(&self) -> bool {
        self.inbound_crypto.is_some()
    }

    pub fn writer_handle(&mut self) -> Result<AppleWriterHandle> {
        if let Some(writer) = &self.writer {
            return Ok(writer.clone());
        }
        let stream = self
            .stream
            .try_clone()
            .context("无法复制 Apple writer socket")?;
        let writer = spawn_writer(stream, self.absolute_deadline, self.outbound_crypto.take());
        self.writer = Some(writer.clone());
        Ok(writer)
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        if self.inbound_crypto.is_some() || self.writer.is_some() {
            return self.writer_handle()?.send_private_message(bytes);
        }
        self.apply_deadline_timeouts()?;
        self.stream
            .write_all(bytes)
            .context("写入失败（连接中断？）")
    }

    pub fn read_app_frame(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(data) = self.read_app_frame_step()? {
                return Ok(data);
            }
        }
    }

    pub fn read_app_frame_step(&mut self) -> Result<Option<Vec<u8>>> {
        if self.inbound_crypto.is_none() {
            bail!("连接未挂载加密");
        }
        if self.position < self.end {
            bail!("读缓冲有未消费数据，帧模式与流模式混用");
        }
        if let Some(ciphertext) = take_wire_ciphertext_frame(&mut self.wire_pending)? {
            return self
                .inbound_crypto
                .as_mut()
                .expect("加密状态已检查")
                .open(&ciphertext)
                .map(Some);
        }
        let mut temporary = [0; 16384];
        self.apply_deadline_timeouts()?;
        let received = self
            .stream
            .read(&mut temporary)
            .context("读取失败（连接可能已被服务器关闭）")?;
        if received == 0 {
            return Err(PeerClosed.into());
        }
        self.wire_pending.extend_from_slice(&temporary[..received]);
        let Some(ciphertext) = take_wire_ciphertext_frame(&mut self.wire_pending)? else {
            return Ok(None);
        };
        self.inbound_crypto
            .as_mut()
            .expect("加密状态已检查")
            .open(&ciphertext)
            .map(Some)
    }

    fn inject(&mut self, data: &[u8]) {
        let remaining = self.buffer[self.position..self.end].to_vec();
        let mut joined = data.to_vec();
        joined.extend_from_slice(&remaining);
        if joined.len() > self.buffer.len() {
            self.buffer.resize(joined.len(), 0);
        }
        self.buffer[..joined.len()].copy_from_slice(&joined);
        self.position = 0;
        self.end = joined.len();
    }

    pub fn read_exact_bytes(&mut self, output: &mut [u8]) -> Result<()> {
        let requested = output.len();
        let mut filled = 0;
        while filled < requested {
            if self.position < self.end {
                let take = (self.end - self.position).min(requested - filled);
                output[filled..filled + take]
                    .copy_from_slice(&self.buffer[self.position..self.position + take]);
                self.position += take;
                filled += take;
            } else if self.inbound_crypto.is_some() {
                loop {
                    if let Some(ciphertext) = take_wire_ciphertext_frame(&mut self.wire_pending)? {
                        let data = self
                            .inbound_crypto
                            .as_mut()
                            .expect("加密状态已检查")
                            .open(&ciphertext)?;
                        self.inject(&data);
                        break;
                    }
                    let mut temporary = [0; 16384];
                    self.apply_deadline_timeouts()?;
                    let received = self
                        .stream
                        .read(&mut temporary)
                        .context("读取失败（连接可能已被服务器关闭）")?;
                    if received == 0 {
                        return Err(PeerClosed.into());
                    }
                    self.wire_pending.extend_from_slice(&temporary[..received]);
                }
            } else if requested - filled >= self.buffer.len() {
                self.apply_deadline_timeouts()?;
                let received = self
                    .stream
                    .read(&mut output[filled..])
                    .context("读取失败（连接可能已被服务器关闭）")?;
                if received == 0 {
                    return Err(PeerClosed.into());
                }
                filled += received;
            } else {
                self.position = 0;
                self.end = 0;
                self.apply_deadline_timeouts()?;
                let received = self
                    .stream
                    .read(&mut self.buffer)
                    .context("读取失败（连接可能已被服务器关闭）")?;
                if received == 0 {
                    return Err(PeerClosed.into());
                }
                self.end = received;
            }
        }
        Ok(())
    }

    pub fn read_vec(&mut self, length: usize) -> Result<Vec<u8>> {
        let mut bytes = vec![0; length];
        self.read_exact_bytes(&mut bytes)?;
        Ok(bytes)
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        let mut bytes = [0; 1];
        self.read_exact_bytes(&mut bytes)?;
        Ok(bytes[0])
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        let mut bytes = [0; 2];
        self.read_exact_bytes(&mut bytes)?;
        Ok(u16::from_be_bytes(bytes))
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        let mut bytes = [0; 4];
        self.read_exact_bytes(&mut bytes)?;
        Ok(u32::from_be_bytes(bytes))
    }

    pub fn shutdown(&self) -> Result<()> {
        if let Some(writer) = &self.writer {
            writer.shutdown()?;
        }
        self.stream
            .shutdown(Shutdown::Both)
            .context("关闭 Apple 连接失败")
    }
}

impl Drop for AppleConnection {
    fn drop(&mut self) {
        if let Some(writer) = &self.writer {
            let _ = writer.shutdown();
        }
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn encrypted_frame_prefix_read_app_frame_and_stream_refills_decrypt_same_literal_wire_frame() {
        const WIRE_FRAME: [u8; 34] = [
            0x00, 0x20, 0xd4, 0x0f, 0x3e, 0x05, 0x98, 0x0f, 0x86, 0xde, 0x8c, 0x7d, 0x28, 0x28,
            0xdf, 0xf9, 0xf6, 0x02, 0x67, 0x0e, 0xf5, 0xaa, 0xb5, 0x0a, 0x17, 0x45, 0x98, 0xca,
            0x07, 0xc7, 0xe8, 0x22, 0x9a, 0x43,
        ];

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&WIRE_FRAME[1..]).unwrap();
        });
        let stream = TcpStream::connect(address).unwrap();
        let mut app_connection = AppleConnection::new(stream);
        app_connection
            .set_crypto(SessionCrypto::from_key_iv([0x11; 16], [0x22; 16]))
            .unwrap();
        app_connection
            .wire_pending
            .extend_from_slice(&WIRE_FRAME[..1]);
        assert_eq!(app_connection.read_app_frame().unwrap(), b"same");
        server.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&WIRE_FRAME[7..]).unwrap();
        });
        let stream = TcpStream::connect(address).unwrap();
        let mut stream_connection = AppleConnection::new(stream);
        stream_connection
            .set_crypto(SessionCrypto::from_key_iv([0x11; 16], [0x22; 16]))
            .unwrap();
        stream_connection
            .wire_pending
            .extend_from_slice(&WIRE_FRAME[..7]);
        let mut plaintext = [0u8; 4];
        stream_connection.read_exact_bytes(&mut plaintext).unwrap();
        assert_eq!(&plaintext, b"same");
        server.join().unwrap();
    }

    #[test]
    fn encrypted_app_frame_step_yields_after_one_incomplete_socket_read() {
        let key = [0x31; 16];
        let iv = [0x42; 16];
        let mut sender_crypto = SessionCrypto::from_key_iv(key, iv);
        let wire = sender_crypto.seal(b"incremental").unwrap();
        let (first_byte_sent, first_byte_ready) = std::sync::mpsc::channel();
        let (remainder_ready, send_remainder) = std::sync::mpsc::channel();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&wire[..1]).unwrap();
            first_byte_sent.send(()).unwrap();
            send_remainder.recv().unwrap();
            stream.write_all(&wire[1..]).unwrap();
        });

        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut connection = AppleConnection::new(stream);
        connection
            .set_crypto(SessionCrypto::from_key_iv(key, iv))
            .unwrap();
        first_byte_ready.recv().unwrap();

        assert_eq!(connection.read_app_frame_step().unwrap(), None);
        remainder_ready.send(()).unwrap();
        let plaintext = loop {
            if let Some(plaintext) = connection.read_app_frame_step().unwrap() {
                break plaintext;
            }
        };
        assert_eq!(plaintext, b"incremental");
        server.join().unwrap();
    }

    #[test]
    fn sequential_deadline_reads_and_writes_refresh_both_socket_timeouts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for expected in [0x11_u8, 0x22] {
                let mut byte = [0_u8; 1];
                stream.read_exact(&mut byte).unwrap();
                assert_eq!(byte[0], expected);
                stream.write_all(&[expected.wrapping_add(1)]).unwrap();
            }
        });
        let stream = TcpStream::connect(address).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut connection = AppleConnection::new_with_deadline(stream, deadline);

        connection.write_all(&[0x11]).unwrap();
        assert_eq!(connection.read_u8().unwrap(), 0x12);
        connection
            .stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        connection
            .stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        connection.write_all(&[0x22]).unwrap();
        assert_eq!(connection.read_u8().unwrap(), 0x23);

        let read_timeout = connection.read_timeout().unwrap().unwrap();
        let write_timeout = connection.write_timeout().unwrap().unwrap();
        assert!(read_timeout > Duration::from_secs(10));
        assert!(write_timeout > Duration::from_secs(10));
        assert!(read_timeout <= Duration::from_secs(30));
        assert!(write_timeout <= Duration::from_secs(30));
        server.join().unwrap();
    }
}
