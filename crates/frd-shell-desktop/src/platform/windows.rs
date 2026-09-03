use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmDefWindowProc, DwmExtendFrameIntoClientArea};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IsZoomed, SetWindowPos, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTCLOSE,
    HTLEFT, HTMAXBUTTON, HTMINBUTTON, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, NCCALCSIZE_PARAMS,
    SM_CXPADDEDBORDER, SM_CXSIZE, SM_CXSIZEFRAME, SM_CYSIZEFRAME, SWP_FRAMECHANGED, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, WM_CLOSE, WM_NCCALCSIZE, WM_NCHITTEST,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{
    ChromeHit, ChromeHitRegions, NativeChromeInsets, WindowChromeAction, WindowChromeAdapter,
    WindowChromeError, TITLE_BAR_HEIGHT_POINTS,
};

const SUBCLASS_ID: usize = 0x4652_4443;

struct SubclassState {
    regions: Mutex<Option<ChromeHitRegions>>,
    resize_metrics: Mutex<(i32, i32)>,
}

pub(crate) struct PlatformWindowChrome {
    hwnd: Option<HWND>,
    state: Box<SubclassState>,
}

impl PlatformWindowChrome {
    pub(crate) fn new() -> Self {
        Self {
            hwnd: None,
            state: Box::new(SubclassState {
                regions: Mutex::new(None),
                resize_metrics: Mutex::new((8, 8)),
            }),
        }
    }

    fn hwnd(window: &winit::window::Window) -> Result<HWND, WindowChromeError> {
        let handle = window
            .window_handle()
            .map_err(|_| WindowChromeError::UnsupportedWindow)?;
        match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as HWND),
            _ => Err(WindowChromeError::UnsupportedWindow),
        }
    }

    fn refresh_native_metrics(&mut self, hwnd: HWND) -> Result<(), WindowChromeError> {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let resize_x = unsafe {
            GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
        }
        .max(1);
        let resize_y = unsafe {
            GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi)
                + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
        }
        .max(1);
        if let Ok(mut metrics) = self.state.resize_metrics.lock() {
            *metrics = (resize_x, resize_y);
        } else {
            return Err(WindowChromeError::PlatformCallFailed);
        }
        let top = (TITLE_BAR_HEIGHT_POINTS * f64::from(dpi) / 96.0).ceil() as i32;
        let margins = MARGINS {
            cxLeftWidth: 0,
            cxRightWidth: 0,
            cyTopHeight: top,
            cyBottomHeight: 0,
        };
        if unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) } < 0 {
            return Err(WindowChromeError::PlatformCallFailed);
        }
        Ok(())
    }
}

impl WindowChromeAdapter for PlatformWindowChrome {
    fn configure(&mut self, window: &winit::window::Window) -> Result<(), WindowChromeError> {
        let hwnd = Self::hwnd(window)?;
        self.refresh_native_metrics(hwnd)?;
        let state_ptr = (&mut *self.state) as *mut SubclassState as usize;
        if unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, state_ptr) } == 0 {
            return Err(WindowChromeError::PlatformCallFailed);
        }
        self.hwnd = Some(hwnd);
        if unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            )
        } == 0
        {
            unsafe {
                RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
            }
            self.hwnd = None;
            return Err(WindowChromeError::PlatformCallFailed);
        }
        Ok(())
    }

    fn refresh_for_dpi(&mut self, window: &winit::window::Window) -> Result<(), WindowChromeError> {
        self.refresh_native_metrics(Self::hwnd(window)?)
    }

    fn native_insets(&self, window: &winit::window::Window) -> NativeChromeInsets {
        let Ok(hwnd) = Self::hwnd(window) else {
            return NativeChromeInsets::default();
        };
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let button_width = unsafe { GetSystemMetricsForDpi(SM_CXSIZE, dpi) }.max(0) as u32;
        NativeChromeInsets {
            leading_px: 0,
            trailing_px: button_width.saturating_mul(3),
        }
    }

    fn publish_hit_regions(&mut self, regions: Option<ChromeHitRegions>) {
        if let Ok(mut slot) = self.state.regions.lock() {
            *slot = regions;
        }
    }

    fn execute(&mut self, window: &winit::window::Window, action: WindowChromeAction) {
        match action {
            WindowChromeAction::Minimize => window.set_minimized(true),
            WindowChromeAction::ToggleMaximize => window.set_maximized(!window.is_maximized()),
            WindowChromeAction::Close => {
                if let Ok(hwnd) = Self::hwnd(window) {
                    unsafe {
                        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                            hwnd, WM_CLOSE, 0, 0,
                        );
                    }
                }
            }
        }
    }
}

impl Drop for PlatformWindowChrome {
    fn drop(&mut self) {
        if let Some(hwnd) = self.hwnd.take() {
            unsafe {
                RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
            }
        }
    }
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    let state = &*(reference_data as *const SubclassState);
    if message == WM_NCCALCSIZE && wparam != 0 {
        let params = &mut *(lparam as *mut NCCALCSIZE_PARAMS);
        let original_top = params.rgrc[0].top;
        let _ = DefSubclassProc(hwnd, message, wparam, lparam);
        params.rgrc[0].top = original_top;
        return 0;
    }
    if message == WM_NCHITTEST {
        let mut dwm_result = 0;
        if DwmDefWindowProc(hwnd, message, wparam, lparam, &mut dwm_result) != 0
            && matches!(dwm_result as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE)
        {
            return dwm_result;
        }

        let mut point = POINT {
            x: low_word_signed(lparam),
            y: high_word_signed(lparam),
        };
        if ScreenToClient(hwnd, &mut point) == 0 {
            return DefSubclassProc(hwnd, message, wparam, lparam);
        }
        let Some(regions) = state.regions.lock().ok().and_then(|slot| *slot) else {
            return DefSubclassProc(hwnd, message, wparam, lparam);
        };
        let Some((resize_x, resize_y)) = state.resize_metrics.lock().ok().map(|metrics| *metrics)
        else {
            return DefSubclassProc(hwnd, message, wparam, lparam);
        };
        if let Some(resize) = resize_hit_for_window(
            IsZoomed(hwnd) != 0,
            point.x,
            point.y,
            regions.layout.title_bar.width as i32,
            regions.layout.content_rect.y as i32 + regions.layout.content_rect.height as i32,
            resize_x,
            resize_y,
        ) {
            return resize as LRESULT;
        }
        if point.x < 0 || point.y < 0 {
            return DefSubclassProc(hwnd, message, wparam, lparam);
        }
        return match regions.layout.hit_test(point.x as u32, point.y as u32) {
            ChromeHit::Drag => HTCAPTION as LRESULT,
            ChromeHit::Minimize => HTMINBUTTON as LRESULT,
            ChromeHit::Maximize => HTMAXBUTTON as LRESULT,
            ChromeHit::Close => HTCLOSE as LRESULT,
            ChromeHit::Connection
            | ChromeHit::Audio
            | ChromeHit::Clipboard
            | ChromeHit::SessionAction
            | ChromeHit::Client => HTCLIENT as LRESULT,
        };
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

fn low_word_signed(value: LPARAM) -> i32 {
    (value as u32 & 0xffff) as u16 as i16 as i32
}

fn high_word_signed(value: LPARAM) -> i32 {
    ((value as u32 >> 16) & 0xffff) as u16 as i16 as i32
}

fn resize_hit(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    border_x: i32,
    border_y: i32,
) -> Option<u32> {
    let left = x >= 0 && x < border_x;
    let right = x < width && x >= width - border_x;
    let top = y >= 0 && y < border_y;
    let bottom = y < height && y >= height - border_y;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(HTTOPLEFT),
        (_, true, true, _) => Some(HTTOPRIGHT),
        (true, _, _, true) => Some(HTBOTTOMLEFT),
        (_, true, _, true) => Some(HTBOTTOMRIGHT),
        (true, _, _, _) => Some(HTLEFT),
        (_, true, _, _) => Some(HTRIGHT),
        (_, _, true, _) => Some(HTTOP),
        (_, _, _, true) => Some(HTBOTTOM),
        _ => None,
    }
}

fn resize_hit_for_window(
    maximized: bool,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    border_x: i32,
    border_y: i32,
) -> Option<u32> {
    (!maximized).then(|| resize_hit(x, y, width, height, border_x, border_y))?
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::UI::WindowsAndMessaging::{HTBOTTOMRIGHT, HTCLIENT, HTTOPLEFT};

    use crate::{ChromeHitRegions, ChromeLayout, WindowChromeAdapter};

    use super::{resize_hit, resize_hit_for_window, PlatformWindowChrome};

    #[test]
    fn resize_edges_win_only_at_the_physical_frame_boundary() {
        assert_eq!(resize_hit(1, 1, 1200, 800, 8, 8), Some(HTTOPLEFT));
        assert_eq!(resize_hit(1199, 799, 1200, 800, 8, 8), Some(HTBOTTOMRIGHT));
        assert_eq!(resize_hit(600, 20, 1200, 800, 8, 8), None);
        assert_ne!(HTCLIENT, HTTOPLEFT);
    }

    #[test]
    fn maximized_windows_never_publish_resize_edges() {
        assert_eq!(resize_hit_for_window(true, 1, 1, 1200, 800, 8, 8), None);
        assert_eq!(
            resize_hit_for_window(false, 1, 1, 1200, 800, 8, 8),
            Some(HTTOPLEFT)
        );
    }

    #[test]
    fn clearing_published_hit_regions_removes_the_previous_layout() {
        let layout = ChromeLayout::for_window(1000, 700, 1.0, 0, 138).unwrap();
        let regions = ChromeHitRegions { layout };
        let mut chrome = PlatformWindowChrome::new();

        chrome.publish_hit_regions(Some(regions));
        assert_eq!(*chrome.state.regions.lock().unwrap(), Some(regions));

        chrome.publish_hit_regions(None);
        assert_eq!(*chrome.state.regions.lock().unwrap(), None);
    }
}
