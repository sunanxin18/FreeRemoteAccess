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

const PRODUCTION_WRITER_IO_TIMEOUT: Duration = Duration::from_secs(2);

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

pub fn is_timeout(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io_error| {
            matches!(
                io_error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            )
        })
}

enum WriterCommand {
    Message {
        plaintext: Vec<u8>,
        result: SyncSender<Result<()>>,
    },
    EncryptedKeyEvent {
        down: bool,
        keysym: u32,
        result: SyncSender<Result<()>>,
    },
    Shutdown,
}

struct WriterControl {
    commands: Sender<WriterCommand>,
    interrupt: TcpStream,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WriterIoTestEvent {
    Started { wire_bytes: usize },
    Finished { succeeded: bool },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionRuntimeTestEvent {
    LoopEntered,
    DisconnectDequeued,
    MediaClosed,
    ConnectionShutdown,
    WriterShutdown,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplicationFrameReadTestEvent {
    ReadPollSet(Duration),
    ApplicationFrameRead(Duration),
}

#[derive(Default)]
struct WriterHooks {
    #[cfg(test)]
    io_events: Option<Sender<WriterIoTestEvent>>,
}

impl WriterHooks {
    fn notify_write_started(&self, _wire_bytes: usize) {
        #[cfg(test)]
        if let Some(io_events) = &self.io_events {
            let _ = io_events.send(WriterIoTestEvent::Started {
                wire_bytes: _wire_bytes,
            });
        }
    }

    fn notify_write_finished(&self, _succeeded: bool) {
        #[cfg(test)]
        if let Some(io_events) = &self.io_events {
            let _ = io_events.send(WriterIoTestEvent::Finished {
                succeeded: _succeeded,
            });
        }
    }
}

#[derive(Clone)]
pub struct AppleWriterHandle {
    control: Arc<WriterControl>,
}

impl AppleWriterHandle {
    pub fn send_private_message(&self, plaintext: &[u8]) -> Result<()> {
        Self::receive_message_result(self.enqueue_private_message(plaintext.to_vec())?)
    }

    fn enqueue_private_message(&self, plaintext: Vec<u8>) -> Result<Receiver<Result<()>>> {
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        self.control
            .commands
            .send(WriterCommand::Message {
                plaintext,
                result: result_tx,
            })
            .map_err(|_| anyhow!("Apple writer 已关闭"))?;
        Ok(result_rx)
    }

    pub(crate) fn send_encrypted_key_event(&self, down: bool, keysym: u32) -> Result<()> {
        (|| {
            let (result_tx, result_rx) = mpsc::sync_channel(1);
            self.control
                .commands
                .send(WriterCommand::EncryptedKeyEvent {
                    down,
                    keysym,
                    result: result_tx,
                })
                .map_err(|_| anyhow!("Apple writer 已关闭"))?;
            Self::receive_message_result(result_rx)
        })()
        .context("发送 Apple High Performance 加密按键失败")
    }

    fn receive_message_result(result_rx: Receiver<Result<()>>) -> Result<()> {
        result_rx
            .recv()
            .map_err(|_| anyhow!("Apple writer 未返回发送结果"))?
    }

    pub fn shutdown(&self) -> Result<()> {
        let worker = self
            .control
            .worker
            .lock()
            .map_err(|_| anyhow!("Apple writer join 状态已损坏"))?
            .take();
        if let Some(worker) = worker {
            let _ = self.control.interrupt.shutdown(Shutdown::Both);
            let _ = self.control.commands.send(WriterCommand::Shutdown);
            worker
                .join()
                .map_err(|_| anyhow!("Apple writer 线程异常退出"))?;
        }
        Ok(())
    }
}

fn spawn_writer_with_hooks(
    mut stream: TcpStream,
    interrupt: TcpStream,
    absolute_deadline: Option<Instant>,
    mut crypto: Option<OutboundSessionCrypto>,
    hooks: WriterHooks,
) -> AppleWriterHandle {
    let (commands_tx, commands_rx) = mpsc::channel();
    let key_clock_origin = Instant::now();
    let worker = thread::spawn(move || {
        writer_loop(
            &mut stream,
            absolute_deadline,
            &mut crypto,
            commands_rx,
            hooks,
            key_clock_origin,
        )
    });
    AppleWriterHandle {
        control: Arc::new(WriterControl {
            commands: commands_tx,
            interrupt,
            worker: Mutex::new(Some(worker)),
        }),
    }
}

fn writer_loop(
    stream: &mut TcpStream,
    absolute_deadline: Option<Instant>,
    crypto: &mut Option<OutboundSessionCrypto>,
    commands: Receiver<WriterCommand>,
    hooks: WriterHooks,
    mut previous_key_event_at: Instant,
) {
    while let Ok(command) = commands.recv() {
        let (wire_result, result) = match command {
            WriterCommand::Message { plaintext, result } => {
                let wire_result = match crypto {
                    Some(crypto) => crypto.seal(&plaintext),
                    None => Ok(plaintext),
                };
                (wire_result, result)
            }
            WriterCommand::EncryptedKeyEvent {
                down,
                keysym,
                result,
            } => {
                let now = Instant::now();
                let delta_microseconds = now
                    .saturating_duration_since(previous_key_event_at)
                    .as_micros() as u32;
                previous_key_event_at = now;
                let wire_result = crypto
                    .as_mut()
                    .context("Apple High Performance 加密按键要求已建立的 Apple 加密会话")
                    .and_then(|crypto| {
                        crypto.seal_high_performance_key_event(down, keysym, delta_microseconds)
                    });
                (wire_result, result)
            }
            WriterCommand::Shutdown => {
                let _ = stream.shutdown(Shutdown::Both);
                break;
            }
        };
        let send_result = (|| {
            let write_timeout = if let Some(deadline) = absolute_deadline {
                deadline
                    .checked_duration_since(Instant::now())
                    .filter(|duration| !duration.is_zero())
                    .ok_or_else(cold_deadline_error)?
                    .min(PRODUCTION_WRITER_IO_TIMEOUT)
            } else {
                PRODUCTION_WRITER_IO_TIMEOUT
            };
            stream
                .set_write_timeout(Some(write_timeout))
                .context("设置 Apple writer 写超时失败")?;
            let wire = wire_result?;
            hooks.notify_write_started(wire.len());
            let write_result = stream.write_all(&wire).context("写入失败（连接中断？）");
            hooks.notify_write_finished(write_result.is_ok());
            write_result
        })();
        let failed = send_result.is_err();
        let _ = result.send(send_result);
        if failed {
            let _ = stream.shutdown(Shutdown::Both);
            break;
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
    #[cfg(test)]
    session_runtime_test_events: Option<Sender<SessionRuntimeTestEvent>>,
    #[cfg(test)]
    application_frame_read_test_events: Option<Sender<ApplicationFrameReadTestEvent>>,
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
            #[cfg(test)]
            session_runtime_test_events: None,
            #[cfg(test)]
            application_frame_read_test_events: None,
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
        #[cfg(test)]
        if let Some(duration) = duration {
            self.notify_application_frame_read_test_event(
                ApplicationFrameReadTestEvent::ReadPollSet(duration),
            );
        }
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
        self.writer_handle_with_hooks(WriterHooks::default())
    }

    #[cfg(test)]
    pub(crate) fn writer_handle_with_io_events(
        &mut self,
        io_events: Sender<WriterIoTestEvent>,
    ) -> Result<AppleWriterHandle> {
        self.writer_handle_with_hooks(WriterHooks {
            io_events: Some(io_events),
        })
    }

    #[cfg(test)]
    pub(crate) fn set_session_runtime_test_events(
        &mut self,
        events: Sender<SessionRuntimeTestEvent>,
    ) {
        self.session_runtime_test_events = Some(events);
    }

    #[cfg(test)]
    pub(crate) fn set_application_frame_read_test_events(
        &mut self,
        events: Sender<ApplicationFrameReadTestEvent>,
    ) {
        self.application_frame_read_test_events = Some(events);
    }

    #[cfg(test)]
    pub(crate) fn notify_session_runtime_test_event(&self, event: SessionRuntimeTestEvent) {
        if let Some(events) = &self.session_runtime_test_events {
            let _ = events.send(event);
        }
    }

    #[cfg(test)]
    fn notify_application_frame_read_test_event(&self, event: ApplicationFrameReadTestEvent) {
        if let Some(events) = &self.application_frame_read_test_events {
            let _ = events.send(event);
        }
    }

    fn writer_handle_with_hooks(&mut self, hooks: WriterHooks) -> Result<AppleWriterHandle> {
        if let Some(writer) = &self.writer {
            return Ok(writer.clone());
        }
        let stream = self
            .stream
            .try_clone()
            .context("无法复制 Apple writer socket")?;
        let interrupt = self
            .stream
            .try_clone()
            .context("无法复制 Apple writer 中断 socket")?;
        let writer = spawn_writer_with_hooks(
            stream,
            interrupt,
            self.absolute_deadline,
            self.outbound_crypto.take(),
            hooks,
        );
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
        #[cfg(test)]
        if let Some(duration) = self.read_timeout_cap.get() {
            self.notify_application_frame_read_test_event(
                ApplicationFrameReadTestEvent::ApplicationFrameRead(duration),
            );
        }
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

    fn decrypt_high_performance_key_event(initial_key: &[u8; 16], message: &[u8]) -> [u8; 18] {
        use aes::cipher::{BlockDecryptMut, KeyInit};

        assert_eq!(&message[..2], &[0x10, 0x00]);
        let mut plaintext: [u8; 18] = message.try_into().unwrap();
        let mut decryptor = <ecb::Decryptor<aes::Aes128>>::new_from_slice(initial_key).unwrap();
        decryptor.decrypt_block_mut((&mut plaintext[2..]).into());
        plaintext
    }

    #[test]
    fn encrypted_key_commands_are_queued_then_wrapped_by_outer_session_crypto() {
        let initial_key = [0x21; 16];
        let outer_key = [0x31; 16];
        let outer_iv = [0x42; 16];
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let mut outer_crypto = SessionCrypto::from_key_iv(outer_key, outer_iv);
            let mut messages = Vec::new();
            for _ in 0..2 {
                let mut prefix = [0u8; 2];
                peer.read_exact(&mut prefix).unwrap();
                let mut ciphertext = vec![0u8; usize::from(u16::from_be_bytes(prefix))];
                peer.read_exact(&mut ciphertext).unwrap();
                messages.push(outer_crypto.open(&ciphertext).unwrap());
            }
            messages
        });

        let stream = TcpStream::connect(address).unwrap();
        let mut connection = AppleConnection::new(stream);
        connection
            .set_crypto(SessionCrypto::from_key_iv_with_initial_key(
                outer_key,
                outer_iv,
                initial_key,
            ))
            .unwrap();
        let writer = connection.writer_handle().unwrap();
        writer.send_encrypted_key_event(true, 0x61).unwrap();
        writer.send_encrypted_key_event(false, 0x61).unwrap();
        writer.shutdown().unwrap();

        let messages = server.join().unwrap();
        let pressed = decrypt_high_performance_key_event(&initial_key, &messages[0]);
        let released = decrypt_high_performance_key_event(&initial_key, &messages[1]);
        assert_eq!(pressed[2], 0xff);
        assert_eq!(pressed[3], 1);
        assert_eq!(&pressed[4..8], &0x61u32.to_be_bytes());
        assert_eq!(&pressed[12..], &[0; 6]);
        assert_eq!(released[2], 0xff);
        assert_eq!(released[3], 0);
        assert_eq!(&released[4..8], &0x61u32.to_be_bytes());
        assert_eq!(&released[12..], &[0; 6]);
    }

    #[test]
    fn encrypted_key_send_failure_has_safe_operation_context() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });
        let stream = TcpStream::connect(address).unwrap();
        let mut connection = AppleConnection::new(stream);
        connection
            .set_crypto(SessionCrypto::from_key_iv([0x31; 16], [0x42; 16]))
            .unwrap();
        let writer = connection.writer_handle().unwrap();

        let error = writer.send_encrypted_key_event(true, 0x61).unwrap_err();

        assert_eq!(
            error.to_string(),
            "发送 Apple High Performance 加密按键失败"
        );
        let chain = format!("{error:#}");
        assert!(chain.contains("Apple High Performance 按键初始密钥不可用"));
        assert!(!chain.contains("0x61"));
        writer.shutdown().unwrap();
        peer.join().unwrap();
    }

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

    #[test]
    fn shutdown_interrupts_in_flight_write_and_disconnects_queued_callers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (peer_ready_tx, peer_ready_rx) = mpsc::channel();
        let (release_peer_tx, release_peer_rx) = mpsc::channel();
        let peer = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            peer_ready_tx.send(()).unwrap();
            release_peer_rx.recv().unwrap();
        });

        let stream = TcpStream::connect(address).unwrap();
        peer_ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut connection = AppleConnection::new(stream);
        let (writer_io_tx, writer_io_rx) = mpsc::channel();
        let writer = connection
            .writer_handle_with_io_events(writer_io_tx)
            .unwrap();

        let active_writer = writer.clone();
        let active_send = thread::spawn(move || {
            active_writer.send_private_message(&vec![0x5a; 64 * 1024 * 1024])
        });
        let event = writer_io_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer must enter write_all before shutdown");
        assert!(matches!(event, WriterIoTestEvent::Started { .. }));

        let queued_one = writer.enqueue_private_message(vec![0x11; 8]).unwrap();
        let queued_two = writer.enqueue_private_message(vec![0x22; 8]).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let shutdown_threads: Vec<_> = [writer.clone(), writer.clone()]
            .into_iter()
            .map(|writer| {
                let barrier = barrier.clone();
                let shutdown_tx = shutdown_tx.clone();
                thread::spawn(move || {
                    barrier.wait();
                    shutdown_tx.send(writer.shutdown()).unwrap();
                })
            })
            .collect();
        barrier.wait();
        drop(shutdown_tx);

        for _ in 0..2 {
            assert!(shutdown_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("shutdown must be bounded")
                .is_ok());
        }
        let active_error = active_send.join().unwrap().unwrap_err();
        assert_eq!(active_error.to_string(), "写入失败（连接中断？）");
        for queued in [queued_one, queued_two] {
            let queued_error = AppleWriterHandle::receive_message_result(queued).unwrap_err();
            assert_eq!(queued_error.to_string(), "Apple writer 未返回发送结果");
        }
        for thread in shutdown_threads {
            thread.join().unwrap();
        }
        assert!(writer.shutdown().is_ok());
        assert!(connection.shutdown().is_ok());

        release_peer_tx.send(()).unwrap();
        peer.join().unwrap();
    }
}
