# ARD Runtime Resource and Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove confirmed allocation, conversion, deadline, UTF-8, CopyRect, mutex-poison, and viewer-thread failure modes without weakening P1/P2 generation semantics.

**Architecture:** Pure checked helpers own FFI sizes, viewer scale/geometry, deadlines, and fixed-width UTF-8 fields. Framebuffer operations stay allocation-bounded. Shared-lock access becomes fallible on user/network paths, and both viewers use explicit reader shutdown/join so teardown has one deterministic owner.

**Tech Stack:** Rust 2021, `anyhow`, `clap`, `minifb`, `cpal`, Windows sockets, Rust unit and loopback tests.

**Spec:** `docs/superpowers/specs/2026-08-22-ard-p3-p4-p6-hardening.md`

## Global Constraints

- Scale is finite and strictly positive; scaled pixels must fit `protocol::limits::MAX_FRAMEBUFFER_PIXELS`.
- User-controlled durations use `Instant::checked_add`; overflow returns a Chinese error and never panics.
- Display-name truncation occurs only at a UTF-8 character boundary; wire record length stays exact.
- CopyRect uses no `w*h` temporary buffer and remains correct for horizontal/vertical overlap and clipping.
- No user/network/viewer production path may use `Mutex::lock().unwrap()`.
- Reader shutdown sets closing, shuts down the socket, and joins exactly once.
- This workspace has no Git metadata. Do not initialize Git; record verification checkpoints instead.

---

### Task 1: Validate signed AAC encoder buffer lengths before allocation

**Files:**
- Modify: `src/vnc/audio_codec.rs:180-245`
- Test: `src/vnc/audio_codec.rs` test module

**Interfaces:**
- Produces: `checked_positive_ffi_buffer_len(value: i32, field: &'static str) -> Result<usize>`.

- [ ] **Step 1: Add RED tests for negative, zero, and positive lengths**

```rust
#[test]
fn aac_encoder_output_length_rejects_non_positive_ffi_values() {
    assert!(checked_positive_ffi_buffer_len(-1, "maxOutBufBytes").is_err());
    assert!(checked_positive_ffi_buffer_len(0, "maxOutBufBytes").is_err());
    assert_eq!(checked_positive_ffi_buffer_len(4096, "maxOutBufBytes").unwrap(), 4096);
}
```

- [ ] **Step 2: Run RED**

```powershell
cargo test aac_encoder_output_length_rejects_non_positive_ffi_values
```

Expected: helper does not exist.

- [ ] **Step 3: Implement checked conversion and use it**

```rust
fn checked_positive_ffi_buffer_len(value: i32, field: &'static str) -> Result<usize> {
    ensure!(value > 0, "AAC-ELD 编码器报告非法 {field}: {value}");
    usize::try_from(value).with_context(|| format!("AAC-ELD {field} 超过 usize"))
}
```

Replace `info.maxOutBufBytes != 0` and `as usize` with this helper before constructing `AacEldEncoder`. Apply the same checked pattern to signed FDK fields used as sizes; equality-only fields may compare in their native type without casting.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test aac_encoder_output_length_
cargo test encoder_
```

Expected: all pass and no `maxOutBufBytes as usize` remains.

- [ ] **Step 5: Record checkpoint**

Record all FDK signed-to-size conversions inspected and their disposition.

---

### Task 2: Introduce one checked viewer scale and viewport geometry contract

**Files:**
- Create: `src/viewer_geometry.rs`
- Modify: `src/main.rs:1-230,280-370,830-875`
- Modify: `src/viewer.rs:35-220`
- Modify: `src/vnc/hpss_viewer.rs:1240-1760`
- Test: `src/viewer_geometry.rs`

**Interfaces:**
- Produces: `ValidatedScale`, `ViewportSize`, `scaled_viewport_size`, and `map_pointer_to_remote`.

- [ ] **Step 1: Add RED tests in the new module declaration**

Add `mod viewer_geometry;` in `main.rs` without a viewer feature gate so no-default tests compile the pure contract. Write:

```rust
#[test]
fn scale_rejects_nonfinite_nonpositive_and_pixel_budget_overflow() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0] {
        assert!(ValidatedScale::try_from(value).is_err());
    }
    let scale = ValidatedScale::try_from(f32::MAX).unwrap();
    assert!(scaled_viewport_size(u16::MAX, u16::MAX, scale).is_err());
}

#[test]
fn classic_and_hpss_viewers_share_exact_scaled_geometry() {
    let scale = ValidatedScale::try_from(0.75).unwrap();
    assert_eq!(
        scaled_viewport_size(1440, 900, scale).unwrap(),
        ViewportSize { width: 1080, height: 675 }
    );
}
```

- [ ] **Step 2: Run RED**

```powershell
cargo test scale_rejects_nonfinite_nonpositive_and_pixel_budget_overflow
cargo test classic_and_hpss_viewers_share_exact_scaled_geometry
```

Expected: module/types do not exist.

- [ ] **Step 3: Implement validated types**

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValidatedScale(f32);

impl TryFrom<f32> for ValidatedScale {
    type Error = anyhow::Error;
    fn try_from(value: f32) -> Result<Self> {
        ensure!(value.is_finite() && value > 0.0, "显示缩放必须是有限正数");
        Ok(Self(value))
    }
}

impl FromStr for ValidatedScale {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::try_from(value.parse::<f32>().context("显示缩放不是有效数字")?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewportSize { pub width: usize, pub height: usize }
```

Implement `Display` and `ONE`. `scaled_viewport_size` multiplies in `f64`, requires finite results within `usize`, applies `ceil`, then calls `framebuffer::validate_framebuffer_geometry`. `map_pointer_to_remote` validates viewport dimensions and clamps in integer/f64 space before returning `(u16, u16)`.

- [ ] **Step 4: Change CLI and both viewers to the new type**

Use `ValidatedScale` directly in `Cmd::View` and `Cmd::Hpssview`, with `default_value_t = ValidatedScale::ONE`. Delete classic viewer's `scale <= 0.0` fallback, all direct float-to-`usize` casts, and HPSS `saturating_mul` allocation. Both windows obtain `(width,height)` only from `scaled_viewport_size`; both pointer paths use `map_pointer_to_remote`.

- [ ] **Step 5: Run GREEN and help output**

```powershell
cargo test viewer_geometry
cargo test --no-default-features
cargo run -- --help
cargo run -- hpssview --help
```

Expected: tests pass; `--scale inf`, `nan`, zero, and negatives are rejected by clap before connection; help remains Chinese and documents a finite positive scale.

- [ ] **Step 6: Record checkpoint**

Record invalid CLI examples using only loopback/local targets; do not include private target data.

---

### Task 3: Make all user-controlled deadlines checked and policy values named

**Files:**
- Create: `src/runtime_policy.rs`
- Modify: `src/main.rs:1-30,280-620`
- Modify: `src/vnc/hpss.rs:450-680`
- Modify: `src/arp.rs:1-250`
- Test: `src/runtime_policy.rs` and `src/main.rs` test module

**Interfaces:**
- Produces: `checked_deadline`, CLI timeout constants, RFB default port, and scan policy constants.

- [ ] **Step 1: Add RED deadline overflow tests**

```rust
#[test]
fn user_duration_overflow_returns_error_instead_of_panicking() {
    let now = Instant::now();
    assert!(checked_deadline(now, Duration::MAX, "测试期限").is_err());
    assert!(checked_deadline(now, Duration::from_millis(1), "测试期限").is_ok());
}
```

Add command-level tests that parse `u64::MAX` for `--seconds` and `--wait-ms`, call the shared duration/deadline helper, and assert an error string containing `期限溢出`.

- [ ] **Step 2: Run RED**

```powershell
cargo test user_duration_overflow_returns_error_instead_of_panicking
cargo test cli_duration_overflow_is_reported
```

Expected: helper does not exist and current `Instant + Duration` can panic.

- [ ] **Step 3: Implement the helper and named policies**

```rust
pub fn checked_deadline(now: Instant, duration: Duration, label: &str) -> Result<Instant> {
    now.checked_add(duration)
        .with_context(|| format!("{label}溢出 Instant 可表示范围"))
}
```

Define semantic constants for the default RFB port, connect/handshake/read timeouts, default scan threads, scan probe timeout, maximum scan threads, banner read timeout, default HPSS duration, and short UDP poll interval in their owning modules. Replace production `Instant::now() + Duration::...` and duplicated clap literals where a policy name applies.

- [ ] **Step 4: Keep network discovery local and explicit**

Name the routing-probe address and explain that UDP `connect` performs route selection without payload. Do not move a private target into code. Reuse `client::RFB_BANNER_LEN` (make it `pub(crate)`) in ARP probing instead of a second `12`.

- [ ] **Step 5: Run GREEN**

```powershell
cargo test user_duration_
cargo test cli_duration_
cargo test arp::tests
cargo test --no-default-features
```

Expected: all pass; source search finds no user-derived `Instant::now() + Duration`.

- [ ] **Step 6: Record checkpoint**

Record every user-controlled duration site and its checked deadline owner.

---

### Task 4: Truncate HPSS display names at UTF-8 boundaries

**Files:**
- Modify: `src/vnc/hpss.rs:245-285`
- Test: `src/vnc/hpss.rs` test module

**Interfaces:**
- Consumes: the typed SetDisplayConfiguration builder from the wire plan.
- Produces: `encode_display_name_field(&str) -> [u8; DISPLAY_NAME_WIRE_CAPACITY]`.

- [ ] **Step 1: Add a RED multibyte boundary test**

```rust
#[test]
fn display_name_truncation_never_splits_utf8() {
    let input = format!("{}界", "a".repeat(DISPLAY_NAME_WIRE_CAPACITY - 1));
    let wire = build_set_display_config(&input);
    let field = &wire[DISPLAY_NAME_FIELD_RANGE];
    let used = field.iter().position(|byte| *byte == 0).unwrap_or(field.len());
    assert_eq!(std::str::from_utf8(&field[..used]).unwrap(), "a".repeat(39));
    assert_eq!(wire.len(), SET_DISPLAY_CONFIGURATION_WIRE_BYTES);
}
```

- [ ] **Step 2: Run RED**

```powershell
cargo test display_name_truncation_never_splits_utf8
```

Expected: current raw-byte truncation produces invalid UTF-8 at the field boundary.

- [ ] **Step 3: Implement char-boundary truncation**

Iterate `char_indices`, retain the largest end offset not exceeding `DISPLAY_NAME_WIRE_CAPACITY`, copy that prefix, and leave remaining bytes zero. Do not silently replace invalid data because Rust `&str` is already valid UTF-8.

- [ ] **Step 4: Run GREEN and exact wire tests**

```powershell
cargo test display_name_
cargo test build_set_display_config
```

Expected: multibyte test and existing 308-byte fixture pass.

- [ ] **Step 5: Record checkpoint**

Record the exact capacity and resulting retained character count for the test.

---

### Task 5: Remove CopyRect's rectangle-sized temporary allocation

**Files:**
- Modify: `src/framebuffer.rs:40-90,127-end`
- Test: `src/framebuffer.rs`

**Interfaces:**
- Produces: allocation-free `Framebuffer::copy_rect` with the current clipping contract.

- [ ] **Step 1: Add snapshot-oracle RED tests for overlap and clipping**

Write a test-only oracle that clones the entire small framebuffer before copying. Cover right/left horizontal overlap, down/up vertical overlap, source clipping, and destination clipping:

```rust
for case in COPY_RECT_CASES {
    let mut actual = numbered_framebuffer(6, 5);
    let expected = copy_rect_snapshot_oracle(&actual, case);
    actual.copy_rect(case.sx, case.sy, case.dx, case.dy, case.w, case.h);
    assert_eq!(actual.pixels(), expected.as_slice(), "{case:?}");
}
```

The oracle may allocate because it is test-only; production may not.

- [ ] **Step 2: Run RED against one large-area resource assertion**

Add a source inspection assertion in the test module using `include_str!("framebuffer.rs")`. Split at the first top-level `#[cfg(test)]`, isolate the production `fn copy_rect` body, and reject the old rectangle-sized allocation expression there; this prevents the test's own string literal from matching itself. Run:

```powershell
cargo test copy_rect_does_not_allocate_a_rectangle_sized_temporary
```

Expected: fails on the current implementation.

- [ ] **Step 3: Implement directional row copying**

Compute the valid width/height intersection with checked arithmetic. If `dy > sy` and vertical ranges overlap, iterate rows in reverse; otherwise forward. For each fully valid source/destination row use `self.pixels.copy_within(source_range, destination_start)`, which handles horizontal overlap. Preserve the existing out-of-bounds-source zero-fill behavior for the clipped portion without allocating.

- [ ] **Step 4: Run GREEN**

```powershell
cargo test copy_rect_
cargo test framebuffer_
```

Expected: every oracle case passes and the source assertion finds no rectangle-sized temporary.

- [ ] **Step 5: Record checkpoint**

Record maximum additional production memory as O(1) per operation.

---

### Task 6: Convert mutex poisoning to errors and join both viewer readers

**Files:**
- Create: `src/sync_util.rs`
- Modify: `src/main.rs` module declarations
- Modify: `src/viewer.rs:1-230`
- Modify: `src/vnc/hpss_viewer.rs:1-1915`
- Modify: `src/vnc/client.rs:90-205`
- Modify: `src/proxy.rs`
- Modify: `src/arp.rs:170-250`
- Test: `src/sync_util.rs`, `src/viewer.rs`, and `src/vnc/hpss_viewer.rs`

**Interfaces:**
- Produces: `lock_or_error`, `store_error_once`, classic `shutdown_reader`, and deterministic reader handles.

- [ ] **Step 1: Add RED poison and shutdown tests**

```rust
#[test]
fn poisoned_user_path_lock_returns_contextual_error() {
    let value = Arc::new(Mutex::new(1usize));
    let poisoned = Arc::clone(&value);
    let _ = thread::spawn(move || {
        let _guard = poisoned.lock().unwrap();
        panic!("poison for test");
    }).join();
    let error = lock_or_error(&value, "测试状态").unwrap_err();
    assert!(error.to_string().contains("测试状态"));
}
```

Add a loopback classic-reader test: start a reader blocked in `read_server_message`, call `shutdown_reader`, assert it returns within one second and the handle is joined. Add the equivalent HPSS helper regression if not already covered.

- [ ] **Step 2: Run RED**

```powershell
cargo test poisoned_user_path_lock_returns_contextual_error
cargo test classic_viewer_shutdown_unblocks_and_joins_reader
```

Expected: helper/classic join path do not exist.

- [ ] **Step 3: Implement shared lock helpers**

```rust
pub fn lock_or_error<'a, T>(mutex: &'a Mutex<T>, label: &str) -> Result<MutexGuard<'a, T>> {
    mutex.lock().map_err(|_| anyhow!("{label}互斥锁已损坏"))
}

pub fn store_error_once(slot: &Mutex<Option<String>>, message: String) {
    if let Ok(mut slot) = slot.lock() {
        if slot.is_none() { *slot = Some(message); }
    }
}
```

Use `lock_or_error` in every fallible user/network path. Callbacks/threads that cannot return use `store_error_once`, set closing, and exit; they do not panic on a poisoned diagnostic slot.

- [ ] **Step 4: Refactor classic viewer to own the reader handle**

Store `let reader = thread::spawn(...)`. Move the loop body to a testable `reader_loop` returning `Result<()>`; the thread converts an unexpected error to `store_error_once`. On every main-loop exit path, execute:

```rust
fn shutdown_reader(
    closing: &AtomicBool,
    write_stream: &Mutex<TcpStream>,
    reader: JoinHandle<()>,
) -> Result<()> {
    closing.store(true, Ordering::Relaxed);
    let poisoned = write_stream.is_poisoned();
    let stream = match write_stream.lock() {
        Ok(stream) => stream,
        Err(error) => error.into_inner(),
    };
    let shutdown = stream.shutdown(Shutdown::Both);
    drop(stream);
    let joined = reader.join();
    if poisoned {
        bail!("viewer 写 socket 互斥锁已损坏");
    }
    shutdown.context("关闭 viewer socket 失败")?;
    joined.map_err(|_| anyhow!("viewer 读线程 panic"))?;
    Ok(())
}
```

The teardown-only poison recovery exists solely to unblock and join before reporting the poison error; normal user/network paths still use `lock_or_error`. Treat an already-disconnected shutdown error as benign only when its exact `ErrorKind` is documented and tested.

- [ ] **Step 5: Sweep HPSS/client/proxy/ARP production unwraps**

Run `rg -n "lock\(\)\.unwrap\(\)" src`. Convert HPSS viewer and `RfbConn` locks to the shared error helper. Refactor ARP sweep workers to return local `Vec<Host>` values and join them, eliminating the shared poisoned mutex. Keep `unwrap` in tests and provable fixed-size `try_into` sites only when preceded by an explicit invariant; use `expect` with that invariant text.

- [ ] **Step 6: Run GREEN**

```powershell
cargo test poisoned_user_path_
cargo test classic_viewer_shutdown_
cargo test shutdown_reader
rg -n "lock\(\)\.unwrap\(\)" src
```

Expected: tests pass; search returns test-only hits or none. Inspect each hit rather than suppressing it.

- [ ] **Step 7: Record checkpoint**

Record reader ownership and each remaining `unwrap` invariant.

---

### Task 7: Verify the complete runtime/resource slice

**Files:**
- Modify only to repair failures caused by Tasks 1-6.

- [ ] **Step 1: Run source guards**

```powershell
rg -n "maxOutBufBytes as usize|Instant::now\(\) \+ Duration|lock\(\)\.unwrap\(\)|Vec::with_capacity\(w \* h\)" src
rg -n "ceil\(\) as usize|saturating_mul\(drawable_size" src
```

Expected: no production hits; any test hit is reviewed explicitly.

- [ ] **Step 2: Run the complete matrix**

```powershell
cargo fmt -- --check
cargo test
cargo test --no-default-features
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo build --all-features
cargo build --no-default-features
```

Expected: all commands exit 0.

- [ ] **Step 3: Exercise local-only CLI validation**

```powershell
cargo run --features viewer -- view 127.0.0.1:1 --scale inf
cargo run --features viewer -- hpssview 127.0.0.1:1 --scale 0
```

Expected: each exits at argument validation without attempting a connection.

- [ ] **Step 4: Record plan checkpoint**

Record test totals, build modes, help output, CLI rejection messages, and source-guard results.
