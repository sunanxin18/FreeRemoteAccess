//! GTK 4.14 当前设备的 XKB 物理名称查询；不产生输入或读取文本。
//! Wayland 使用 GTK 当前有效表（GTK 对非法 compositor 表可能保留旧表）；
//! X11 查询事件对应 master keyboard。每个新 press 重新查询，release 由宿主重用已发送 HID。
use crate::input_keymap::{physical_key_from_xkb_name, supported_xkb_key_names};
use frd_core::PhysicalKeyCode;
use gtk4::{gdk, glib, prelude::*};
use libloading::Library;
use std::{
    ffi::{c_char, c_int, c_void},
    marker::PhantomData,
    rc::Rc,
};
use x11_dl::xlib;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapError {
    WrongThread,
    NotCurrentKeyboard,
    UnsupportedBackend,
    LibraryUnavailable,
    SymbolUnavailable,
    KeymapUnavailable,
    X11Failure,
    UnknownKey,
    AmbiguousKey,
}
type Result<T> = std::result::Result<T, KeymapError>;

// 函数指针只能在持有对应 Library 的对象内使用。
unsafe fn symbol<T: Copy>(lib: &Library, name: &[u8]) -> Result<T> {
    unsafe {
        lib.get::<T>(name)
            .map(|f| *f)
            .map_err(|_| KeymapError::SymbolUnavailable)
    }
}
fn merge(found: &mut Option<PhysicalKeyCode>, candidate: Option<PhysicalKeyCode>) -> Result<()> {
    if let Some(candidate) = candidate {
        if found.is_some_and(|previous| previous != candidate) {
            return Err(KeymapError::AmbiguousKey);
        }
        *found = Some(candidate);
    }
    Ok(())
}

type MapRef = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type MapUnref = unsafe extern "C" fn(*mut c_void);
type GetName = unsafe extern "C" fn(*mut c_void, u32) -> *const c_char;
type ByName = unsafe extern "C" fn(*mut c_void, *const c_char) -> u32;
struct Xkb {
    _library: Library,
    reference: MapRef,
    unref: MapUnref,
    name: GetName,
    by_name: ByName,
}
impl Xkb {
    fn open() -> Result<Self> {
        unsafe {
            let library =
                Library::new("libxkbcommon.so.0").map_err(|_| KeymapError::LibraryUnavailable)?;
            Ok(Self {
                reference: symbol(&library, b"xkb_keymap_ref\0")?,
                unref: symbol(&library, b"xkb_keymap_unref\0")?,
                name: symbol(&library, b"xkb_keymap_key_get_name\0")?,
                by_name: symbol(&library, b"xkb_keymap_key_by_name\0")?,
                _library: library,
            })
        }
    }
    unsafe fn query(&self, borrowed: *mut c_void, code: u32) -> Result<PhysicalKeyCode> {
        if borrowed.is_null() {
            return Err(KeymapError::KeymapUnavailable);
        }
        let map = unsafe { (self.reference)(borrowed) };
        if map.is_null() {
            return Err(KeymapError::KeymapUnavailable);
        }
        let guard = MapGuard { map, api: self };
        let raw = unsafe { (self.name)(guard.map, code) };
        if raw.is_null() {
            return Err(KeymapError::UnknownKey);
        }
        // libxkbcommon 返回有效的 NUL 结尾键名。只读取至第一个 NUL，最多5字节，
        // 不截断长名称成合法四字节名称。
        let mut direct = [0u8; 4];
        let mut valid = true;
        for index in 0..=4 {
            let byte = unsafe { *raw.add(index) } as u8;
            if byte == 0 {
                break;
            }
            if index == 4 {
                valid = false;
                break;
            }
            direct[index] = byte;
        }
        let mut found = None;
        if valid {
            merge(&mut found, physical_key_from_xkb_name(&direct))?;
        }
        for candidate in supported_xkb_key_names() {
            let mut terminated = [0u8; 5];
            terminated[..4].copy_from_slice(candidate);
            if unsafe { (self.by_name)(guard.map, terminated.as_ptr().cast()) } == code {
                merge(&mut found, physical_key_from_xkb_name(candidate))?;
            }
        }
        found.ok_or(KeymapError::UnknownKey)
    }
}
struct MapGuard<'a> {
    map: *mut c_void,
    api: &'a Xkb,
}
impl Drop for MapGuard<'_> {
    fn drop(&mut self) {
        unsafe { (self.api.unref)(self.map) };
    }
}

/// 仅在 GTK 主线程创建/使用；不保存 borrowed keymap，不缓存设备名称表。
pub struct NativeKeymap {
    gtk: Library,
    xkb: Option<Xkb>,
    x11: Option<xlib::Xlib>,
    _main_thread: PhantomData<Rc<()>>,
}
impl NativeKeymap {
    pub fn new() -> Result<Self> {
        if !gtk4::is_initialized_main_thread() {
            return Err(KeymapError::WrongThread);
        }
        let gtk = unsafe { Library::new("libgtk-4.so.1") }
            .map_err(|_| KeymapError::LibraryUnavailable)?;
        Ok(Self {
            gtk,
            xkb: None,
            x11: None,
            _main_thread: PhantomData,
        })
    }
    pub fn physical_key(
        &mut self,
        device: &gdk::Device,
        hardware_code: u32,
    ) -> Result<PhysicalKeyCode> {
        if !gtk4::is_initialized_main_thread() {
            return Err(KeymapError::WrongThread);
        }
        if device.source() != gdk::InputSource::Keyboard
            || device.seat().keyboard().as_ref() != Some(device)
        {
            return Err(KeymapError::NotCurrentKeyboard);
        }
        let display = device.display();
        if self.backend_is(&display, b"gdk_wayland_display_get_type\0") {
            if self.xkb.is_none() {
                self.xkb = Some(Xkb::open()?);
            }
            unsafe {
                let getter: unsafe extern "C" fn(*mut gdk::ffi::GdkDevice) -> *mut c_void =
                    symbol(&self.gtk, b"gdk_wayland_device_get_xkb_keymap\0")?;
                self.xkb
                    .as_ref()
                    .unwrap()
                    .query(getter(device.as_ptr()), hardware_code)
            }
        } else if self.backend_is(&display, b"gdk_x11_display_get_type\0") {
            if self.x11.is_none() {
                self.x11 = Some(xlib::Xlib::open().map_err(|_| KeymapError::LibraryUnavailable)?);
            }
            unsafe { self.query_x11(device, &display, hardware_code) }
        } else {
            Err(KeymapError::UnsupportedBackend)
        }
    }
    fn backend_is(&self, display: &gdk::Display, getter: &[u8]) -> bool {
        unsafe {
            let Ok(get_type) =
                symbol::<unsafe extern "C" fn() -> glib::ffi::GType>(&self.gtk, getter)
            else {
                return false;
            };
            glib::gobject_ffi::g_type_check_instance_is_a(display.as_ptr().cast(), get_type()) != 0
        }
    }
    unsafe fn query_x11(
        &self,
        device: &gdk::Device,
        display: &gdk::Display,
        code: u32,
    ) -> Result<PhysicalKeyCode> {
        let api = self.x11.as_ref().unwrap();
        let get_display: unsafe extern "C" fn(*mut gdk::ffi::GdkDisplay) -> *mut xlib::Display =
            unsafe { symbol(&self.gtk, b"gdk_x11_display_get_xdisplay\0")? };
        let get_id: unsafe extern "C" fn(*mut gdk::ffi::GdkDevice) -> c_int =
            unsafe { symbol(&self.gtk, b"gdk_x11_device_get_id\0")? };
        let push: unsafe extern "C" fn(*mut gdk::ffi::GdkDisplay) =
            unsafe { symbol(&self.gtk, b"gdk_x11_display_error_trap_push\0")? };
        let pop: unsafe extern "C" fn(*mut gdk::ffi::GdkDisplay) -> c_int =
            unsafe { symbol(&self.gtk, b"gdk_x11_display_error_trap_pop\0")? };
        let raw = unsafe { get_display(display.as_ptr()) };
        let id = unsafe { get_id(device.as_ptr()) };
        if raw.is_null() || id <= 0 || id >= 256 {
            return Err(KeymapError::NotCurrentKeyboard);
        }
        unsafe { push(display.as_ptr()) };
        let trap = ErrorTrap { display, pop };
        // XkbGBN_KeyNamesMask(1<<5)，XkbGetKeyboard 接受 GBN 组件掩码。
        let keyboard = unsafe { (api.XkbGetKeyboard)(raw, 1 << 5, id as u32) };
        let result = if keyboard.is_null() {
            Err(KeymapError::KeymapUnavailable)
        } else {
            let keyboard = KeyboardGuard { keyboard, api };
            // XkbKeyNamesMask | XkbKeyAliasesMask。
            if unsafe { (*keyboard.keyboard).device_spec } != id as u16 {
                Err(KeymapError::NotCurrentKeyboard)
            } else if unsafe { (api.XkbGetNames)(raw, (1 << 9) | (1 << 10), keyboard.keyboard) }
                != 0
            {
                Err(KeymapError::X11Failure)
            } else {
                unsafe { x11_names(keyboard.keyboard, code) }
            }
        };
        if trap.finish() != 0 {
            Err(KeymapError::X11Failure)
        } else {
            result
        }
    }
}
struct ErrorTrap<'a> {
    display: &'a gdk::Display,
    pop: unsafe extern "C" fn(*mut gdk::ffi::GdkDisplay) -> c_int,
}
impl ErrorTrap<'_> {
    fn finish(self) -> c_int {
        let result = unsafe { (self.pop)(self.display.as_ptr()) };
        std::mem::forget(self);
        result
    }
}
impl Drop for ErrorTrap<'_> {
    fn drop(&mut self) {
        unsafe { (self.pop)(self.display.as_ptr()) };
    }
}
struct KeyboardGuard<'a> {
    keyboard: xlib::XkbDescPtr,
    api: &'a xlib::Xlib,
}
impl Drop for KeyboardGuard<'_> {
    fn drop(&mut self) {
        unsafe { (self.api.XkbFreeKeyboard)(self.keyboard, 0, 1) };
    }
}
unsafe fn x11_names(keyboard: xlib::XkbDescPtr, code: u32) -> Result<PhysicalKeyCode> {
    let keyboard = unsafe { &*keyboard };
    if code < u32::from(keyboard.min_key_code) || code > u32::from(keyboard.max_key_code) {
        return Err(KeymapError::UnknownKey);
    }
    let names = unsafe { keyboard.names.as_ref() }.ok_or(KeymapError::KeymapUnavailable)?;
    if names.keys.is_null() || (names.num_key_aliases != 0 && names.key_aliases.is_null()) {
        return Err(KeymapError::KeymapUnavailable);
    }
    // XKB 数字范围最多256项、alias最多255项。先复制固定字段，后续解析不持裸引用。
    let keys: Vec<(u32, [u8; 4])> = (keyboard.min_key_code..=keyboard.max_key_code)
        .map(|index| {
            (
                u32::from(index),
                unsafe { (*names.keys.add(usize::from(index))).name }.map(|byte| byte as u8),
            )
        })
        .collect();
    let aliases: Vec<([u8; 4], [u8; 4])> = (0..usize::from(names.num_key_aliases))
        .map(|index| {
            let alias = unsafe { &*names.key_aliases.add(index) };
            (
                alias.alias.map(|byte| byte as u8),
                alias.real.map(|byte| byte as u8),
            )
        })
        .collect();
    let mut found = None;
    for candidate in supported_xkb_key_names() {
        let mut target_matches = false;
        let mut other_matches = false;
        for (index, name) in &keys {
            let matches = name == candidate
                || aliases
                    .iter()
                    .any(|(alias, real)| alias == candidate && real == name);
            if matches {
                target_matches |= *index == code;
                other_matches |= *index != code;
            }
        }
        if target_matches {
            if other_matches {
                return Err(KeymapError::AmbiguousKey);
            }
            merge(&mut found, physical_key_from_xkb_name(candidate))?;
        }
    }
    found.ok_or(KeymapError::UnknownKey)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn x11_fixed_names_and_aliases_reject_conflicts_without_c_string_reads() {
        let mut keys = vec![xlib::_XkbKeyNameRec { name: [0; 4] }; 256];
        keys[38].name = (*b"TEST").map(|byte| byte as c_char);
        let mut aliases = [
            xlib::_XkbKeyAliasRec {
                real: (*b"TEST").map(|byte| byte as c_char),
                alias: (*b"AC01").map(|byte| byte as c_char),
            },
            xlib::_XkbKeyAliasRec {
                real: (*b"TEST").map(|byte| byte as c_char),
                alias: (*b"AD01").map(|byte| byte as c_char),
            },
        ];
        // x11-dl 的这两个 repr(C)结构只含整数、固定数组及裸指针，零初始化有效。
        let mut names: xlib::_XkbNamesRec = unsafe { std::mem::zeroed() };
        names.keys = keys.as_mut_ptr();
        names.key_aliases = aliases.as_mut_ptr();
        names.num_key_aliases = 1;
        let mut keyboard: xlib::_XkbDesc = unsafe { std::mem::zeroed() };
        keyboard.min_key_code = 8;
        keyboard.max_key_code = 255;
        keyboard.names = &mut names;
        assert_eq!(
            unsafe { x11_names(&mut keyboard, 38) }
                .unwrap()
                .usb_hid_usage(),
            4
        );
        names.num_key_aliases = 2;
        keyboard.names = &mut names;
        assert_eq!(
            unsafe { x11_names(&mut keyboard, 38) },
            Err(KeymapError::AmbiguousKey)
        );
        names.num_key_aliases = 1;
        keyboard.names = &mut names;
        keys[39].name = (*b"AC01").map(|byte| byte as c_char);
        assert_eq!(
            unsafe { x11_names(&mut keyboard, 38) },
            Err(KeymapError::AmbiguousKey)
        );
        assert_eq!(
            unsafe { x11_names(&mut keyboard, 39) },
            Err(KeymapError::AmbiguousKey)
        );
        assert_eq!(
            unsafe { x11_names(&mut keyboard, 7) },
            Err(KeymapError::UnknownKey)
        );
        assert_eq!(
            unsafe { x11_names(&mut keyboard, 256) },
            Err(KeymapError::UnknownKey)
        );
    }
    // 真实 libxkbcommon 字符串查询，不是 compositor/GTK 事件验收。
    unsafe fn fixture(api: &Xkb, text: &str) -> *mut c_void {
        type NewContext = unsafe extern "C" fn(u32) -> *mut c_void;
        type UnrefContext = unsafe extern "C" fn(*mut c_void);
        type NewMap = unsafe extern "C" fn(*mut c_void, *const c_char, u32, u32) -> *mut c_void;
        let new: NewContext = unsafe { symbol(&api._library, b"xkb_context_new\0").unwrap() };
        let unref: UnrefContext = unsafe { symbol(&api._library, b"xkb_context_unref\0").unwrap() };
        let map: NewMap =
            unsafe { symbol(&api._library, b"xkb_keymap_new_from_string\0").unwrap() };
        let context = unsafe { new(0) };
        assert!(!context.is_null());
        let text = std::ffi::CString::new(text).unwrap();
        let result = unsafe { map(context, text.as_ptr(), 1, 0) };
        unsafe { unref(context) };
        assert!(!result.is_null());
        result
    }
    fn text(code: u32, symbol: &str, aliases: &str) -> String {
        format!(
            r#"xkb_keymap {{
            xkb_keycodes {{ minimum = 8; maximum = 255; <AC01> = {code}; {aliases} }};
            xkb_types {{ type "ONE_LEVEL" {{ modifiers = None; map[None] = Level1; level_name[Level1] = "Any"; }}; }};
            xkb_compatibility {{}};
            xkb_symbols {{ key <AC01> {{ type="ONE_LEVEL", [ {symbol} ] }}; }};
        }};"#
        )
    }
    #[test]
    fn native_xkb_library_resolves_renumbered_keys_independently_of_symbols() {
        let api = Xkb::open().expect("Linux fixture需要libxkbcommon.so.0");
        for (code, keysym) in [(38, "a"), (200, "q")] {
            let map = unsafe { fixture(&api, &text(code, keysym, "")) };
            let guard = MapGuard { map, api: &api };
            assert_eq!(
                unsafe { api.query(guard.map, code) }
                    .unwrap()
                    .usb_hid_usage(),
                4
            );
            assert_eq!(
                unsafe { api.query(guard.map, 201) },
                Err(KeymapError::UnknownKey)
            );
        }
    }
    #[test]
    fn native_xkb_library_rejects_conflicting_standard_alias() {
        let api = Xkb::open().unwrap();
        let map = unsafe { fixture(&api, &text(38, "a", "alias <AD01> = <AC01>;")) };
        let guard = MapGuard { map, api: &api };
        assert_eq!(
            unsafe { api.query(guard.map, 38) },
            Err(KeymapError::AmbiguousKey)
        );
    }
    #[test]
    fn native_xkb_library_finds_standard_alias_of_unknown_real_name() {
        let api = Xkb::open().unwrap();
        let source = text(200, "a", "")
            .replace("AC01", "TEST")
            .replace("<TEST> = 200;", "<TEST> = 200; alias <AC01> = <TEST>;");
        let map = unsafe { fixture(&api, &source) };
        let guard = MapGuard { map, api: &api };
        assert_eq!(
            unsafe { api.query(guard.map, 200) }
                .unwrap()
                .usb_hid_usage(),
            4
        );
    }
}
