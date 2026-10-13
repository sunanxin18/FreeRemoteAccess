use crate::{
    app_adapter::{AppEngine, Command, Edit, InputEpoch},
    input_adapter::ViewGeometry,
    *,
};
use frd_core::{
    InputEvent, KeyState, Modifiers, PhysicalKeyCode, PixelRect, PixelSize, PointerButtons,
    SecretBuffer, WheelDelta,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};
fn apps() -> &'static Mutex<(u64, BTreeMap<u64, Arc<AppEngine>>)> {
    static APPS: OnceLock<Mutex<(u64, BTreeMap<u64, Arc<AppEngine>>)>> = OnceLock::new();
    APPS.get_or_init(|| Mutex::new((0, BTreeMap::new())))
}
fn app(id: u64) -> Result<Arc<AppEngine>, i32> {
    apps()
        .lock()
        .map_err(|_| PANIC)?
        .1
        .get(&id)
        .cloned()
        .ok_or(INVALID_HANDLE)
}
unsafe fn bytes<'a>(ptr: *const u8, len: usize, max: usize) -> Result<&'a [u8], i32> {
    if len > max || (ptr.is_null() && len != 0) {
        return Err(INVALID_ARGUMENT);
    }
    if len == 0 {
        return Ok(&[]);
    }
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_create(path: *const u8, len: usize, out: *mut u64) -> i32 {
    boundary(|| {
        if out.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        unsafe { *out = 0 };
        let path = std::str::from_utf8(unsafe { bytes(path, len, 4096)? })
            .map_err(|_| INVALID_ARGUMENT)?;
        let root = PathBuf::from(path);
        if !root.is_absolute()
            || root
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(INVALID_ARGUMENT);
        }
        let engine = Arc::new(AppEngine::unregistered(root)?);
        let mut a = apps().lock().map_err(|_| PANIC)?;
        a.0 = a.0.checked_add(1).ok_or(EXHAUSTED)?;
        let id = a.0;
        a.1.insert(id, engine);
        unsafe { *out = id };
        Ok(())
    })
}
#[no_mangle]
pub extern "C" fn frd_ohos_app_destroy(id: u64) -> i32 {
    boundary(|| {
        let e = apps()
            .lock()
            .map_err(|_| PANIC)?
            .1
            .remove(&id)
            .ok_or(INVALID_HANDLE)?;
        e.stop();
        Ok(())
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_edit(id: u64, p: *const u8, n: usize) -> i32 {
    boundary(|| {
        let edit: Edit = serde_json::from_slice(unsafe { bytes(p, n, 16_384)? })
            .map_err(|_| INVALID_ARGUMENT)?;
        if edit.address.len() > 4096 || edit.username.len() > 4096 || edit.protocol.len() > 128 {
            return Err(INVALID_ARGUMENT);
        }
        app(id)?.send(Command::Edit(edit))
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_password(id: u64, p: *const u8, n: usize) -> i32 {
    boundary(|| {
        let b = unsafe { bytes(p, n, 65_536)? };
        std::str::from_utf8(b).map_err(|_| INVALID_ARGUMENT)?;
        app(id)?.send(Command::Password(SecretBuffer::new(b.to_vec())))
    })
}
#[no_mangle]
pub extern "C" fn frd_ohos_app_action(
    id: u64,
    action: u32,
    event: u64,
    flags: u32,
    index: u32,
) -> i32 {
    boundary(|| {
        let a = app(id)?;
        match action {
            1 => a.send(Command::Submit {
                event,
                composition: flags & 1 != 0,
                repeat: flags & 2 != 0,
            }),
            2 => {
                a.cancel();
                Ok(())
            }
            3 => a.send(Command::Intent(frd_app::AppIntent::ReturnToConnection)),
            4 => a.send(Command::Select(index as usize)),
            _ => Err(INVALID_ARGUMENT),
        }
    })
}
#[no_mangle]
pub extern "C" fn frd_ohos_app_focus(id: u64, foreground: u32, focused: u32, remote: u32) -> i32 {
    boundary(|| {
        if foreground > 1 || focused > 1 || remote > 1 {
            return Err(INVALID_ARGUMENT);
        }
        let a = app(id)?;
        if foreground == 0 || focused == 0 || remote == 0 {
            a.lose_focus();
            return Ok(());
        }
        a.send(Command::Focus {
            foreground: true,
            focused: true,
            remote: true,
        })
    })
}
#[repr(C)]
pub struct AppGeometry {
    pub abi_version: u32,
    pub struct_size: u32,
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    pub content_width: u32,
    pub content_height: u32,
    pub scale: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_geometry(id: u64, g: *const AppGeometry) -> i32 {
    boundary(|| {
        if g.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        let g = unsafe { &*g };
        if g.abi_version != 1 || g.struct_size as usize != std::mem::size_of::<AppGeometry>() {
            return Err(INVALID_ARGUMENT);
        }
        let v = ViewGeometry {
            drawable: PixelSize {
                width: g.width,
                height: g.height,
            },
            bounds: PixelRect {
                x: g.x,
                y: g.y,
                width: g.content_width,
                height: g.content_height,
            },
            scale: g.scale,
            pan_x: g.pan_x,
            pan_y: g.pan_y,
        };
        if v.viewport(PixelSize {
            width: 1,
            height: 1,
        })
        .is_none()
        {
            return Err(INVALID_ARGUMENT);
        }
        app(id)?.send(Command::Geometry(v))
    })
}
#[repr(C)]
pub struct AppInput {
    pub abi_version: u32,
    pub struct_size: u32,
    pub session: u64,
    pub generation: u64,
    pub revision: u64,
    pub kind: u32,
    pub flags: u32,
    pub x: f32,
    pub y: f32,
    pub hid: u32,
    pub modifiers: u32,
    pub wheel_x: i32,
    pub wheel_y: i32,
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_input(id: u64, p: *const AppInput) -> i32 {
    boundary(|| {
        if p.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        let p = unsafe { &*p };
        if p.abi_version != 1 || p.struct_size as usize != std::mem::size_of::<AppInput>() {
            return Err(INVALID_ARGUMENT);
        }
        let a = app(id)?;
        let epoch = InputEpoch {
            session: p.session,
            generation: p.generation,
            revision: p.revision,
        };
        match p.kind {
            1 => {
                if !p.x.is_finite()
                    || !p.y.is_finite()
                    || p.flags > 7
                    || !(-127..=127).contains(&p.wheel_x)
                    || !(-127..=127).contains(&p.wheel_y)
                {
                    return Err(INVALID_ARGUMENT);
                }
                a.send(Command::Pointer(
                    epoch,
                    p.x,
                    p.y,
                    PointerButtons {
                        primary: p.flags & 1 != 0,
                        middle: p.flags & 2 != 0,
                        secondary: p.flags & 4 != 0,
                    },
                    WheelDelta {
                        horizontal: p.wheel_x as i8,
                        vertical: p.wheel_y as i8,
                    },
                ))
            }
            2 => {
                if p.hid < 4 || p.hid > 0xe7 || p.flags > 1 || p.modifiers > 15 {
                    return Err(INVALID_ARGUMENT);
                }
                a.send(Command::Input(
                    epoch,
                    InputEvent::PhysicalKey {
                        code: PhysicalKeyCode::from_usb_hid_usage(p.hid as u16),
                        state: if p.flags == 1 {
                            KeyState::Pressed
                        } else {
                            KeyState::Released
                        },
                        modifiers: Modifiers {
                            shift: p.modifiers & 1 != 0,
                            control: p.modifiers & 2 != 0,
                            alt: p.modifiers & 4 != 0,
                            meta: p.modifiers & 8 != 0,
                        },
                    },
                    p.flags == 1,
                ))
            }
            3 => {
                a.lose_focus();
                Ok(())
            }
            _ => Err(INVALID_ARGUMENT),
        }
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_text(
    id: u64,
    session: u64,
    generation: u64,
    revision: u64,
    p: *const u8,
    n: usize,
) -> i32 {
    boundary(|| {
        let text =
            std::str::from_utf8(unsafe { bytes(p, n, 16_384)? }).map_err(|_| INVALID_ARGUMENT)?;
        if text.is_empty() {
            return Ok(());
        }
        app(id)?.send(Command::Input(
            InputEpoch {
                session,
                generation,
                revision,
            },
            InputEvent::Text {
                utf8: text.to_owned(),
            },
            false,
        ))
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_snapshot(id: u64, out: *mut *mut u8, len: *mut usize) -> i32 {
    boundary(|| {
        if out.is_null() || len.is_null() {
            return Err(INVALID_ARGUMENT);
        }
        unsafe {
            *out = std::ptr::null_mut();
            *len = 0
        };
        let snapshot = app(id)?.snapshot()?.into_boxed_slice();
        unsafe {
            *len = snapshot.len();
            *out = Box::into_raw(snapshot) as *mut u8
        };
        Ok(())
    })
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_snapshot_free(p: *mut u8, n: usize) {
    if !p.is_null() {
        unsafe { drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(p, n))) }
    }
}
#[no_mangle]
pub unsafe extern "C" fn frd_ohos_app_huks_probe(id: u64, path: *const u8, len: usize) -> i32 {
    boundary(|| {
        #[cfg(debug_assertions)]
        {
            let root = PathBuf::from(
                std::str::from_utf8(unsafe { bytes(path, len, 4096)? })
                    .map_err(|_| INVALID_ARGUMENT)?,
            );
            if !root.is_absolute() {
                return Err(INVALID_ARGUMENT);
            }
            app(id)?.send(Command::Probe(root.join("huks-probe")))
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = (id, path, len);
            Err(UNSUPPORTED_PLATFORM)
        }
    })
}
