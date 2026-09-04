use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

use frd_ui_model::{IslandAction, IslandWindowCapabilities};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmDefWindowProc, DwmExtendFrameIntoClientArea};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, IsZoomed, SetWindowPos, ShowWindow, SystemParametersInfoW, HTBOTTOM,
    HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTMAXBUTTON, HTRIGHT, HTTOP,
    HTTOPLEFT, HTTOPRIGHT, NCCALCSIZE_PARAMS, SM_CXPADDEDBORDER, SM_CXSIZEFRAME, SM_CYSIZEFRAME,
    SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SW_MAXIMIZE, SW_RESTORE, WM_CLOSE, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE,
    WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_SETTINGCHANGE,
    WM_THEMECHANGED,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{
    AppearancePolicy, ChromeHitMap, ChromeHitTarget, NativeChromeInsets, WindowChromeAdapter,
    WindowChromeCommand, WindowChromeError, TITLE_BAR_HEIGHT_POINTS,
};

const SUBCLASS_ID: usize = 0x4652_4443;

struct SubclassState {
    hit_map: Mutex<Option<ChromeHitMap>>,
    resize_metrics: Mutex<(i32, i32)>,
    native_interaction: AtomicBool,
    appearance_dirty: AtomicBool,
    maximize_press_armed: AtomicBool,
}

pub(crate) struct PlatformWindowChrome {
    hwnd: Option<HWND>,
    state: Box<SubclassState>,
    appearance_policy: AppearancePolicy,
}

impl PlatformWindowChrome {
    pub(crate) fn new() -> Self {
        Self {
            hwnd: None,
            state: Box::new(SubclassState {
                hit_map: Mutex::new(None),
                resize_metrics: Mutex::new((8, 8)),
                native_interaction: AtomicBool::new(false),
                appearance_dirty: AtomicBool::new(false),
                maximize_press_armed: AtomicBool::new(false),
            }),
            appearance_policy: AppearancePolicy::conservative(),
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
        self.appearance_policy = AppearancePolicy::from_probe(query_appearance_preferences());
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

    fn native_insets(&self, _window: &winit::window::Window) -> NativeChromeInsets {
        windows_island_native_insets()
    }

    fn capabilities(&self) -> IslandWindowCapabilities {
        IslandWindowCapabilities::WINDOWS
    }

    fn appearance_policy(&self) -> AppearancePolicy {
        self.appearance_policy
    }

    fn refresh_appearance_policy(&mut self) -> bool {
        if !self.state.appearance_dirty.swap(false, Ordering::AcqRel) {
            return false;
        }
        let next = AppearancePolicy::from_probe(query_appearance_preferences());
        if next == self.appearance_policy {
            return false;
        }
        self.appearance_policy = next;
        true
    }

    fn native_interaction_active(&self) -> bool {
        self.state.native_interaction.load(Ordering::Acquire)
    }

    fn publish_hit_map(&mut self, hit_map: ChromeHitMap) {
        if let Ok(mut slot) = self.state.hit_map.lock() {
            *slot = Some(hit_map);
        }
    }

    fn execute(
        &mut self,
        window: &winit::window::Window,
        command: WindowChromeCommand,
    ) -> Result<(), WindowChromeError> {
        match command {
            WindowChromeCommand::BeginMove => window
                .drag_window()
                .map_err(|_| WindowChromeError::PlatformCallFailed),
            WindowChromeCommand::Minimize => {
                window.set_minimized(true);
                Ok(())
            }
            WindowChromeCommand::ToggleMaximize => {
                window.set_maximized(!window.is_maximized());
                Ok(())
            }
            WindowChromeCommand::Close => {
                let hwnd = Self::hwnd(window)?;
                (unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd, WM_CLOSE, 0, 0)
                } != 0)
                    .then_some(())
                    .ok_or(WindowChromeError::PlatformCallFailed)
            }
            WindowChromeCommand::ShowSystemMenu => {
                window.show_window_menu(winit::dpi::PhysicalPosition::new(0_i32, 0_i32));
                Ok(())
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
    observe_native_interaction_message(&state.native_interaction, message);
    observe_appearance_message(&state.appearance_dirty, message);
    if matches!(message, WM_NCLBUTTONDOWN | WM_NCLBUTTONUP) {
        let was_armed = if message == WM_NCLBUTTONUP {
            state.maximize_press_armed.swap(false, Ordering::AcqRel)
        } else {
            state.maximize_press_armed.store(false, Ordering::Release);
            false
        };
        let hit = chrome_hit_target_at_screen_point(hwnd, state, lparam);
        match native_maximize_click_decision(message, wparam, hit, was_armed) {
            NativeMaximizeClickDecision::Pass => {}
            NativeMaximizeClickDecision::Arm => {
                state.maximize_press_armed.store(true, Ordering::Release);
                return 0;
            }
            NativeMaximizeClickDecision::Toggle => {
                ShowWindow(
                    hwnd,
                    if IsZoomed(hwnd) != 0 {
                        SW_RESTORE
                    } else {
                        SW_MAXIMIZE
                    },
                );
                return 0;
            }
            NativeMaximizeClickDecision::Consume => return 0,
        }
    }
    if message == WM_NCCALCSIZE && wparam != 0 {
        let params = &mut *(lparam as *mut NCCALCSIZE_PARAMS);
        let original_top = params.rgrc[0].top;
        let _ = DefSubclassProc(hwnd, message, wparam, lparam);
        params.rgrc[0].top = original_top;
        return 0;
    }
    if message == WM_NCHITTEST {
        let mut dwm_result = 0;
        let dwm_resize_hit = (DwmDefWindowProc(hwnd, message, wparam, lparam, &mut dwm_result)
            != 0)
            .then(|| accepted_dwm_resize_hit(dwm_result))
            .flatten();

        let mut point = POINT {
            x: low_word_signed(lparam),
            y: high_word_signed(lparam),
        };
        if ScreenToClient(hwnd, &mut point) == 0 {
            return dwm_resize_hit.unwrap_or(HTCLIENT as LRESULT);
        };
        let Some((resize_x, resize_y)) = state.resize_metrics.lock().ok().map(|metrics| *metrics)
        else {
            return dwm_resize_hit.unwrap_or(HTCLIENT as LRESULT);
        };
        let mut client_rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetClientRect(hwnd, &mut client_rect) == 0 {
            return dwm_resize_hit.unwrap_or(HTCLIENT as LRESULT);
        }
        if let Some(resize) = resize_hit_for_window(
            IsZoomed(hwnd) != 0,
            point.x,
            point.y,
            client_rect.right.saturating_sub(client_rect.left),
            client_rect.bottom.saturating_sub(client_rect.top),
            resize_x,
            resize_y,
        ) {
            return resize as LRESULT;
        }
        if let Some(resize) = dwm_resize_hit {
            return resize;
        }
        if point.x < 0 || point.y < 0 {
            return HTCLIENT as LRESULT;
        }
        let Some(hit_map) = state.hit_map.lock().ok().and_then(|slot| slot.clone()) else {
            return HTCLIENT as LRESULT;
        };
        return windows_native_hit(&hit_map, (point.x as u32, point.y as u32)) as LRESULT;
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeMaximizeClickDecision {
    Pass,
    Arm,
    Toggle,
    Consume,
}

fn native_maximize_click_decision(
    message: u32,
    wparam: WPARAM,
    hit: Option<ChromeHitTarget>,
    was_armed: bool,
) -> NativeMaximizeClickDecision {
    let reported_maximize = wparam == HTMAXBUTTON as WPARAM;
    let exact_maximize = matches!(
        hit,
        Some(ChromeHitTarget::IslandAction(
            IslandAction::ToggleMaximizeWindow
        ))
    );
    match message {
        WM_NCLBUTTONDOWN if reported_maximize && exact_maximize => NativeMaximizeClickDecision::Arm,
        WM_NCLBUTTONDOWN if reported_maximize => NativeMaximizeClickDecision::Consume,
        WM_NCLBUTTONUP if was_armed && reported_maximize && exact_maximize => {
            NativeMaximizeClickDecision::Toggle
        }
        WM_NCLBUTTONUP if was_armed || reported_maximize => NativeMaximizeClickDecision::Consume,
        _ => NativeMaximizeClickDecision::Pass,
    }
}

unsafe fn chrome_hit_target_at_screen_point(
    hwnd: HWND,
    state: &SubclassState,
    lparam: LPARAM,
) -> Option<ChromeHitTarget> {
    let mut point = POINT {
        x: low_word_signed(lparam),
        y: high_word_signed(lparam),
    };
    if ScreenToClient(hwnd, &mut point) == 0 || point.x < 0 || point.y < 0 {
        return None;
    }
    state
        .hit_map
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref()?.hit_test((point.x as u32, point.y as u32)))
}

fn observe_native_interaction_message(active: &AtomicBool, message: u32) {
    match message {
        WM_ENTERSIZEMOVE => active.store(true, Ordering::Release),
        WM_EXITSIZEMOVE => active.store(false, Ordering::Release),
        _ => {}
    }
}

fn observe_appearance_message(dirty: &AtomicBool, message: u32) {
    if matches!(message, WM_SETTINGCHANGE | WM_THEMECHANGED) {
        dirty.store(true, Ordering::Release);
    }
}

fn query_appearance_preferences() -> Option<(bool, bool)> {
    let mut high_contrast = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        dwFlags: 0,
        lpszDefaultScheme: std::ptr::null_mut(),
    };
    if unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            high_contrast.cbSize,
            (&mut high_contrast as *mut HIGHCONTRASTW).cast(),
            0,
        )
    } == 0
    {
        return None;
    }

    let mut client_area_animation = 0_i32;
    if unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut client_area_animation as *mut i32).cast(),
            0,
        )
    } == 0
    {
        return None;
    }

    Some((
        high_contrast.dwFlags & HCF_HIGHCONTRASTON != 0,
        client_area_animation != 0,
    ))
}

fn accepted_dwm_resize_hit(result: LRESULT) -> Option<LRESULT> {
    matches!(
        result as u32,
        HTLEFT | HTRIGHT | HTTOP | HTTOPLEFT | HTTOPRIGHT | HTBOTTOM | HTBOTTOMLEFT | HTBOTTOMRIGHT
    )
    .then_some(result)
}

fn windows_island_native_insets() -> NativeChromeInsets {
    NativeChromeInsets::default()
}

fn windows_native_hit(hit_map: &ChromeHitMap, point: (u32, u32)) -> u32 {
    match hit_map.hit_test(point) {
        Some(ChromeHitTarget::IslandAction(IslandAction::ToggleMaximizeWindow)) => HTMAXBUTTON,
        Some(ChromeHitTarget::WindowMoveRegion) => HTCAPTION,
        Some(
            ChromeHitTarget::IslandAction(_)
            | ChromeHitTarget::IslandRepositionHandle
            | ChromeHitTarget::IslandSurface
            | ChromeHitTarget::NativeChrome
            | ChromeHitTarget::RemoteContent,
        )
        | None => HTCLIENT,
    }
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use frd_core::PixelRect;
    use frd_ui_model::IslandAction;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON, HTTOPLEFT,
        WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NULL,
        WM_SETTINGCHANGE, WM_THEMECHANGED,
    };

    use super::{
        accepted_dwm_resize_hit, native_maximize_click_decision, observe_appearance_message,
        observe_native_interaction_message, resize_hit, resize_hit_for_window,
        windows_island_native_insets, windows_native_hit, NativeMaximizeClickDecision,
    };
    use crate::{
        ChromeGeometrySnapshot, ChromeHitMap, ChromeHitTarget, ChromeRect, ControlIslandPlacement,
        NativeChromeInsets,
    };

    fn local_page_layout_with_close() -> crate::ChromeLayouts {
        ChromeGeometrySnapshot::new(1200, 800, 1.5, NativeChromeInsets::default())
            .unwrap()
            .with_window_capabilities(frd_ui_model::IslandWindowCapabilities::WINDOWS)
            .local_page_layouts(ControlIslandPlacement::default())
            .expect("valid Windows local-page chrome")
    }

    fn local_page_hit_map_with_close() -> ChromeHitMap {
        local_page_layout_with_close().hit_map
    }

    fn local_close_center() -> (u32, u32) {
        local_page_layout_with_close()
            .hit_map
            .action_rect(IslandAction::CloseWindow)
            .expect("close capability")
            .center()
    }

    fn local_move_center() -> (u32, u32) {
        local_page_layout_with_close()
            .overlay
            .window_move_region
            .expect("begin-move capability")
            .center()
    }

    #[test]
    fn windows_native_move_messages_pin_until_the_modal_interaction_exits() {
        let active = AtomicBool::new(false);

        observe_native_interaction_message(&active, WM_ENTERSIZEMOVE);
        assert!(active.load(Ordering::Acquire));
        observe_native_interaction_message(&active, WM_NULL);
        assert!(active.load(Ordering::Acquire));
        observe_native_interaction_message(&active, WM_EXITSIZEMOVE);
        assert!(!active.load(Ordering::Acquire));
    }

    #[test]
    fn native_maximize_click_requires_an_armed_exact_button_up() {
        let maximize = Some(ChromeHitTarget::IslandAction(
            IslandAction::ToggleMaximizeWindow,
        ));
        assert_eq!(
            native_maximize_click_decision(WM_NCLBUTTONDOWN, HTMAXBUTTON as usize, maximize, false,),
            NativeMaximizeClickDecision::Arm
        );
        assert_eq!(
            native_maximize_click_decision(WM_NCLBUTTONUP, HTMAXBUTTON as usize, maximize, true,),
            NativeMaximizeClickDecision::Toggle
        );
        assert_eq!(
            native_maximize_click_decision(WM_NCLBUTTONUP, HTMAXBUTTON as usize, maximize, false,),
            NativeMaximizeClickDecision::Consume
        );
        assert_eq!(
            native_maximize_click_decision(WM_NCLBUTTONUP, HTCLIENT as usize, maximize, true,),
            NativeMaximizeClickDecision::Consume
        );
        assert_eq!(
            native_maximize_click_decision(
                WM_NCLBUTTONUP,
                HTMAXBUTTON as usize,
                Some(ChromeHitTarget::RemoteContent),
                true,
            ),
            NativeMaximizeClickDecision::Consume
        );
    }

    #[test]
    fn windows_setting_and_theme_messages_mark_appearance_preferences_dirty() {
        for message in [WM_SETTINGCHANGE, WM_THEMECHANGED] {
            let dirty = AtomicBool::new(false);
            observe_appearance_message(&dirty, message);
            assert!(dirty.load(Ordering::Acquire));
        }

        let dirty = AtomicBool::new(false);
        observe_appearance_message(&dirty, WM_NULL);
        assert!(!dirty.load(Ordering::Acquire));
    }

    #[test]
    fn windows_maximize_rect_preserves_native_snap_hit() {
        let maximize_rect = ChromeRect {
            x: 500,
            y: 8,
            width: 44,
            height: 44,
        };
        let map = ChromeHitMap::candidate(
            PixelRect {
                x: 0,
                y: 0,
                width: 1200,
                height: 800,
            },
            vec![(maximize_rect, IslandAction::ToggleMaximizeWindow)],
            None,
            None,
            Vec::new(),
        )
        .unwrap();

        assert_eq!(
            map.hit_test(maximize_rect.center()),
            Some(ChromeHitTarget::IslandAction(
                IslandAction::ToggleMaximizeWindow
            ))
        );
        assert_eq!(
            windows_native_hit(&map, maximize_rect.center()),
            HTMAXBUTTON
        );
    }

    #[test]
    fn page_move_region_is_native_caption_after_resize_edges_take_priority() {
        let move_region = ChromeRect {
            x: 8,
            y: 8,
            width: 1184,
            height: 36,
        };
        let map = ChromeHitMap::candidate(
            PixelRect {
                x: 0,
                y: 0,
                width: 1200,
                height: 800,
            },
            Vec::new(),
            None,
            Some(move_region),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(windows_native_hit(&map, move_region.center()), HTCAPTION);
        assert_eq!(resize_hit(1, 1, 1200, 800, 8, 8), Some(HTTOPLEFT));
    }

    #[test]
    fn local_close_is_client_input_and_neighboring_bar_is_caption() {
        let map = local_page_hit_map_with_close();

        assert_eq!(windows_native_hit(&map, local_close_center()), HTCLIENT);
        assert_eq!(windows_native_hit(&map, local_move_center()), HTCAPTION);
    }

    #[test]
    fn legacy_dwm_caption_hits_are_rejected_without_discarding_resize_hits() {
        for legacy_caption in [HTMINBUTTON, HTMAXBUTTON, HTCLOSE] {
            assert_eq!(accepted_dwm_resize_hit(legacy_caption as isize), None);
        }
        assert_eq!(
            accepted_dwm_resize_hit(HTTOPLEFT as isize),
            Some(HTTOPLEFT as isize)
        );
    }

    #[test]
    fn windows_island_native_insets_do_not_reserve_legacy_caption_buttons() {
        assert_eq!(
            windows_island_native_insets(),
            crate::NativeChromeInsets::default()
        );
    }

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
}
