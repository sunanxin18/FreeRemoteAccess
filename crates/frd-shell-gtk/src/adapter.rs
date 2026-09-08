use crate::presentation::{SubmissionError, WindowSubmission, WindowSubmissionObserver};
use crate::{drawable_size, PendingBatch, RejectedBatch, SubmitError};
use frd_core::{ContentViewport, PixelSize};
use frd_frame::FrameTransaction;
use frd_render_gl::{
    ConfirmedGlPresentation, DrawReceipt, ExternalContext, GlBatchFailure, GlError,
    GlOutputContract, GlRenderTarget, RemoteGlRenderer,
};
use gtk4::{gdk, glib, prelude::*, GLArea};
use std::{cell::RefCell, ffi::c_void, rc::Rc};

#[derive(Debug)]
pub enum AdapterError {
    GtkContext,
    Loader,
    InvalidAllocation,
    Gl(GlError),
    Batch(GlBatchFailure),
    Submission(SubmissionError),
}
/// Drawn 只证明本次 GL 命令提交；既不证明 GTK snapshot 呈现，也不触发协议 ACK。
pub enum AdapterEvent {
    Drawn {
        receipt: DrawReceipt,
        viewport: ContentViewport,
    },
    Failed(AdapterError),
    Invalidated {
        discarded_transactions: usize,
    },
}

struct Active {
    context: ExternalContext,
    renderer: RemoteGlRenderer,
    remote: Option<PixelSize>,
}
impl Drop for Active {
    fn drop(&mut self) {
        // 异常释放也先使所有旧回执失效；Drop 永不调用 GL。
        self.context.mark_lost();
    }
}
#[derive(Default)]
struct State {
    pending: PendingBatch,
    active: Option<Active>,
    events: Vec<AdapterEvent>,
    faulted: bool,
    submission: Option<Rc<WindowSubmissionObserver>>,
}
impl State {
    fn event(&mut self, event: AdapterEvent) {
        // 事件是当前状态快照，不是 ACK 队列；重复同类事件合并，有界为三项。
        self.events
            .retain(|old| std::mem::discriminant(old) != std::mem::discriminant(&event));
        self.events.push(event);
    }
    fn invalidate(&mut self) {
        if let Some(observer) = &self.submission {
            observer.invalidate();
        }
        self.events
            .retain(|event| !matches!(event, AdapterEvent::Drawn { .. }));
        let discarded_transactions = self.pending.invalidate();
        self.event(AdapterEvent::Invalidated {
            discarded_transactions,
        });
    }
    fn retire(&mut self) {
        if let Some(observer) = self.submission.take() {
            observer.invalidate();
        }
        self.invalidate();
        if let Some(mut active) = self.active.take() {
            // detach 即使不 current 也撤销证明；此时不删除 GL 名称。
            let result = active.renderer.detach();
            active.context.mark_lost();
            if let Err(error) = result {
                self.event(AdapterEvent::Failed(AdapterError::Gl(error)));
            }
        }
    }
}

/// 仅 GTK 主线程使用。widget 必须保留在正常 GTK realize/unrealize 生命周期内。
/// 帧批次来自协议无关编译器；一次至多等待一个完整批次，Busy 原样返回所有权。
pub struct GtkFrameArea {
    area: GLArea,
    state: Rc<RefCell<State>>,
}
impl GtkFrameArea {
    pub fn new() -> Self {
        let area = GLArea::new();
        area.set_allowed_apis(gdk::GLAPI::GL);
        area.set_required_version(3, 3);
        area.set_has_depth_buffer(false);
        area.set_has_stencil_buffer(false);
        area.set_auto_render(false);
        area.set_hexpand(true);
        area.set_vexpand(true);
        let state = Rc::new(RefCell::new(State::default()));
        let weak = Rc::downgrade(&state);
        area.connect_realize(move |area| {
            if area.error().is_some() {
                if let Some(state) = weak.upgrade() {
                    let mut state = state.borrow_mut();
                    state.faulted = true;
                    state.invalidate();
                    state.event(AdapterEvent::Failed(AdapterError::GtkContext));
                }
            }
        });
        let weak = Rc::downgrade(&state);
        area.connect_render(move |area, context| {
            if let Some(state) = weak.upgrade() {
                let mut state = state.borrow_mut();
                if !state.faulted && !state.pending.closed {
                    if let Err(error) = render(area, context, &mut state) {
                        state.retire();
                        state.faulted = true;
                        state.event(AdapterEvent::Failed(error));
                    }
                }
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(&state);
        // RUN_LAST 信号：普通处理器先于 GtkGLArea 类处理器释放其 FBO/context。
        area.connect_unrealize(move |area| {
            if let Some(state) = weak.upgrade() {
                area.make_current();
                let mut state = state.borrow_mut();
                state.retire();
                state.faulted = false;
            }
        });
        Self { area, state }
    }
    pub fn widget(&self) -> &GLArea {
        &self.area
    }
    pub fn submit_batch(&self, transactions: Vec<FrameTransaction>) -> Result<(), RejectedBatch> {
        let mut state = self.state.borrow_mut();
        if !state.pending.closed && self.area.is_realized() && self.area.error().is_some() {
            return Err(RejectedBatch {
                reason: SubmitError::ContextUnavailable,
                transactions,
            });
        }
        state.pending.push(transactions)?;
        // 失败后只允许新的完整 startup（invalidate 已置位）重新建立执行器。
        state.faulted = false;
        drop(state);
        self.area.queue_render();
        Ok(())
    }
    pub fn drain_events(&self) -> Vec<AdapterEvent> {
        std::mem::take(&mut self.state.borrow_mut().events)
    }
    /// 仅已 realize 且属于该窗口的区域可启用；卸载/重建后必须重新启用。
    /// 开启后 DrawReceipt 同步交给窗口 observer，不再同时发出 Drawn 事件。
    pub fn enable_window_submission(&self, window: &gtk4::Window) -> Result<(), SubmissionError> {
        if self.state.borrow().pending.closed {
            return Err(SubmissionError::Lifecycle);
        }
        let observer = WindowSubmissionObserver::attach(&self.area, window)?;
        let mut state = self.state.borrow_mut();
        if let Some(old) = state.submission.replace(observer) {
            old.invalidate();
        }
        state
            .events
            .retain(|event| !matches!(event, AdapterEvent::Drawn { .. }));
        drop(state);
        self.area.queue_render();
        Ok(())
    }
    pub fn take_window_submission(&self) -> Option<WindowSubmission> {
        self.state.borrow().submission.as_ref()?.take_submission()
    }
    /// 先消费实际窗口提交证明，再在同一执行器中精确确认并退休帧回执。
    /// 不切换上下文，不执行 GL 调用；调用者应在提交下一批事务以前调用。
    pub fn take_confirmed_presentation(
        &self,
    ) -> Result<Option<ConfirmedGlPresentation>, AdapterError> {
        let mut state = self.state.borrow_mut();
        let Some(submission) = state
            .submission
            .as_ref()
            .and_then(|observer| observer.take_submission())
        else {
            return Ok(None);
        };
        let draw = submission.consume().map_err(AdapterError::Submission)?;
        let active = state
            .active
            .as_mut()
            .ok_or(AdapterError::Submission(SubmissionError::Lifecycle))?;
        active
            .renderer
            .confirm_submitted_draw(draw)
            .map(Some)
            .map_err(AdapterError::Gl)
    }
    pub fn take_submission_error(&self) -> Option<SubmissionError> {
        self.state.borrow().submission.as_ref()?.take_error()
    }
    pub fn submission_diagnostics(&self) -> Option<crate::presentation::SubmissionDiagnostics> {
        Some(self.state.borrow().submission.as_ref()?.diagnostics())
    }
    /// 正常主动卸载；调用后永久拒绝新事务。
    pub fn detach(&self) {
        if self.area.is_realized() && self.area.context().is_some() {
            self.area.make_current();
        }
        let mut state = self.state.borrow_mut();
        state.pending.closed = true;
        state.retire();
    }
}
impl Default for GtkFrameArea {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for GtkFrameArea {
    fn drop(&mut self) {
        let mut state = self.state.borrow_mut();
        state.pending.closed = true;
        state.invalidate();
        if let Some(observer) = state.submission.take() {
            observer.invalidate();
        }
        // 无法保证 Drop 时 current；仅标记丢失，Active/renderer Drop 不调用 GL。
        state.active.take();
    }
}

fn render(
    area: &GLArea,
    gtk_context: &gdk::GLContext,
    state: &mut State,
) -> Result<(), AdapterError> {
    if area.error().is_some()
        || gdk::GLContext::current().as_ref() != Some(gtk_context)
        || area.context().as_ref() != Some(gtk_context)
    {
        return Err(AdapterError::GtkContext);
    }
    let drawable = drawable_size(area.width(), area.height(), area.scale_factor())
        .ok_or(AdapterError::InvalidAllocation)?;
    if state.active.is_none() {
        let context = create_context(gtk_context)?;
        let renderer = RemoteGlRenderer::create(&context).map_err(AdapterError::Gl)?;
        state.active = Some(Active {
            context,
            renderer,
            remote: None,
        });
    }
    let active = state.active.as_mut().expect("已初始化执行器");
    // GLArea 每次 snapshot 都可能更换附件，绝不缓存 FBO/texture 名称。
    let target = GlRenderTarget::capture_with_output_contract(
        &active.context,
        drawable,
        GlOutputContract::SrgbEncodedRgba8,
    )
    .map_err(AdapterError::Gl)?;
    if let Some(transactions) = state.pending.take() {
        let outcome = active
            .renderer
            .apply_batch(transactions)
            .map_err(AdapterError::Batch)?;
        if let Some(surface) = outcome.installed_surface {
            active.remote = Some(surface.size);
        }
        state.pending.applied();
    }
    if let Some(remote) = active.remote {
        let viewport = ContentViewport::fit(remote, drawable);
        if let Some(receipt) = active
            .renderer
            .draw(&target, viewport)
            .map_err(AdapterError::Gl)?
        {
            if let Some(observer) = &state.submission {
                // 不克隆证明。布局或 bootstrap 导致的关联拒绝由独立 observer 错误出口报告。
                let _ = observer.record_draw(receipt);
            } else {
                state.event(AdapterEvent::Drawn { receipt, viewport });
            }
        }
    }
    Ok(())
}

fn create_context(gtk_context: &gdk::GLContext) -> Result<ExternalContext, AdapterError> {
    // libepoxy 的公开 epoxy_gl* 导出是函数指针变量。读取变量内容，不能调用 dlsym 地址。
    let library = Rc::new(
        unsafe { libloading::Library::new("libepoxy.so.0") }.map_err(|_| AdapterError::Loader)?,
    );
    let loader_owner = library.clone();
    let retained_context = gtk_context.clone();
    // 安全边界：GTK 主线程 render 信号中 actual current 已核对；闭包强持有 GTK context
    // 与 library，且每个新 realize 创建独立 glow，不跨 context/epoch 复用 dispatch 项。
    unsafe {
        ExternalContext::new(
            move |name| {
                let symbol = format!("epoxy_{name}\0");
                loader_owner
                    .get::<*const *const c_void>(symbol.as_bytes())
                    .map_or(std::ptr::null(), |address| **address)
            },
            move || {
                let _keep_library_alive = &library;
                gdk::GLContext::current().as_ref() == Some(&retained_context)
            },
        )
        .map_err(AdapterError::Gl)
    }
}
