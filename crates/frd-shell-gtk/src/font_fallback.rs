//! Linux GTK 私有字体配置；只给本窗口树增加随包的简体中文回退。
//!
//! Pango 1.52 尚未提供 `FontMap::add_font_file` 的稳定 API，因此这里仅通过
//! fontconfig/PangoCairo 的公开 C ABI 建立私有 font map。不会修改进程全局
//! fontconfig，也不会写入用户配置或安装系统字体。

use gtk4::pango::{self, glib, prelude::*};
use libloading::Library;
use std::{
    ffi::{c_int, c_void, CString},
    path::{Path, PathBuf},
};

type FcConfig = c_void;
type FontMapPtr = *mut pango::ffi::PangoFontMap;
type InitConfig = unsafe extern "C" fn() -> *mut FcConfig;
type AddFont = unsafe extern "C" fn(*mut FcConfig, *const u8) -> c_int;
type BuildFonts = unsafe extern "C" fn(*mut FcConfig) -> c_int;
type DestroyConfig = unsafe extern "C" fn(*mut FcConfig);
type NewFontMap = unsafe extern "C" fn() -> FontMapPtr;
type SetConfig = unsafe extern "C" fn(FontMapPtr, *mut FcConfig);

unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Option<T> {
    library.get::<T>(name).ok().map(|symbol| *symbol)
}

fn bundled_font_path() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let executable_dir = executable.parent()?;
    let packaged =
        executable_dir.join("share/fonts/freeremotedesk/NotoSansSC-VariableFont_wght.ttf");
    if packaged.is_file() {
        return Some(packaged);
    }
    // 开发树回退只用于本地调试；完整包必须使用上面的随包路径。
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts/noto-sans-sc/NotoSansSC-VariableFont_wght.ttf");
    source.is_file().then_some(source)
}

/// 持有私有 Pango font map 以及其 ABI 动态库；不可复制，必须与窗口树同寿命。
pub struct BundledFontMap {
    map: Option<pango::FontMap>,
    config: *mut FcConfig,
    destroy_config: DestroyConfig,
    _fontconfig: Library,
    _pango: Library,
    _pangocairo: Library,
}

impl BundledFontMap {
    pub fn load() -> Option<Self> {
        let font_path = bundled_font_path()?;
        let path = CString::new(font_path.to_str()?).ok()?;
        // Linux 发行版的 soname 由打包目标负责提供；缺少任一 ABI 时保持宿主字体。
        let fontconfig = unsafe { Library::new("libfontconfig.so.1").ok()? };
        let pango = unsafe { Library::new("libpango-1.0.so.0").ok()? };
        let pangocairo = unsafe { Library::new("libpangocairo-1.0.so.0").ok()? };
        let init: InitConfig = unsafe { symbol(&fontconfig, b"FcInitLoadConfigAndFonts\0")? };
        let add_font: AddFont = unsafe { symbol(&fontconfig, b"FcConfigAppFontAddFile\0")? };
        let build_fonts: BuildFonts = unsafe { symbol(&fontconfig, b"FcConfigBuildFonts\0")? };
        let destroy_config: DestroyConfig = unsafe { symbol(&fontconfig, b"FcConfigDestroy\0")? };
        let new_font_map: NewFontMap =
            unsafe { symbol(&pangocairo, b"pango_cairo_font_map_new\0")? };
        let set_config: SetConfig = unsafe { symbol(&pango, b"pango_fc_font_map_set_config\0")? };
        let config = unsafe { init() };
        if config.is_null()
            || unsafe { add_font(config, path.as_ptr().cast()) } == 0
            || unsafe { build_fonts(config) } == 0
        {
            if !config.is_null() {
                unsafe { destroy_config(config) };
            }
            return None;
        }
        let map_ptr = unsafe { new_font_map() };
        if map_ptr.is_null() {
            unsafe { destroy_config(config) };
            return None;
        }
        unsafe { set_config(map_ptr, config) };
        // pango_fc_font_map_set_config 借用 config，故由此对象在 map 之后销毁它。
        let map = unsafe { glib::translate::from_glib_full(map_ptr) };
        Some(Self {
            map: Some(map),
            config,
            destroy_config,
            _fontconfig: fontconfig,
            _pango: pango,
            _pangocairo: pangocairo,
        })
    }

    pub fn map(&self) -> Option<&pango::FontMap> {
        self.map.as_ref()
    }

    pub fn has_bundled_family(&self) -> bool {
        self.map.as_ref().is_some_and(|map| {
            map.list_families()
                .iter()
                .any(|family| family.name().starts_with("Noto Sans SC"))
        })
    }
}

impl Drop for BundledFontMap {
    fn drop(&mut self) {
        // 先释放 Pango 对 config 的借用，再销毁私有 config。
        self.map.take();
        if !self.config.is_null() {
            unsafe { (self.destroy_config)(self.config) };
        }
    }
}
