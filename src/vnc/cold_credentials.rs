use anyhow::Result;
use std::io::Read;
use std::ops::Range;
use std::sync::atomic::{compiler_fence, Ordering};

const MAGIC: &[u8; 8] = b"FRDSTD01";
const HEADER_LEN: usize = 20;
const MAX_FRAME_LEN: usize = 1554;
const READ_LIMIT: usize = MAX_FRAME_LEN + 1;

pub struct GuardedCredentialFrame {
    bytes: Vec<u8>,
    host: Range<usize>,
    username: Range<usize>,
    password: Range<usize>,
    port: u16,
    cleared: bool,
}

pub struct CredentialSlices<'a> {
    pub host: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub port: u16,
}

impl GuardedCredentialFrame {
    #[allow(dead_code, reason = "保留可注入 Read 的确定性 FRDSTD01 解析测试入口")]
    pub fn read_stdin_v1<R: Read>(reader: &mut R) -> Result<Self> {
        Self::read_with(|buffer| reader.read(buffer))
    }

    /// 从进程标准输入的原生句柄直接读取 FRDSTD01。
    ///
    /// 该入口不经过 `std::io::Stdin` 的全局 `BufReader`，因此凭据字节只会进入
    /// `GuardedCredentialFrame::bytes`。原生标准输入句柄由进程所有，本函数既不接管
    /// 也不关闭它。Windows 使用同步 `ReadFile`；Unix 使用 fd 0 上的 `read(2)`；
    /// 其它平台保守地返回固定类别错误。
    pub fn read_process_stdin_v1() -> Result<Self> {
        let mut input =
            process_stdin::ProcessStdin::open().map_err(|_| category_error("stdin frame input"))?;
        Self::read_with(|buffer| input.read(buffer))
    }

    fn read_with(mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>) -> Result<Self> {
        let mut frame = Self {
            bytes: vec![0; READ_LIMIT],
            host: 0..0,
            username: 0..0,
            password: 0..0,
            port: 0,
            cleared: false,
        };
        let mut used = 0usize;

        while used < READ_LIMIT {
            match read(&mut frame.bytes[used..]) {
                Ok(0) => break,
                Ok(count) => {
                    used = used
                        .checked_add(count)
                        .ok_or_else(|| category_error("stdin frame input"))?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(category_error("stdin frame input")),
            }
        }
        frame.bytes.truncate(used);
        frame.parse(used)
    }

    fn parse(mut self, used: usize) -> Result<Self> {
        if used == READ_LIMIT {
            return Err(category_error("stdin frame extra"));
        }
        if self.bytes.len() != used || used < HEADER_LEN {
            return Err(category_error("stdin frame header"));
        }
        if self.bytes[..8] != MAGIC[..] {
            return Err(category_error("stdin frame magic"));
        }

        let payload_length = u32::from_be_bytes(
            self.bytes[8..12]
                .try_into()
                .map_err(|_| category_error("stdin frame payload"))?,
        ) as usize;
        let host_length = u16::from_be_bytes(
            self.bytes[12..14]
                .try_into()
                .map_err(|_| category_error("stdin frame host"))?,
        ) as usize;
        let username_length = u16::from_be_bytes(
            self.bytes[14..16]
                .try_into()
                .map_err(|_| category_error("stdin frame username"))?,
        ) as usize;
        let password_length = u16::from_be_bytes(
            self.bytes[16..18]
                .try_into()
                .map_err(|_| category_error("stdin frame password"))?,
        ) as usize;
        let port = u16::from_be_bytes(
            self.bytes[18..20]
                .try_into()
                .map_err(|_| category_error("stdin frame port"))?,
        );

        if !(1..=255).contains(&host_length) {
            return Err(category_error("stdin frame host"));
        }
        if !(1..=255).contains(&username_length) {
            return Err(category_error("stdin frame username"));
        }
        if !(1..=1024).contains(&password_length) {
            return Err(category_error("stdin frame password"));
        }
        if port == 0 {
            return Err(category_error("stdin frame port"));
        }

        let expected_payload = 8usize
            .checked_add(host_length)
            .and_then(|value| value.checked_add(username_length))
            .and_then(|value| value.checked_add(password_length))
            .ok_or_else(|| category_error("stdin frame payload"))?;
        if !(11..=1542).contains(&payload_length) || payload_length != expected_payload {
            return Err(category_error("stdin frame payload"));
        }

        let host_end = HEADER_LEN
            .checked_add(host_length)
            .ok_or_else(|| category_error("stdin frame host"))?;
        let username_end = host_end
            .checked_add(username_length)
            .ok_or_else(|| category_error("stdin frame username"))?;
        let password_end = username_end
            .checked_add(password_length)
            .ok_or_else(|| category_error("stdin frame password"))?;
        if password_end != self.bytes.len() {
            return Err(category_error("stdin frame payload"));
        }

        let host = std::str::from_utf8(&self.bytes[HEADER_LEN..host_end])
            .map_err(|_| category_error("stdin frame host"))?;
        validate_host_identity(host, "stdin frame host")?;
        let username = std::str::from_utf8(&self.bytes[host_end..username_end])
            .map_err(|_| category_error("stdin frame username"))?;
        crate::vnc::local_username::validate_local_username(username)
            .map_err(|_| category_error("stdin frame username"))?;
        let password = std::str::from_utf8(&self.bytes[username_end..password_end])
            .map_err(|_| category_error("stdin frame password"))?;
        if password.as_bytes().contains(&0) {
            return Err(category_error("stdin frame password"));
        }

        self.host = HEADER_LEN..host_end;
        self.username = host_end..username_end;
        self.password = username_end..password_end;
        self.port = port;
        Ok(self)
    }

    pub fn with_slices<T>(&self, f: impl FnOnce(CredentialSlices<'_>) -> T) -> T {
        assert!(!self.cleared, "credential frame cleared");
        let host = std::str::from_utf8(&self.bytes[self.host.start..self.host.end])
            .expect("validated host");
        let username = std::str::from_utf8(&self.bytes[self.username.start..self.username.end])
            .expect("validated username");
        let password = std::str::from_utf8(&self.bytes[self.password.start..self.password.end])
            .expect("validated password");
        f(CredentialSlices {
            host,
            username,
            password,
            port: self.port,
        })
    }

    /// 仅在凭据认证阶段借出切片，并在把拥有型认证结果交还调用方前清零整帧。
    /// 即使认证闭包 unwind，恢复 unwind 前也会先执行同一清零路径。
    pub fn with_slices_then_clear<T>(&mut self, f: impl FnOnce(CredentialSlices<'_>) -> T) -> T {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.with_slices(f)));
        self.clear();
        match result {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    pub fn clear(&mut self) {
        if self.cleared {
            return;
        }
        #[cfg(test)]
        let mut volatile_writes = 0usize;
        for (_, byte) in self.bytes.iter_mut().enumerate() {
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
            #[cfg(test)]
            {
                volatile_writes += 1;
            }
        }
        compiler_fence(Ordering::SeqCst);
        self.cleared = true;
        #[cfg(test)]
        notify_clear_observer(&self.bytes, volatile_writes);
        #[cfg(not(test))]
        notify_clear_observer(&self.bytes);
    }

    #[cfg(test)]
    fn guarded_bounds(&self) -> (*const u8, *const u8) {
        let start = self.bytes.as_ptr();
        (start, start.wrapping_add(self.bytes.len()))
    }
}

#[cfg(windows)]
mod process_stdin {
    use std::ffi::c_void;
    use std::io;

    type Handle = *mut c_void;
    const STD_INPUT_HANDLE: u32 = -10_i32 as u32;
    const INVALID_HANDLE_VALUE: Handle = -1_isize as Handle;
    const ERROR_HANDLE_EOF: i32 = 38;
    const ERROR_BROKEN_PIPE: i32 = 109;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> Handle;
        fn ReadFile(
            file: Handle,
            buffer: *mut c_void,
            bytes_to_read: u32,
            bytes_read: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    pub(super) struct ProcessStdin {
        handle: Handle,
    }

    impl ProcessStdin {
        pub(super) fn open() -> io::Result<Self> {
            // SAFETY: GetStdHandle has no caller-provided pointer preconditions. The returned
            // process-owned handle is only borrowed and is never passed to CloseHandle here.
            let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            if handle.is_null() {
                return Err(io::Error::from_raw_os_error(6));
            }
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { handle })
        }

        pub(super) fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let requested = u32::try_from(buffer.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "stdin read size"))?;
            let mut bytes_read = 0_u32;
            // SAFETY: `buffer` is writable for exactly `requested` bytes, `bytes_read` is a
            // valid out-pointer, synchronous ReadFile requires a null OVERLAPPED pointer, and
            // `self.handle` remains a borrowed live process standard-input handle for this call.
            let ok = unsafe {
                ReadFile(
                    self.handle,
                    buffer.as_mut_ptr().cast(),
                    requested,
                    &mut bytes_read,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 {
                return Ok(bytes_read as usize);
            }
            let error = io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(ERROR_HANDLE_EOF | ERROR_BROKEN_PIPE)
            ) {
                Ok(0)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(unix)]
mod process_stdin {
    use std::ffi::c_void;
    use std::io;

    extern "C" {
        #[link_name = "read"]
        fn read_fd(fd: i32, buffer: *mut c_void, count: usize) -> isize;
    }

    pub(super) struct ProcessStdin;

    impl ProcessStdin {
        pub(super) fn open() -> io::Result<Self> {
            Ok(Self)
        }

        pub(super) fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            // SAFETY: fd 0 is borrowed rather than owned or closed; `buffer` is writable for
            // `buffer.len()` bytes for the duration of this synchronous POSIX read call.
            let result = unsafe { read_fd(0, buffer.as_mut_ptr().cast(), buffer.len()) };
            if result >= 0 {
                Ok(result as usize)
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod process_stdin {
    use std::io;

    pub(super) struct ProcessStdin;

    impl ProcessStdin {
        pub(super) fn open() -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unbuffered process stdin is unavailable on this platform",
            ))
        }

        pub(super) fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unbuffered process stdin is unavailable on this platform",
            ))
        }
    }
}

impl Drop for GuardedCredentialFrame {
    fn drop(&mut self) {
        self.clear();
    }
}

fn validate_host_identity(value: &str, category: &'static str) -> Result<()> {
    if value
        .as_bytes()
        .iter()
        .any(|byte| *byte == 0 || byte.is_ascii_control())
        || value.trim() != value
    {
        return Err(category_error(category));
    }
    Ok(())
}

fn category_error(category: &'static str) -> anyhow::Error {
    anyhow::Error::msg(category)
}

#[cfg(test)]
static CLEAR_OBSERVER: std::sync::Mutex<Option<(std::thread::ThreadId, fn(&[u8], usize))>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn set_clear_observer(observer: Option<fn(&[u8], usize)>) {
    *CLEAR_OBSERVER.lock().expect("clear observer") =
        observer.map(|callback| (std::thread::current().id(), callback));
}

#[cfg(test)]
fn notify_clear_observer(bytes: &[u8], volatile_writes: usize) {
    if let Some((owner, observer)) = *CLEAR_OBSERVER.lock().expect("clear observer") {
        if owner == std::thread::current().id() {
            observer(bytes, volatile_writes);
        }
    }
}

#[cfg(not(test))]
fn notify_clear_observer(_: &[u8]) {}

#[cfg(test)]
mod tests {
    use super::{set_clear_observer, CredentialSlices, GuardedCredentialFrame};
    use std::io::{self, Cursor, Read};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Mutex;

    const MAGIC: &[u8; 8] = b"FRDSTD01";
    const FROZEN_MIN_FRAME: &[u8; 23] =
        b"FRDSTD01\x00\x00\x00\x0b\x00\x01\x00\x01\x00\x01\x17\x0cHUP";
    static OBSERVED: Mutex<Vec<(usize, bool, usize)>> = Mutex::new(Vec::new());
    static OBSERVER_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn observe_zeroed(bytes: &[u8], volatile_writes: usize) {
        OBSERVED.lock().expect("observer state").push((
            bytes.len(),
            bytes.iter().all(|byte| *byte == 0),
            volatile_writes,
        ));
    }

    fn valid_frame() -> Vec<u8> {
        frame(b"canary-host", b"canary-user", b"canary-pass", 5900)
    }

    fn frame(host: &[u8], username: &[u8], password: &[u8], port: u16) -> Vec<u8> {
        let payload = 8usize + host.len() + username.len() + password.len();
        let mut bytes = Vec::with_capacity(20 + host.len() + username.len() + password.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(payload as u32).to_be_bytes());
        bytes.extend_from_slice(&(host.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&(username.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&(password.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&port.to_be_bytes());
        bytes.extend_from_slice(host);
        bytes.extend_from_slice(username);
        bytes.extend_from_slice(password);
        bytes
    }

    fn parse(bytes: &[u8]) -> anyhow::Result<GuardedCredentialFrame> {
        GuardedCredentialFrame::read_stdin_v1(&mut Cursor::new(bytes))
    }

    fn parse_reader<R: Read>(reader: &mut R) -> anyhow::Result<GuardedCredentialFrame> {
        GuardedCredentialFrame::read_stdin_v1(reader)
    }

    struct CanaryReadError;

    impl Read for CanaryReadError {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Other, "canary-reader-error"))
        }
    }

    fn error_category(bytes: &[u8], canary: &str) -> String {
        let error = match parse(bytes) {
            Ok(mut guarded) => {
                guarded.clear();
                panic!("frame must be rejected");
            }
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(!message.contains(canary), "error leaked fake input");
        assert!(!message.chars().any(|character| character.is_ascii_digit()));
        message
    }

    fn expect_rejected(bytes: &[u8]) {
        assert!(parse(bytes).is_err(), "malformed frame must be rejected");
    }

    #[test]
    fn parses_exact_fake_frame_as_borrowed_slices() {
        let mut guarded = parse(&valid_frame()).expect("valid fake frame");
        let (buffer_start, buffer_end) = guarded.guarded_bounds();
        guarded.with_slices(|slices| {
            assert_eq!(slices.host, "canary-host");
            assert_eq!(slices.username, "canary-user");
            assert_eq!(slices.password, "canary-pass");
            assert_eq!(slices.port, 5900);
            assert!(slices.host.as_ptr() >= buffer_start && slices.host.as_ptr() < buffer_end);
            assert!(
                slices.username.as_ptr() >= buffer_start && slices.username.as_ptr() < buffer_end
            );
            assert!(
                slices.password.as_ptr() >= buffer_start && slices.password.as_ptr() < buffer_end
            );
        });
        guarded.clear();
    }

    #[test]
    fn accepts_literal_minimum_frame_with_payload_11_and_total_23() {
        let mut guarded = parse(FROZEN_MIN_FRAME).expect("frozen minimum frame");
        guarded.with_slices(|slices| {
            assert_eq!(slices.host, "H");
            assert_eq!(slices.username, "U");
            assert_eq!(slices.password, "P");
            assert_eq!(slices.port, 5900);
        });
        guarded.clear();
    }

    #[test]
    fn accepts_all_exact_maxima_and_port_65535() {
        let host = vec![b'h'; 255];
        let username = vec![b'u'; 255];
        let password = vec![b'p'; 1024];
        let mut guarded = parse(&frame(&host, &username, &password, 65535)).expect("maxima");
        guarded.with_slices(|slices| {
            assert_eq!(slices.host.len(), 255);
            assert_eq!(slices.username.len(), 255);
            assert_eq!(slices.password.len(), 1024);
            assert_eq!(slices.port, 65535);
        });
        guarded.clear();
    }

    #[test]
    fn rejects_zero_and_maximum_plus_one_lengths() {
        expect_rejected(&frame(b"", b"user", b"password", 1));
        expect_rejected(&frame(b"host", b"", b"password", 1));
        expect_rejected(&frame(b"host", b"user", b"", 1));
        expect_rejected(&frame(&vec![b'h'; 256], b"user", b"password", 1));
        expect_rejected(&frame(b"host", &vec![b'u'; 256], b"password", 1));
        expect_rejected(&frame(b"host", b"user", &vec![b'p'; 1025], 1));
    }

    #[test]
    fn rejects_port_zero() {
        expect_rejected(&frame(b"host", b"user", b"password", 0));
    }

    #[test]
    fn rejects_every_fixed_header_truncation_boundary() {
        let full = valid_frame();
        for boundary in [0usize, 7, 8, 11, 12, 13, 14, 15, 16, 17, 18, 19] {
            expect_rejected(&full[..boundary]);
        }
    }

    #[test]
    fn rejects_truncated_variable_fields() {
        let full = valid_frame();
        expect_rejected(&full[..20]);
        expect_rejected(&full[..21]);
        expect_rejected(&full[..31]);
        expect_rejected(&full[..35]);
        expect_rejected(&FROZEN_MIN_FRAME[..21]);
        expect_rejected(&FROZEN_MIN_FRAME[..22]);
    }

    #[test]
    fn rejects_payload_length_mismatch_and_one_extra_byte() {
        let mut mismatch = valid_frame();
        mismatch[8..12].copy_from_slice(&11u32.to_be_bytes());
        expect_rejected(&mismatch);
        let mut extra = valid_frame();
        extra.push(0x5a);
        expect_rejected(&extra);
    }

    #[test]
    fn rejects_invalid_utf8_in_every_text_field() {
        expect_rejected(&frame(&[0xff], b"user", b"password", 1));
        expect_rejected(&frame(b"host", &[0xff], b"password", 1));
        expect_rejected(&frame(b"host", b"user", &[0xff], 1));
    }

    #[test]
    fn rejects_host_and_username_nul_controls_and_unicode_edge_whitespace() {
        for bad in [
            b"a\0b".as_slice(),
            b"a\x1fb".as_slice(),
            "\u{2003}host".as_bytes(),
            "user\u{2003}".as_bytes(),
        ] {
            expect_rejected(&frame(bad, b"user", b"password", 1));
            expect_rejected(&frame(b"host", bad, b"password", 1));
        }
    }

    #[test]
    fn rejects_non_ascii_control_inside_local_username() {
        expect_rejected(&frame(
            b"host",
            "local\u{85}user".as_bytes(),
            b"password",
            1,
        ));
    }

    #[test]
    fn rejects_password_nul_but_preserves_other_password_bytes() {
        expect_rejected(&frame(b"host", b"user", b"pass\0word", 1));
        let password = "  canary\t\n\u{2003} ";
        let mut guarded =
            parse(&frame(b"host", b"user", password.as_bytes(), 1)).expect("password bytes");
        guarded.with_slices(|slices| assert_eq!(slices.password, password));
        guarded.clear();
    }

    #[test]
    fn errors_are_stable_categories_without_fake_values_or_lengths() {
        let mut magic = valid_frame();
        magic[0] = b'X';
        assert_eq!(error_category(&magic, "canary-host"), "stdin frame magic");
        assert_eq!(
            error_category(
                &frame(b"\xff", b"canary-user", b"canary-pass", 1),
                "canary-user"
            ),
            "stdin frame host"
        );
    }

    #[test]
    fn every_error_category_redacts_its_fake_canary() {
        let mut magic = valid_frame();
        magic[0] = b'X';
        let mut payload = frame(b"canary-payload", b"user", b"password", 1);
        payload[8..12].copy_from_slice(&11u32.to_be_bytes());
        let mut extra_host = b"canary-extra".to_vec();
        extra_host.extend_from_slice(&[b'h'; 255 - b"canary-extra".len()]);
        let mut extra = frame(&extra_host, &vec![b'u'; 255], &vec![b'p'; 1024], 1);
        extra.push(0x5a);
        let mut reader = CanaryReadError;
        let cases = vec![
            (parse(&magic), "canary-host", "stdin frame magic"),
            (
                parse(b"canary-header"),
                "canary-header",
                "stdin frame header",
            ),
            (parse(&payload), "canary-payload", "stdin frame payload"),
            (
                parse(&frame(b"canary-host\x1f", b"user", b"password", 1)),
                "canary-host",
                "stdin frame host",
            ),
            (
                parse(&frame(b" canary-host", b"user", b"password", 1)),
                "canary-host",
                "stdin frame host",
            ),
            (
                parse(&frame(b"host", b"canary-user\x1f", b"password", 1)),
                "canary-user",
                "stdin frame username",
            ),
            (
                parse(&frame(b"host", b"canary-user ", b"password", 1)),
                "canary-user",
                "stdin frame username",
            ),
            (
                parse(&frame(b"host", b"user", b"canary-password\0", 1)),
                "canary-password",
                "stdin frame password",
            ),
            (
                parse(&frame(b"canary-port", b"user", b"password", 0)),
                "canary-port",
                "stdin frame port",
            ),
            (parse(&extra), "canary-extra", "stdin frame extra"),
            (
                parse_reader(&mut reader),
                "canary-reader-error",
                "stdin frame input",
            ),
        ];
        for (result, canary, category) in cases {
            let error = match result {
                Ok(mut guarded) => {
                    guarded.clear();
                    panic!("frame must be rejected");
                }
                Err(error) => error,
            };
            let message = error.to_string();
            assert_eq!(message, category);
            assert!(!message.contains(canary), "error leaked fake input");
            assert!(!message.chars().any(|character| character.is_ascii_digit()));
        }
    }

    #[test]
    fn checked_boundaries_reject_declared_ranges_past_available_bytes() {
        let mut bytes = valid_frame();
        bytes[12..14].copy_from_slice(&255u16.to_be_bytes());
        bytes[14..16].copy_from_slice(&255u16.to_be_bytes());
        bytes[16..18].copy_from_slice(&1024u16.to_be_bytes());
        bytes[8..12].copy_from_slice(&1542u32.to_be_bytes());
        expect_rejected(&bytes);
    }

    fn install_observer() {
        OBSERVED.lock().expect("observer state").clear();
        set_clear_observer(Some(observe_zeroed));
    }

    fn assert_observed_zero(expected_length: usize) {
        let correct = {
            let observations = OBSERVED.lock().expect("observer state");
            observations.len() == 1 && observations[0] == (expected_length, true, expected_length)
        };
        assert!(
            correct,
            "observer must receive one fully zeroed guarded vector"
        );
    }

    fn replace_all_guarded_bytes_with_fake_nonzero_canary(guarded: &mut GuardedCredentialFrame) {
        for byte in &mut guarded.bytes {
            *byte = 0xa5;
        }
    }

    #[test]
    fn explicit_clear_overwrites_after_parse_success() {
        let _serial = OBSERVER_TEST_LOCK.lock().expect("observer test lock");
        install_observer();
        let length = valid_frame().len();
        let mut guarded = parse(&valid_frame()).expect("valid fake frame");
        replace_all_guarded_bytes_with_fake_nonzero_canary(&mut guarded);
        guarded.clear();
        assert_observed_zero(length);
        set_clear_observer(None);
    }

    #[test]
    fn explicit_clear_overwrites_after_callback_error() {
        let _serial = OBSERVER_TEST_LOCK.lock().expect("observer test lock");
        install_observer();
        let length = valid_frame().len();
        let mut guarded = parse(&valid_frame()).expect("valid fake frame");
        let result: Result<(), &str> = guarded.with_slices(|_| Err("fake callback error"));
        assert_eq!(result, Err("fake callback error"));
        replace_all_guarded_bytes_with_fake_nonzero_canary(&mut guarded);
        guarded.clear();
        assert_observed_zero(length);
        set_clear_observer(None);
    }

    #[test]
    fn explicit_clear_overwrites_after_authentication_style_callback_success() {
        let _serial = OBSERVER_TEST_LOCK.lock().expect("observer test lock");
        install_observer();
        let length = valid_frame().len();
        let mut guarded = parse(&valid_frame()).expect("valid fake frame");
        let accepted = guarded.with_slices(
            |CredentialSlices {
                 host,
                 username,
                 password,
                 port,
             }| {
                !host.is_empty() && !username.is_empty() && !password.is_empty() && port == 5900
            },
        );
        assert!(accepted);
        replace_all_guarded_bytes_with_fake_nonzero_canary(&mut guarded);
        guarded.clear();
        assert_observed_zero(length);
        set_clear_observer(None);
    }

    #[test]
    fn authentication_callback_success_clears_before_returning_owned_state() {
        let _serial = OBSERVER_TEST_LOCK.lock().expect("observer test lock");
        install_observer();
        let length = valid_frame().len();
        let mut guarded = parse(&valid_frame()).expect("valid fake frame");

        let authenticated = guarded.with_slices_then_clear(|slices| {
            assert_eq!(slices.password, "canary-pass");
            "owned-authenticated-state"
        });

        assert_eq!(authenticated, "owned-authenticated-state");
        assert_observed_zero(length);
        set_clear_observer(None);
    }

    #[test]
    fn authentication_callback_error_clears_before_returning_error() {
        let _serial = OBSERVER_TEST_LOCK.lock().expect("observer test lock");
        install_observer();
        let length = valid_frame().len();
        let mut guarded = parse(&valid_frame()).expect("valid fake frame");

        let result: Result<(), &str> =
            guarded.with_slices_then_clear(|_| Err("stable-authentication-error"));

        assert_eq!(result, Err("stable-authentication-error"));
        assert_observed_zero(length);
        set_clear_observer(None);
    }

    #[test]
    fn authentication_callback_panic_clears_before_resuming_unwind() {
        let _serial = OBSERVER_TEST_LOCK.lock().expect("observer test lock");
        install_observer();
        let length = valid_frame().len();
        let mut guarded = parse(&valid_frame()).expect("valid fake frame");

        let panic_result = catch_unwind(AssertUnwindSafe(|| {
            guarded.with_slices_then_clear(|slices| {
                assert_eq!(slices.password, "canary-pass");
                panic!("authentication callback panic");
            });
        }));

        assert!(panic_result.is_err());
        assert_observed_zero(length);
        set_clear_observer(None);
    }

    #[test]
    fn explicit_clear_overwrites_after_caught_callback_panic() {
        let _serial = OBSERVER_TEST_LOCK.lock().expect("observer test lock");
        install_observer();
        let length = valid_frame().len();
        let mut guarded = parse(&valid_frame()).expect("valid fake frame");
        let panic_result = catch_unwind(AssertUnwindSafe(|| {
            guarded.with_slices(|_| panic!("fake callback panic"));
        }));
        assert!(panic_result.is_err());
        replace_all_guarded_bytes_with_fake_nonzero_canary(&mut guarded);
        guarded.clear();
        assert_observed_zero(length);
        set_clear_observer(None);
    }

    #[test]
    fn drop_overwrites_the_guarded_vector() {
        let _serial = OBSERVER_TEST_LOCK.lock().expect("observer test lock");
        install_observer();
        let length = valid_frame().len();
        {
            let mut guarded = parse(&valid_frame()).expect("valid fake frame");
            replace_all_guarded_bytes_with_fake_nonzero_canary(&mut guarded);
        }
        assert_observed_zero(length);
        set_clear_observer(None);
    }

    #[test]
    fn rejected_parse_drop_overwrites_every_initialized_byte() {
        let _serial = OBSERVER_TEST_LOCK.lock().expect("observer test lock");
        install_observer();
        let mut malformed = *FROZEN_MIN_FRAME;
        malformed[0] = b'X';
        expect_rejected(&malformed);
        assert_observed_zero(malformed.len());
        set_clear_observer(None);
    }
}
