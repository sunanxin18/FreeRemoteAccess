//! HarmonyOS 原生窗口生命周期桥；不接收协议数据或密码。
//!
//! 引擎句柄是不复用的整数。每个引擎拥有一个串行渲染线程；registry 锁在
//! 阻塞前释放。attach 的借用窗口仅在调用期间跨线程移交，必须等待独立
//! 引用取得和初始化结果；没有超时后仍访问借用窗口的路径。

#[cfg(all(
    any(
        feature = "normal-hap-rdp-validation",
        feature = "normal-hap-apple-mvs-validation"
    ),
    any(
        feature = "native-presentation-fixture",
        feature = "normal-hap-codec-fixture",
        feature = "normal-hap-codec-benchmark",
        feature = "normal-hap-avc444-profile",
        feature = "normal-hap-hevc444-profile"
    )
))]
compile_error!("普通协议验证与合成/codec 测量模式必须分别构建，包括 release");

#[cfg(all(
    debug_assertions,
    feature = "normal-hap-codec-fixture",
    feature = "native-presentation-fixture"
))]
compile_error!("两个合成夹具必须分别构建");

#[cfg(any(
    all(
        feature = "normal-hap-codec-benchmark",
        feature = "normal-hap-avc444-profile"
    ),
    all(
        feature = "normal-hap-codec-benchmark",
        feature = "normal-hap-hevc444-profile"
    ),
    all(
        feature = "normal-hap-avc444-profile",
        feature = "normal-hap-hevc444-profile"
    ),
    all(
        any(
            feature = "normal-hap-codec-benchmark",
            feature = "normal-hap-avc444-profile",
            feature = "normal-hap-hevc444-profile"
        ),
        any(
            feature = "normal-hap-codec-fixture",
            feature = "native-presentation-fixture"
        )
    )
))]
compile_error!("测量与正确性/呈现夹具必须分别构建");
mod app_abi;
pub mod app_adapter;
#[cfg(all(
    debug_assertions,
    feature = "normal-hap-codec-fixture",
    any(test, target_env = "ohos")
))]
mod codec_fixture;
#[cfg(all(
    any(test, target_env = "ohos"),
    any(
        all(debug_assertions, feature = "normal-hap-codec-fixture"),
        any(
            feature = "normal-hap-codec-benchmark",
            feature = "normal-hap-avc444-profile",
            feature = "normal-hap-hevc444-profile"
        )
    )
))]
mod codec_fixture_common;
#[cfg(all(
    any(test, target_env = "ohos"),
    any(
        feature = "normal-hap-codec-benchmark",
        feature = "normal-hap-avc444-profile",
        feature = "normal-hap-hevc444-profile"
    )
))]
mod codec_measurement;
mod composition;
#[cfg(all(debug_assertions, feature = "native-presentation-fixture"))]
mod fixture;
pub mod input_adapter;
#[cfg(any(test, target_env = "ohos"))]
mod limits;
mod local_display;
#[cfg(target_env = "ohos")]
mod ohos;
mod panic_scope;
mod presentation;
#[cfg(test)]
mod tests;
mod worker;

use std::{
    collections::BTreeMap,
    ffi::c_void,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex, OnceLock},
};
use worker::{Command, Engine};

pub const OK: i32 = 0;
pub const INVALID_HANDLE: i32 = -1;
pub const INVALID_ARGUMENT: i32 = -2;
pub const STALE_GENERATION: i32 = -3;
pub const UNSUPPORTED_PLATFORM: i32 = -4;
pub const WINDOW_REFERENCE_FAILED: i32 = -5;
pub const GPU_FAILED: i32 = -6;
pub const WORKER_STOPPED: i32 = -7;
pub const PANIC: i32 = -8;
pub const EXHAUSTED: i32 = -9;
pub const NOT_ATTACHED: i32 = -10;
pub const QUEUE_FULL: i32 = -11;

/// 固定宽度、无秘密的图形 POC 诊断，绝不代表网络会话成功。
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct PocStatus {
    pub surface_generation: u64,
    pub presented_frames: u64,
    pub attaches: u64,
    pub resizes: u64,
    pub detaches: u64,
    pub width: u32,
    pub height: u32,
    pub attached: u32,
    pub foreground: u32,
    pub last_error: i32,
    pub reserved: u32,
}

#[derive(Default)]
struct Registry {
    next: u64,
    engines: BTreeMap<u64, Arc<Engine>>,
}
fn registry() -> &'static Mutex<Registry> {
    static REG: OnceLock<Mutex<Registry>> = OnceLock::new();
    REG.get_or_init(Mutex::default)
}
fn lookup(id: u64) -> Result<Arc<Engine>, i32> {
    registry()
        .lock()
        .map_err(|_| PANIC)?
        .engines
        .get(&id)
        .cloned()
        .ok_or(INVALID_HANDLE)
}
fn boundary(f: impl FnOnce() -> Result<(), i32>) -> i32 {
    // std 禁止在已经 panicking 的线程修改 hook；不让初始化错误越过 ABI。
    if std::thread::panicking() {
        return PANIC;
    }
    let _scope = panic_scope::Scope::enter();
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => OK,
        Ok(Err(code)) => code,
        Err(_) => PANIC,
    }
}

#[no_mangle]
pub extern "C" fn frd_ohos_abi_version() -> u32 {
    1
}
#[no_mangle]
pub extern "C" fn frd_ohos_create() -> u64 {
    if std::thread::panicking() {
        return 0;
    }
    let _scope = panic_scope::Scope::enter();
    catch_unwind(AssertUnwindSafe(|| {
        let id = {
            let mut registry = registry().lock().map_err(|_| PANIC)?;
            registry.next = registry.next.checked_add(1).ok_or(EXHAUSTED)?;
            registry.next
        };
        let engine = Arc::new(Engine::spawn(platform_backend)?);
        registry()
            .lock()
            .map_err(|_| PANIC)?
            .engines
            .insert(id, engine);
        Ok::<_, i32>(id)
    }))
    .ok()
    .and_then(Result::ok)
    .unwrap_or(0)
}
fn platform_backend() -> Box<dyn worker::Backend> {
    #[cfg(target_env = "ohos")]
    {
        Box::new(ohos::OhosBackend::default())
    }
    #[cfg(not(target_env = "ohos"))]
    {
        Box::new(worker::UnavailableBackend)
    }
}

#[no_mangle]
pub extern "C" fn frd_ohos_destroy(engine: u64) -> i32 {
    boundary(|| {
        let owned = registry()
            .lock()
            .map_err(|_| PANIC)?
            .engines
            .remove(&engine)
            .ok_or(INVALID_HANDLE)?;
        owned.shutdown()
    })
}

/// # Safety
/// `window` 是框架提供的 OHNativeWindow，在本次调用返回前保持有效。
/// `out_surface_generation` 必须是可写、对齐的 u64；非空坏指针不属于 ABI 合约。
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_attach(
    engine: u64,
    window: *mut c_void,
    width: u32,
    height: u32,
    out_surface_generation: *mut u64,
) -> i32 {
    boundary(|| {
        if out_surface_generation.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        // SAFETY: 调用方保证输出存储可写且对齐，所有失败清零。
        unsafe {
            *out_surface_generation = 0;
        }
        let owned = lookup(engine)?;
        if window.is_null() || width == 0 || height == 0 {
            return Err(INVALID_ARGUMENT);
        }
        let generation = owned.call(Command::Attach {
            window: window as usize,
            width,
            height,
        })?;
        // SAFETY: 输出存储在整个调用期有效。
        unsafe {
            *out_surface_generation = generation;
        }
        Ok(())
    })
}
#[no_mangle]
pub extern "C" fn frd_ohos_resize(
    engine: u64,
    surface_generation: u64,
    width: u32,
    height: u32,
) -> i32 {
    boundary(|| {
        lookup(engine)?.call(Command::Resize {
            generation: surface_generation,
            width,
            height,
        })?;
        Ok(())
    })
}
#[no_mangle]
pub extern "C" fn frd_ohos_detach(engine: u64, surface_generation: u64) -> i32 {
    boundary(|| {
        lookup(engine)?.call(Command::Detach(surface_generation))?;
        Ok(())
    })
}
#[no_mangle]
pub extern "C" fn frd_ohos_set_foreground(engine: u64, foreground: u32) -> i32 {
    boundary(|| {
        if foreground > 1 {
            return Err(INVALID_ARGUMENT);
        }
        lookup(engine)?.call(Command::Foreground(foreground != 0))?;
        Ok(())
    })
}
/// UI 帧回调只尝试入队；队列满时丢弃本次 tick，不阻塞 UI。
#[no_mangle]
pub extern "C" fn frd_ohos_poc_tick(engine: u64, surface_generation: u64) -> i32 {
    boundary(|| {
        let owned = lookup(engine)?;
        if !cfg!(debug_assertions) {
            return Err(UNSUPPORTED_PLATFORM);
        }
        owned.tick(surface_generation)
    })
}
/// # Safety
/// `out_status` 必须指向可写、对齐的完整 PocStatus，调用期保持有效。
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_poc_status(engine: u64, out_status: *mut PocStatus) -> i32 {
    boundary(|| {
        if out_status.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        let status = lookup(engine)?.status()?;
        // SAFETY: 调用方保证输出指针满足存储合约。
        unsafe {
            *out_status = status;
        }
        Ok(())
    })
}
