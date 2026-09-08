//! 固定 GTK 4.14 旧 GSK GL/EGL 路径的窗口提交观察；不证明 scanout，不生成协议 ACK。
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionError {
    Unavailable,
    Lifecycle,
    Association,
    GlFault,
    EglFault,
}

/// 有界诊断：仅阶段计数与关联布尔，不暴露对象地址、像素或用户内容。
#[derive(Clone, Copy, Debug, Default)]
pub struct SubmissionDiagnostics {
    pub before_count: u64,
    pub paint_count: u64,
    pub draw_count: u64,
    pub after_count: u64,
    pub bootstrap_count: u64,
    pub initial_attach_gap_draw_count: u64,
    pub layout_gap_draw_count: u64,
    pub confirmed_count: u64,
    pub invalidation_count: u64,
    pub before_frame: i64,
    pub paint_frame: i64,
    pub draw_frame: i64,
    pub frame_counter: i64,
    pub epoch: u64,
    pub live: bool,
    pub baseline_clean: bool,
    pub nonempty_damage: bool,
    pub invocation_matches: bool,
    pub current_window: bool,
    pub same_known: bool,
    pub exact_draw: bool,
    pub observed_paint: bool,
    pub clean_after: bool,
    pub draw_surfaceless: bool,
    pub window_context_transition: bool,
    pub last_egl_error: u32,
}

// EGL 报告最近一次调用结果；查询 current 身份也可能覆盖错误。
// 强制先取错误，再允许身份查询，不能照搬 GL 的 sticky error 语义。
fn capture_egl_before_identity<T>(
    error: impl FnOnce() -> u32,
    identity: impl FnOnce() -> T,
) -> (u32, T) {
    let error = error();
    (error, identity())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservationGap {
    InitialAttach(i64),
    Layout(i64),
}
impl ObservationGap {
    fn frame(self) -> i64 {
        match self {
            Self::InitialAttach(frame) | Self::Layout(frame) => frame,
        }
    }
}

#[derive(Default)]
struct FrameGate {
    epoch: Rc<Cell<u64>>,
    exhausted: Rc<Cell<bool>>,
    frame: Option<(i64, u64, bool)>,
    gap: Option<ObservationGap>,
    paint: bool,
    draw: bool,
    rejected: bool,
}
impl FrameGate {
    fn initial_attach(frame: i64) -> Self {
        Self {
            gap: Some(ObservationGap::InitialAttach(frame)),
            ..Self::default()
        }
    }
    fn observes(&self, frame: i64) -> bool {
        self.frame.is_some_and(|f| f.0 == frame) || self.gap.is_some_and(|gap| gap.frame() == frame)
    }
    fn invalidate_layout(&mut self, frame: i64) {
        let same_frame = self.observes(frame);
        let rejected = same_frame && self.rejected;
        if !same_frame {
            self.paint = false;
            self.draw = false;
        }
        self.invalidate();
        self.gap = Some(ObservationGap::Layout(frame));
        self.rejected = rejected;
    }
    fn begin(&mut self, frame: i64, clean_baseline: bool) {
        self.gap = None;
        self.frame = Some((
            frame,
            self.epoch.get(),
            clean_baseline && !self.exhausted.get(),
        ));
        self.paint = false;
        self.draw = false;
        self.rejected = false;
    }
    fn paint(&mut self, frame: i64, region_present: bool) {
        if !self.observes(frame) || self.paint || !region_present {
            self.rejected = true;
        }
        self.paint = true;
    }
    fn draw(&mut self, frame: i64, invocation_matches: bool) -> bool {
        if !self.observes(frame) || !self.paint || self.draw || !invocation_matches {
            self.rejected = true;
        }
        self.draw = true;
        !self.rejected
    }
    fn finish(&mut self, frame: i64, clean_after: bool, window_context_transition: bool) -> bool {
        self.frame.take().is_some_and(|(f, epoch, baseline)| {
            f == frame
                && epoch == self.epoch.get()
                && baseline
                && self.paint
                && self.draw
                && !self.rejected
                && clean_after
                && window_context_transition
        })
    }
    fn invalidate(&mut self) {
        if let Some(epoch) = self.epoch.get().checked_add(1) {
            self.epoch.set(epoch);
        } else {
            self.exhausted.set(true);
        }
        self.frame = None;
        self.gap = None;
        self.rejected = true;
    }
}

#[cfg(all(target_os = "linux", feature = "gtk-shell"))]
mod native {
    use super::*;
    use frd_render_gl::DrawReceipt;
    use glib::translate::*;
    use gtk4::{gdk, glib, prelude::*, GLArea, Window};
    use std::{cell::RefCell, ffi::c_void};

    type EglContext = *mut c_void;
    type EglSurface = *mut c_void;
    type GetError = unsafe extern "C" fn() -> u32;
    type GetCurrent = unsafe extern "C" fn() -> *mut c_void;
    type GetSurface = unsafe extern "C" fn(u32) -> EglSurface;

    struct Api {
        _egl: libloading::Library,
        _epoxy: libloading::Library,
        egl_error: GetError,
        gl_error: GetError,
        context: GetCurrent,
        display: GetCurrent,
        surface: GetSurface,
    }
    impl Api {
        fn load() -> Result<Self, SubmissionError> {
            // EGL 导出是函数；epoxy_glGetError 则是公开函数指针变量，必须解引用两次。
            unsafe {
                let egl = libloading::Library::new("libEGL.so.1")
                    .map_err(|_| SubmissionError::Unavailable)?;
                let epoxy = libloading::Library::new("libepoxy.so.0")
                    .map_err(|_| SubmissionError::Unavailable)?;
                let egl_error = *egl
                    .get(b"eglGetError\0")
                    .map_err(|_| SubmissionError::Unavailable)?;
                let context = *egl
                    .get(b"eglGetCurrentContext\0")
                    .map_err(|_| SubmissionError::Unavailable)?;
                let display = *egl
                    .get(b"eglGetCurrentDisplay\0")
                    .map_err(|_| SubmissionError::Unavailable)?;
                let surface = *egl
                    .get(b"eglGetCurrentSurface\0")
                    .map_err(|_| SubmissionError::Unavailable)?;
                let slot = epoxy
                    .get::<*const GetError>(b"epoxy_glGetError\0")
                    .map_err(|_| SubmissionError::Unavailable)?;
                if (*slot).is_null() {
                    return Err(SubmissionError::Unavailable);
                }
                let gl_error = **slot;
                Ok(Self {
                    _egl: egl,
                    _epoxy: epoxy,
                    egl_error,
                    gl_error,
                    context,
                    display,
                    surface,
                })
            }
        }
        fn identity(&self) -> Option<(EglContext, *mut c_void, EglSurface)> {
            // 仅 GTK 主线程调用。没有 current EGL context 即 GLX/其他后端，不调用 GL。
            unsafe {
                let context = (self.context)();
                let display = (self.display)();
                let surface = (self.surface)(0x3059); // EGL_DRAW
                (!context.is_null() && !display.is_null()).then_some((context, display, surface))
            }
        }
        fn clean(&self, egl: u32) -> Result<(), SubmissionError> {
            // 一次 drain 记录所有本作用域可见错误；上界防止丢失 context 时无限循环。
            let mut gl_fault = false;
            for _ in 0..32 {
                let error = unsafe { (self.gl_error)() };
                if error == 0 {
                    return if egl != 0x3000 {
                        Err(SubmissionError::EglFault)
                    } else if gl_fault {
                        Err(SubmissionError::GlFault)
                    } else {
                        Ok(())
                    };
                }
                gl_fault = true;
            }
            Err(SubmissionError::GlFault)
        }
    }

    struct Known {
        context: gdk::GLContext,
        egl_context: EglContext,
        display: *mut c_void,
        drawable: EglSurface,
    }
    struct State {
        gate: FrameGate,
        known: Option<Known>,
        draw: Option<DrawReceipt>,
        draw_egl_context: Option<EglContext>,
        ready: Option<WindowSubmission>,
        error: Option<SubmissionError>,
        retry_after_layout: bool,
        diagnostics: SubmissionDiagnostics,
    }
    impl State {
        fn invalidate(&mut self) {
            self.diagnostics.invalidation_count =
                self.diagnostics.invalidation_count.saturating_add(1);
            self.gate.invalidate();
            self.known = None;
            self.draw = None;
            self.draw_egl_context = None;
            self.ready = None;
        }
    }

    /// 不能构造或克隆；消费时再核对生命周期和 renderer serial。
    pub struct WindowSubmission {
        draw: DrawReceipt,
        epoch: Rc<Cell<u64>>,
        observed_epoch: u64,
        exhausted: Rc<Cell<bool>>,
        surface: glib::WeakRef<gdk::Surface>,
        area: glib::WeakRef<GLArea>,
        frame_counter: i64,
    }
    impl WindowSubmission {
        pub fn frame_counter(&self) -> i64 {
            self.frame_counter
        }
        pub fn consume(self) -> Result<DrawReceipt, SubmissionError> {
            if self.epoch.get() != self.observed_epoch
                || self.exhausted.get()
                || !self.draw.is_valid()
                || self
                    .surface
                    .upgrade()
                    .is_none_or(|s| s.is_destroyed() || !s.is_mapped())
                || self.area.upgrade().is_none_or(|a| {
                    !a.is_realized()
                        || !a.is_mapped()
                        || a.error().is_some()
                        || a.native().and_then(|n| n.surface()) != self.surface.upgrade()
                })
            {
                return Err(SubmissionError::Lifecycle);
            }
            Ok(self.draw)
        }
    }

    /// 绑定一个已 realize 的 GtkWindow/GLArea；模块拥有信号连接与原生库。
    /// attach 仅接受 GTK 4.14 的 GskGLRenderer；GLX 暂时显式 Unavailable。
    pub struct WindowSubmissionObserver {
        state: RefCell<State>,
        area: glib::WeakRef<GLArea>,
        surface: gdk::Surface,
        clock: gdk::FrameClock,
        api: Api,
        render_signal: u32,
        hook: Cell<u64>,
        geometry: Cell<(i32, i32, i32)>,
        connections: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    }
    impl WindowSubmissionObserver {
        pub(crate) fn attach(area: &GLArea, window: &Window) -> Result<Rc<Self>, SubmissionError> {
            if gtk4::major_version() != 4
                || gtk4::minor_version() != 14
                || window
                    .renderer()
                    .is_none_or(|r| r.type_().name() != "GskGLRenderer")
                || !area.is_realized()
            {
                return Err(SubmissionError::Unavailable);
            }
            let surface = window.surface().ok_or(SubmissionError::Lifecycle)?;
            if area.native().and_then(|n| n.surface()).as_ref() != Some(&surface) {
                return Err(SubmissionError::Association);
            }
            let clock = surface.frame_clock();
            let render_signal = unsafe {
                glib::gobject_ffi::g_signal_lookup(
                    c"render".as_ptr(),
                    gdk::Surface::static_type().into_glib(),
                )
            };
            if render_signal == 0 {
                return Err(SubmissionError::Unavailable);
            }
            let this = Rc::new(Self {
                state: RefCell::new(State {
                    gate: FrameGate::initial_attach(clock.frame_counter()),
                    known: None,
                    draw: None,
                    draw_egl_context: None,
                    ready: None,
                    error: None,
                    retry_after_layout: true,
                    diagnostics: SubmissionDiagnostics::default(),
                }),
                area: area.downgrade(),
                surface,
                clock,
                api: Api::load()?,
                render_signal,
                hook: Cell::new(0),
                geometry: Cell::new((window.width(), window.height(), window.scale_factor())),
                connections: RefCell::new(Vec::new()),
            });
            let weak = Rc::downgrade(&this);
            let id = this.clock.connect_before_paint(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.before();
                }
            });
            this.connections
                .borrow_mut()
                .push((this.clock.clone().upcast(), id));
            let weak = Rc::downgrade(&this);
            let id = this.clock.connect_after_paint(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.after();
                }
            });
            this.connections
                .borrow_mut()
                .push((this.clock.clone().upcast(), id));
            let weak = Rc::downgrade(&this);
            let id = area.connect_resize(move |_, _, _| {
                if let Some(this) = weak.upgrade() {
                    this.invalidate_layout();
                }
            });
            this.connections
                .borrow_mut()
                .push((area.clone().upcast(), id));
            let weak = Rc::downgrade(&this);
            let id = this.surface.connect_layout(move |surface, width, height| {
                if let Some(this) = weak.upgrade() {
                    let geometry = (width, height, surface.scale_factor());
                    if this.geometry.replace(geometry) != geometry {
                        this.invalidate_layout();
                    }
                }
            });
            this.connections
                .borrow_mut()
                .push((this.surface.clone().upcast(), id));
            let weak = Rc::downgrade(&this);
            let id = this.surface.connect_mapped_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.invalidate();
                }
            });
            this.connections
                .borrow_mut()
                .push((this.surface.clone().upcast(), id));
            let weak = Rc::downgrade(&this);
            let id = this.surface.connect_scale_factor_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.invalidate_layout();
                }
            });
            this.connections
                .borrow_mut()
                .push((this.surface.clone().upcast(), id));
            let weak = Rc::downgrade(&this);
            let id = area.connect_unrealize(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.invalidate();
                }
            });
            this.connections
                .borrow_mut()
                .push((area.clone().upcast(), id));
            let weak = Rc::downgrade(&this);
            let id = area.connect_context_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.invalidate();
                }
            });
            this.connections
                .borrow_mut()
                .push((area.clone().upcast(), id));
            // emission hook 在 true-handled 累加器前关联真实 Surface::render。
            // expose region 非最终 GSK damage；空 region 仍可能由新节点 diff 产生绘制。
            // weak 数据由 GObject destroy notifier 释放，Drop 先移除 hook。
            let data = Box::into_raw(Box::new(Rc::downgrade(&this)));
            let hook = unsafe {
                glib::gobject_ffi::g_signal_add_emission_hook(
                    render_signal,
                    0,
                    Some(paint_hook),
                    data.cast(),
                    Some(drop_hook),
                )
            };
            if hook == 0 {
                unsafe {
                    drop(Box::from_raw(data));
                }
                return Err(SubmissionError::Unavailable);
            }
            this.hook.set(hook as u64);
            Ok(this)
        }
        pub fn invalidate(&self) {
            self.state.borrow_mut().invalidate();
        }
        fn invalidate_layout(&self) {
            let mut state = self.state.borrow_mut();
            // 明确的布局撤销保留同帧 signal 关联，但不补造 BEFORE_PAINT baseline。
            let frame = self.clock.frame_counter();
            state.gate.invalidate_layout(frame);
            state.diagnostics.invalidation_count =
                state.diagnostics.invalidation_count.saturating_add(1);
            state.known = None;
            state.draw = None;
            state.draw_egl_context = None;
            state.ready = None;
            state.retry_after_layout = true;
        }
        pub fn take_error(&self) -> Option<SubmissionError> {
            self.state.borrow_mut().error.take()
        }
        pub fn diagnostics(&self) -> SubmissionDiagnostics {
            self.state.borrow().diagnostics
        }
        pub fn take_submission(&self) -> Option<WindowSubmission> {
            self.state.borrow_mut().ready.take()
        }

        /// 必须在 GLArea::render 回调的成功 draw 后同步交出所有权，延迟事件不被接受。
        pub(crate) fn record_draw(&self, receipt: DrawReceipt) -> Result<(), SubmissionError> {
            // 新 draw 的 serial 已使旧证明失效；无论随后关联成功与否都清除旧槽。
            self.state.borrow_mut().ready = None;
            let area = self.area.upgrade().ok_or(SubmissionError::Lifecycle)?;
            let draw_identity = self.api.identity();
            let draw_surfaceless = draw_identity.is_some_and(|id| id.2.is_null());
            let matches = draw_surfaceless
                && self.in_signal(&self.surface, self.render_signal)
                && self.in_signal(&area, unsafe {
                    glib::gobject_ffi::g_signal_lookup(
                        c"render".as_ptr(),
                        GLArea::static_type().into_glib(),
                    )
                })
                && area.native().and_then(|n| n.surface()).as_ref() == Some(&self.surface)
                && gdk::GLContext::current().is_some()
                && gdk::GLContext::current() == area.context()
                && receipt.is_current();
            let mut state = self.state.borrow_mut();
            state.diagnostics.draw_count = state.diagnostics.draw_count.saturating_add(1);
            state.diagnostics.draw_frame = self.clock.frame_counter();
            state.diagnostics.invocation_matches = matches;
            state.diagnostics.draw_surfaceless = draw_surfaceless;
            if !state.gate.draw(self.clock.frame_counter(), matches) {
                state.draw = None;
                state.error = Some(SubmissionError::Association);
                return Err(SubmissionError::Association);
            }
            match state.gate.gap {
                Some(ObservationGap::InitialAttach(_)) => {
                    state.diagnostics.initial_attach_gap_draw_count = state
                        .diagnostics
                        .initial_attach_gap_draw_count
                        .saturating_add(1);
                }
                Some(ObservationGap::Layout(_)) => {
                    state.diagnostics.layout_gap_draw_count =
                        state.diagnostics.layout_gap_draw_count.saturating_add(1);
                }
                None => {}
            }
            state.draw_egl_context = draw_identity.map(|id| id.0);
            state.draw = Some(receipt);
            Ok(())
        }
        fn in_signal(&self, object: &impl IsA<glib::Object>, signal: u32) -> bool {
            unsafe {
                let hint = glib::gobject_ffi::g_signal_get_invocation_hint(
                    object.as_ref().as_ptr().cast(),
                );
                !hint.is_null() && (*hint).signal_id == signal
            }
        }
        fn live(&self) -> bool {
            !self.surface.is_destroyed()
                && self.surface.is_mapped()
                && self.surface.frame_clock() == self.clock
                && self.area.upgrade().is_some_and(|a| {
                    a.is_realized()
                        && a.is_mapped()
                        && a.error().is_none()
                        && a.native().and_then(|n| n.surface()).as_ref() == Some(&self.surface)
                })
        }
        fn before(&self) {
            let egl_before = unsafe { (self.api.egl_error)() };
            let mut state = self.state.borrow_mut();
            state.diagnostics.before_count = state.diagnostics.before_count.saturating_add(1);
            state.diagnostics.before_frame = self.clock.frame_counter();
            state.diagnostics.frame_counter = self.clock.frame_counter();
            state.diagnostics.epoch = state.gate.epoch.get();
            state.diagnostics.live = self.live();
            // GTK 4.14 BEFORE_PAINT 早于 UPDATE/tick；已签发 ready 必须活到消费。
            // 仅新 draw 或真实 invalidate 撤销它，不在开启下一帧时删除。
            state.draw = None;
            state.draw_egl_context = None;
            if !self.live() {
                state.invalidate();
                return;
            }
            if egl_before != 0x3000 {
                state.error = Some(SubmissionError::EglFault);
                state.invalidate();
            }
            let baseline = state.known.as_ref().map(|known| {
                // 帧外 surfaceless baseline；GTK begin_frame 将重新绑定窗口 drawable。
                known.context.make_current();
                let (egl_make_current, matches) = capture_egl_before_identity(
                    || unsafe { (self.api.egl_error)() },
                    || {
                        gdk::GLContext::current().as_ref() == Some(&known.context)
                            && self.api.identity().is_some_and(|id| {
                                id.0 == known.egl_context && id.1 == known.display
                            })
                    },
                );
                if matches {
                    self.api.clean(egl_make_current)
                } else {
                    Err(SubmissionError::Association)
                }
            });
            if let Some(Err(error)) = baseline {
                state.error = Some(error);
                state.invalidate();
            }
            state
                .gate
                .begin(self.clock.frame_counter(), baseline == Some(Ok(())));
            state.diagnostics.baseline_clean = baseline == Some(Ok(()));
        }
        fn after(&self) {
            // 必须是这里最先发生的 EGL 调用；GdkGLContext::current 内部也会查询 EGL。
            let (egl_after, (current, identity)) = capture_egl_before_identity(
                || unsafe { (self.api.egl_error)() },
                || (gdk::GLContext::current(), self.api.identity()),
            );
            let mut state = self.state.borrow_mut();
            state.diagnostics.last_egl_error = egl_after;
            state.diagnostics.after_count = state.diagnostics.after_count.saturating_add(1);
            state.diagnostics.frame_counter = self.clock.frame_counter();
            state.diagnostics.epoch = state.gate.epoch.get();
            state.diagnostics.live = self.live();
            if !self.live() {
                state.invalidate();
                return;
            }
            // 不调用 make_current；只检查 GSK window end_frame 留下的实际 current。
            // 纯 UPDATE/tick 也有 AFTER_PAINT；帧前的 surfaceless baseline
            // 不是提交失败。有 Surface paint 却无事务 receipt 时仍检查 GL/EGL，
            // 包括已消费帧的合法重绘和 UI-only paint，不签发新的远程证明。
            if !state.gate.draw {
                let no_receipt_fault = if !state.gate.paint {
                    (egl_after != 0x3000).then_some(SubmissionError::EglFault)
                } else if current
                    .as_ref()
                    .is_some_and(|c| DrawContextExt::surface(c).as_ref() == Some(&self.surface))
                    && identity.is_some()
                {
                    // 空最终clip可能留下本surface的surfaceless context，不要求drawable。
                    self.api.clean(egl_after).err()
                } else {
                    Some(if egl_after != 0x3000 {
                        SubmissionError::EglFault
                    } else {
                        SubmissionError::Association
                    })
                };
                let _ = state.gate.finish(self.clock.frame_counter(), false, false);
                if let Some(error) = no_receipt_fault {
                    state.error = Some(error);
                    state.invalidate();
                }
                return;
            }
            let valid = current.as_ref().is_some_and(|c| {
                DrawContextExt::surface(c).as_ref() == Some(&self.surface)
                    && !c.is_in_frame()
                    && self.area.upgrade().and_then(|a| a.context()).as_ref() != Some(c)
            }) && identity.is_some_and(|id| !id.2.is_null());
            let clean = if valid {
                self.api.clean(egl_after)
            } else {
                Err(SubmissionError::Unavailable)
            };
            let same = match (&state.known, &current, identity) {
                (Some(k), Some(c), Some(id)) => {
                    k.context == *c && (k.egl_context, k.display, k.drawable) == id
                }
                _ => false,
            };
            let exact_draw = state.draw.as_ref().is_some_and(DrawReceipt::is_valid);
            let observed_paint = state.gate.paint && state.gate.draw && !state.gate.rejected;
            // Surface expose 可为空，GSK 随后根据新 GLTexture 节点计算真正 damage。
            // 固定旧 gl 的 empty_frame 在 make_current 前返回，不能把本次 GLArea 的
            // surfaceless context 转换为这里的窗口 context/drawable。以实际转换证明
            // 经过非空渲染分支，不能把空 expose 或 after-paint 本身当成提交证据。
            let window_context_transition = valid
                && identity.is_some_and(|id| {
                    state
                        .draw_egl_context
                        .is_some_and(|draw_context| draw_context != id.0)
                });
            state.diagnostics.window_context_transition = window_context_transition;
            state.diagnostics.current_window = valid;
            state.diagnostics.same_known = same;
            state.diagnostics.exact_draw = exact_draw;
            state.diagnostics.observed_paint = observed_paint;
            state.diagnostics.clean_after = clean.is_ok();
            let observation_gap = state.gate.gap.is_some();
            let confirmed = state.gate.finish(
                self.clock.frame_counter(),
                same && clean.is_ok() && exact_draw,
                window_context_transition,
            );
            if confirmed {
                state.diagnostics.confirmed_count =
                    state.diagnostics.confirmed_count.saturating_add(1);
                if let Some(draw) = state.draw.take() {
                    state.ready = Some(WindowSubmission {
                        draw,
                        epoch: state.gate.epoch.clone(),
                        observed_epoch: state.gate.epoch.get(),
                        exhausted: state.gate.exhausted.clone(),
                        surface: self.surface.downgrade(),
                        area: self.area.clone(),
                        frame_counter: self.clock.frame_counter(),
                    });
                }
            } else {
                state.draw = None;
                if let Err(error) = clean {
                    state.error = Some(error);
                    state.invalidate();
                } else if !observation_gap
                    && observed_paint
                    && exact_draw
                    && window_context_transition
                    && !same
                {
                    state.diagnostics.bootstrap_count =
                        state.diagnostics.bootstrap_count.saturating_add(1);
                    state.invalidate();
                    if let (Some(context), Some((egl_context, display, drawable))) =
                        (current, identity)
                    {
                        state.known = Some(Known {
                            context,
                            egl_context,
                            display,
                            drawable,
                        });
                        // bootstrap 只观察，要求下一次真正的 GLArea render 才能确认。
                        if let Some(area) = self.area.upgrade() {
                            area.queue_render();
                        }
                    }
                }
            }
            // layout/resize 可发生在 before-paint 与 snapshot 之间；本帧不确认，补一次重绘。
            if state.retry_after_layout && valid {
                state.retry_after_layout = false;
                if let Some(area) = self.area.upgrade() {
                    area.queue_render();
                }
            }
        }
    }
    impl Drop for WindowSubmissionObserver {
        fn drop(&mut self) {
            self.state.get_mut().invalidate();
            if self.hook.get() != 0 {
                unsafe {
                    glib::gobject_ffi::g_signal_remove_emission_hook(
                        self.render_signal,
                        self.hook.get() as _,
                    );
                }
            }
            for (object, id) in self.connections.get_mut().drain(..) {
                object.disconnect(id);
            }
        }
    }
    unsafe extern "C" fn drop_hook(data: glib::ffi::gpointer) {
        drop(Box::from_raw(
            data.cast::<std::rc::Weak<WindowSubmissionObserver>>(),
        ));
    }
    unsafe extern "C" fn paint_hook(
        _: *mut glib::gobject_ffi::GSignalInvocationHint,
        n: u32,
        values: *const glib::gobject_ffi::GValue,
        data: glib::ffi::gpointer,
    ) -> glib::ffi::gboolean {
        let weak = &*data.cast::<std::rc::Weak<WindowSubmissionObserver>>();
        if let Some(this) = weak.upgrade() {
            if n >= 2
                && glib::gobject_ffi::g_value_get_object(values) == this.surface.as_ptr().cast()
            {
                let region = glib::gobject_ffi::g_value_get_boxed(values.add(1))
                    .cast::<gtk4::cairo::ffi::cairo_region_t>();
                let nonempty =
                    !region.is_null() && !gtk4::cairo::ffi::cairo_region_is_empty(region).as_bool();
                if let Ok(mut state) = this.state.try_borrow_mut() {
                    state.diagnostics.paint_count = state.diagnostics.paint_count.saturating_add(1);
                    state.diagnostics.paint_frame = this.clock.frame_counter();
                    state.diagnostics.nonempty_damage = nonempty;
                    state
                        .gate
                        .paint(this.clock.frame_counter(), !region.is_null());
                }
            }
        }
        1
    }
}

#[cfg(all(target_os = "linux", feature = "gtk-shell"))]
pub use native::{WindowSubmission, WindowSubmissionObserver};

#[cfg(test)]
#[path = "../tests/support/presentation_state_cases.rs"]
mod tests;
