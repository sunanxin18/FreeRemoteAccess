use crate::{gl_viewport, GlBatchFailure, GlError};
use frd_core::{ContentViewport, PixelSize};
use frd_frame::FrameTransaction;
use frd_render_state::{
    BatchApplyOutcome, PlannedUpdateData, PresentationReceipt, RecoveryRequirement,
    RemoteUpdateState,
};
use glow::HasContext;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    rc::Rc,
    thread::{self, ThreadId},
};

#[derive(Default)]
struct Objects {
    textures: Vec<glow::NativeTexture>,
    programs: Vec<glow::NativeProgram>,
    arrays: Vec<glow::NativeVertexArray>,
}
impl Objects {
    unsafe fn delete(self, gl: &glow::Context) {
        for texture in self.textures {
            gl.delete_texture(texture);
        }
        for program in self.programs {
            gl.delete_program(program);
        }
        for array in self.arrays {
            gl.delete_vertex_array(array);
        }
    }
}

struct ContextInner {
    gl: glow::Context,
    actual_current: Box<dyn Fn() -> bool>,
    thread: ThreadId,
    alive: Cell<bool>,
    epoch: Cell<u64>,
    deferred: RefCell<Objects>,
}
/// 一次外部上下文生命周期。Rc 保证句柄、渲染器和回执不跨线程。
#[derive(Clone)]
pub struct ExternalContext(Rc<ContextInner>);
impl ExternalContext {
    /// # Safety
    /// loader 必须提供同一个活着的 Linux desktop GL 3.3+ 上下文的函数。
    /// actual_current 必须读取宿主实际 current identity，且为 false 时不得隐瞒。
    /// 宿主必须在销毁/替换上下文前调用 mark_lost，或在当前上下文下 detach/drain；
    /// GL 操作期间不得重入、切换或销毁上下文。闭包必须保持所引用宿主对象存活。
    pub unsafe fn new(
        loader: impl FnMut(&str) -> *const c_void,
        actual_current: impl Fn() -> bool + 'static,
    ) -> Result<Self, GlError> {
        if !actual_current() {
            return Err(GlError::ContextNotCurrent);
        }
        let gl = glow::Context::from_loader_function(loader);
        let version = gl.version();
        if version.is_embedded || (version.major, version.minor) < (3, 3) {
            return Err(GlError::UnsupportedContext);
        }
        if gl.get_parameter_i32(glow::CONTEXT_PROFILE_MASK) & glow::CONTEXT_CORE_PROFILE_BIT as i32
            == 0
        {
            return Err(GlError::UnsupportedContext);
        }
        let context = Self(Rc::new(ContextInner {
            gl,
            actual_current: Box::new(actual_current),
            thread: thread::current().id(),
            alive: Cell::new(true),
            epoch: Cell::new(1),
            deferred: RefCell::new(Objects::default()),
        }));
        context.clean()?;
        Ok(context)
    }
    fn check(&self) -> Result<(), GlError> {
        if !self.0.alive.get() {
            return Err(GlError::ContextLost);
        }
        if thread::current().id() != self.0.thread || !(self.0.actual_current)() {
            return Err(GlError::ContextNotCurrent);
        }
        Ok(())
    }
    fn clean(&self) -> Result<(), GlError> {
        self.check()?;
        // 不吞掉宿主残留错误并继续操作：错误必须由宿主检查处理。
        if unsafe { self.0.gl.get_error() } != glow::NO_ERROR {
            return Err(GlError::GlCommand);
        }
        Ok(())
    }
    pub fn mark_lost(&self) {
        self.0.alive.set(false);
        self.0.epoch.set(self.0.epoch.get().saturating_add(1));
        // 名称只属于旧上下文，不能在新上下文中删除同值名称。
        self.0.deferred.take();
    }
    pub fn drain_deletions(&self) -> Result<(), GlError> {
        self.clean()?;
        unsafe {
            self.0.deferred.take().delete(&self.0.gl);
        }
        self.clean()
    }
    fn retire(&self, epoch: u64, objects: Objects) {
        if self.0.alive.get() && self.0.epoch.get() == epoch {
            let mut queue = self.0.deferred.borrow_mut();
            queue.textures.extend(objects.textures);
            queue.programs.extend(objects.programs);
            queue.arrays.extend(objects.arrays);
        }
    }
}

/// 只绑定当前非默认 FBO；尺寸由宿主声明，draw 时复核 FBO 身份与 sRGB 编码。
pub struct GlRenderTarget {
    context: ExternalContext,
    epoch: u64,
    framebuffer: glow::NativeFramebuffer,
    size: PixelSize,
}
impl GlRenderTarget {
    pub fn capture(context: &ExternalContext, size: PixelSize) -> Result<Self, GlError> {
        context.clean()?;
        let gl = &context.0.gl;
        let framebuffer = unsafe { gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING) }
            .ok_or(GlError::InvalidTarget)?;
        let target = Self {
            context: context.clone(),
            epoch: context.0.epoch.get(),
            framebuffer,
            size,
        };
        target.validate(context)?;
        Ok(target)
    }
    fn validate(&self, context: &ExternalContext) -> Result<(), GlError> {
        context.check()?;
        if !Rc::ptr_eq(&self.context.0, &context.0) || self.epoch != context.0.epoch.get() {
            return Err(GlError::InvalidTarget);
        }
        unsafe {
            let gl = &context.0.gl;
            if gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING)
                != Some(self.framebuffer)
                || gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE
                || gl.get_framebuffer_attachment_parameter_i32(
                    glow::DRAW_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING,
                ) != glow::SRGB as i32
                || gl.get_parameter_i32(glow::DRAW_BUFFER0) != glow::COLOR_ATTACHMENT0 as i32
            {
                return Err(GlError::InvalidTarget);
            }
            // 清黑只拥有颜色附件0；不能清除宿主的其他draw buffer。
            for index in 1..gl.get_parameter_i32(glow::MAX_DRAW_BUFFERS) as u32 {
                if gl.get_parameter_i32(glow::DRAW_BUFFER0 + index) != glow::NONE as i32 {
                    return Err(GlError::InvalidTarget);
                }
            }
            if gl.get_framebuffer_attachment_parameter_i32(
                glow::DRAW_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE,
            ) != glow::TEXTURE as i32
            {
                return Err(GlError::InvalidTarget);
            }
            let name = gl.get_framebuffer_attachment_parameter_i32(
                glow::DRAW_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::FRAMEBUFFER_ATTACHMENT_OBJECT_NAME,
            ) as u32;
            let level = gl.get_framebuffer_attachment_parameter_i32(
                glow::DRAW_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL,
            );
            let texture =
                glow::NativeTexture(std::num::NonZeroU32::new(name).ok_or(GlError::InvalidTarget)?);
            let previous = gl.get_parameter_texture(glow::TEXTURE_BINDING_2D);
            context.clean()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            // GL 3.3 没有纹理目标查询；非二维附件绑定失败时立即停止，
            // 不得查询仍绑定的宿主纹理，也不得留下本次绑定产生的错误。
            if gl.get_error() != glow::NO_ERROR {
                gl.bind_texture(glow::TEXTURE_2D, previous);
                context.clean()?;
                return Err(GlError::InvalidTarget);
            }
            let dimensions = [
                gl.get_tex_level_parameter_i32(glow::TEXTURE_2D, level, glow::TEXTURE_WIDTH),
                gl.get_tex_level_parameter_i32(glow::TEXTURE_2D, level, glow::TEXTURE_HEIGHT),
            ];
            gl.bind_texture(glow::TEXTURE_2D, previous);
            context.clean()?;
            if dimensions != [self.size.width as i32, self.size.height as i32] {
                return Err(GlError::InvalidTarget);
            }
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            if viewport != [0, 0, self.size.width as i32, self.size.height as i32] {
                return Err(GlError::InvalidTarget);
            }
        }
        context.clean()
    }
}

/// GL draw 成功记录；不是屏幕呈现确认，不能用于生产 ACK。
pub struct DrawReceipt {
    receipt: PresentationReceipt,
    context: ExternalContext,
    epoch: u64,
    renderer_serial: Rc<Cell<u64>>,
    serial: u64,
}
impl DrawReceipt {
    pub fn frame(&self) -> &PresentationReceipt {
        &self.receipt
    }
    pub fn is_valid(&self) -> bool {
        self.context.0.alive.get()
            && self.context.0.epoch.get() == self.epoch
            && self.renderer_serial.get() == self.serial
    }
}

pub struct RemoteGlRenderer {
    context: ExternalContext,
    epoch: u64,
    state: RemoteUpdateState,
    serial: Rc<Cell<u64>>,
    texture: Option<glow::NativeTexture>,
    surface_size: Option<PixelSize>,
    program: Option<glow::NativeProgram>,
    vao: Option<glow::NativeVertexArray>,
}
impl RemoteGlRenderer {
    pub fn create(context: &ExternalContext) -> Result<Self, GlError> {
        context.clean()?;
        let (program, vao) = unsafe { create_pipeline(&context.0.gl)? };
        if let Err(error) = context.clean() {
            context.retire(
                context.0.epoch.get(),
                Objects {
                    programs: vec![program],
                    arrays: vec![vao],
                    ..Default::default()
                },
            );
            return Err(error);
        }
        Ok(Self {
            context: context.clone(),
            epoch: context.0.epoch.get(),
            state: RemoteUpdateState::default(),
            serial: Rc::new(Cell::new(0)),
            texture: None,
            surface_size: None,
            program: Some(program),
            vao: Some(vao),
        })
    }
    fn ready(&self) -> Result<(), GlError> {
        if self.program.is_none() || self.vao.is_none() || self.epoch != self.context.0.epoch.get()
        {
            return Err(GlError::ContextLost);
        }
        self.context.drain_deletions()?;
        self.context.clean()
    }
    fn advance_serial(&self) -> Result<(), GlError> {
        match crate::next_draw_serial(self.serial.get()) {
            Ok(next) => {
                self.serial.set(next);
                Ok(())
            }
            Err(error) => {
                self.context.mark_lost();
                Err(error)
            }
        }
    }
    fn quarantine(&mut self) -> Option<RecoveryRequirement> {
        let _ = self.advance_serial();
        self.surface_size = None;
        let requirement = self
            .state
            .current_generation()
            .map(|_| self.state.invalidate_for_device_loss());
        if let Some(texture) = self.texture.take() {
            self.context.retire(
                self.epoch,
                Objects {
                    textures: vec![texture],
                    ..Default::default()
                },
            );
        }
        requirement
    }
    pub fn apply_batch(
        &mut self,
        transactions: Vec<FrameTransaction>,
    ) -> Result<BatchApplyOutcome, GlBatchFailure> {
        if let Err(error) = self.advance_serial().and_then(|_| self.ready()) {
            let recovery = self.quarantine();
            return Err(GlBatchFailure {
                identity: None,
                error,
                recovery,
            });
        }
        let candidate =
            self.state
                .prepare_batch(transactions)
                .map_err(|failure| GlBatchFailure {
                    identity: failure.identity,
                    error: failure.error.into(),
                    recovery: None,
                })?;
        let identity = candidate.identity();
        let mut current = self.texture;
        let mut allocated = Vec::new();
        let execution = unsafe {
            let gl = &self.context.0.gl;
            let saved = UploadState::save(gl);
            let result = (|| {
                gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
                gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
                gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);
                for operation in candidate.operations() {
                    match operation.data() {
                        PlannedUpdateData::StartupReset { size, .. } => {
                            let max = gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) as u32;
                            if size.width > max || size.height > max {
                                return Err(GlError::ResourceCreation);
                            }
                            let texture =
                                gl.create_texture().map_err(|_| GlError::ResourceCreation)?;
                            allocated.push(texture);
                            current = Some(texture);
                            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                            gl.tex_parameter_i32(
                                glow::TEXTURE_2D,
                                glow::TEXTURE_MIN_FILTER,
                                glow::LINEAR as i32,
                            );
                            gl.tex_parameter_i32(
                                glow::TEXTURE_2D,
                                glow::TEXTURE_MAG_FILTER,
                                glow::LINEAR as i32,
                            );
                            gl.tex_parameter_i32(
                                glow::TEXTURE_2D,
                                glow::TEXTURE_WRAP_S,
                                glow::CLAMP_TO_EDGE as i32,
                            );
                            gl.tex_parameter_i32(
                                glow::TEXTURE_2D,
                                glow::TEXTURE_WRAP_T,
                                glow::CLAMP_TO_EDGE as i32,
                            );
                            gl.tex_image_2d(
                                glow::TEXTURE_2D,
                                0,
                                glow::SRGB8_ALPHA8 as i32,
                                size.width as i32,
                                size.height as i32,
                                0,
                                glow::RGBA,
                                glow::UNSIGNED_BYTE,
                                glow::PixelUnpackData::Slice(None),
                            );
                        }
                        PlannedUpdateData::Damage { patches, .. } => {
                            gl.bind_texture(
                                glow::TEXTURE_2D,
                                Some(current.ok_or(GlError::ResetRequired)?),
                            );
                            for (patch, upload) in patches.iter().zip(operation.uploads()) {
                                let rect = upload.rect();
                                let bytes = patch.pixels.as_bytes();
                                if upload.stride_bytes() % 4 == 0 {
                                    gl.pixel_store_i32(
                                        glow::UNPACK_ROW_LENGTH,
                                        (upload.stride_bytes() / 4) as i32,
                                    );
                                    gl.tex_sub_image_2d(
                                        glow::TEXTURE_2D,
                                        0,
                                        rect.x as i32,
                                        rect.y as i32,
                                        rect.width as i32,
                                        rect.height as i32,
                                        glow::RGBA,
                                        glow::UNSIGNED_BYTE,
                                        glow::PixelUnpackData::Slice(Some(bytes)),
                                    );
                                } else {
                                    gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
                                    // 非像素整数倍 padding 逐行原切片，零 CPU 转色或重排。
                                    for row in 0..rect.height as usize {
                                        let start = row * upload.stride_bytes() as usize;
                                        gl.tex_sub_image_2d(
                                            glow::TEXTURE_2D,
                                            0,
                                            rect.x as i32,
                                            rect.y as i32 + row as i32,
                                            rect.width as i32,
                                            1,
                                            glow::RGBA,
                                            glow::UNSIGNED_BYTE,
                                            glow::PixelUnpackData::Slice(Some(
                                                &bytes[start..start + rect.width as usize * 4],
                                            )),
                                        );
                                    }
                                }
                            }
                        }
                        PlannedUpdateData::Boundary(_) => {}
                    }
                    self.context.clean()?;
                }
                Ok(())
            })();
            saved.restore(gl);
            result.and_then(|_| self.context.clean())
        };
        if let Err(error) = execution {
            drop(candidate);
            self.context.retire(
                self.epoch,
                Objects {
                    textures: allocated,
                    ..Default::default()
                },
            );
            let recovery = self.quarantine();
            return Err(GlBatchFailure {
                identity: Some(identity),
                error,
                recovery,
            });
        }
        let outcome = candidate.commit();
        if let Some(installed) = outcome.installed_surface {
            self.surface_size = Some(installed.size);
        }
        if current != self.texture {
            if let Some(old) = self.texture.take() {
                allocated.push(old);
            }
            self.texture = current;
        }
        allocated.retain(|texture| Some(*texture) != self.texture);
        self.context.retire(
            self.epoch,
            Objects {
                textures: allocated,
                ..Default::default()
            },
        );
        Ok(outcome)
    }
    pub fn draw(
        &mut self,
        target: &GlRenderTarget,
        viewport: ContentViewport,
    ) -> Result<Option<DrawReceipt>, GlError> {
        let result = (|| {
            self.advance_serial()?;
            self.ready()?;
            target.validate(&self.context)?;
            if viewport.drawable != target.size || Some(viewport.remote) != self.surface_size {
                return Err(GlError::InvalidViewport);
            }
            unsafe {
                let gl = &self.context.0.gl;
                if ((gl.version().major, gl.version().minor) >= (4, 5)
                    || gl.supported_extensions().contains("GL_ARB_clip_control"))
                    && (gl.get_parameter_i32(glow::CLIP_ORIGIN) != glow::LOWER_LEFT as i32
                        || gl.get_parameter_i32(glow::CLIP_DEPTH_MODE)
                            != glow::NEGATIVE_ONE_TO_ONE as i32)
                {
                    return Err(GlError::InvalidTarget);
                }
            }
            let region = gl_viewport(target.size, viewport.content)?;
            let texture = self.texture.ok_or(GlError::ResetRequired)?;
            unsafe {
                let gl = &self.context.0.gl;
                let saved = DrawState::save(gl);
                set_viewport_zero(gl, region);
                for cap in DRAW_DISABLED {
                    gl.disable(cap);
                }
                for index in 0..saved.clip_distances.len() {
                    gl.disable(glow::CLIP_DISTANCE0 + index as u32);
                }
                gl.disable_draw_buffer(glow::BLEND, 0);
                set_scissor_zero(gl, false);
                gl.enable(glow::FRAMEBUFFER_SRGB);
                gl.color_mask_draw_buffer(0, true, true, true, true);
                gl.clear_color(0.0, 0.0, 0.0, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
                gl.active_texture(glow::TEXTURE0);
                gl.bind_sampler(0, None);
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.use_program(self.program);
                gl.bind_vertex_array(self.vao);
                gl.polygon_mode(glow::FRONT_AND_BACK, glow::FILL);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                saved.restore(gl);
            }
            self.context.clean()?;
            Ok(self.state.pending_receipt().map(|receipt| DrawReceipt {
                receipt,
                context: self.context.clone(),
                epoch: self.epoch,
                renderer_serial: self.serial.clone(),
                serial: self.serial.get(),
            }))
        })();
        if result.is_err() {
            self.quarantine();
        }
        result
    }
    /// 必须在宿主 unrealize 的有效 current 上下文中调用，再销毁原生上下文。
    pub fn detach(&mut self) -> Result<Option<RecoveryRequirement>, GlError> {
        // 无 current 时仍撤销回执和退休资源；仅实际 GL 删除要求 current。
        let recovery = self.quarantine();
        self.retire_pipeline();
        self.context.drain_deletions()?;
        Ok(recovery)
    }
    fn retire_pipeline(&mut self) {
        self.context.retire(
            self.epoch,
            Objects {
                programs: self.program.take().into_iter().collect(),
                arrays: self.vao.take().into_iter().collect(),
                ..Default::default()
            },
        );
    }
}
impl Drop for RemoteGlRenderer {
    fn drop(&mut self) {
        self.quarantine();
        self.retire_pipeline();
    }
}

const DRAW_DISABLED: [u32; 6] = [
    glow::DEPTH_TEST,
    glow::STENCIL_TEST,
    glow::CULL_FACE,
    glow::RASTERIZER_DISCARD,
    glow::COLOR_LOGIC_OP,
    glow::DITHER,
];
unsafe fn set_viewport_zero(gl: &glow::Context, viewport: [i32; 4]) {
    let indexed = (gl.version().major, gl.version().minor) >= (4, 1)
        || gl.supported_extensions().contains("GL_ARB_viewport_array");
    if indexed {
        // 非索引 glViewport 会重写所有 viewport；执行器只拥有索引0。
        gl.viewport_f32_slice(0, 1, &[viewport.map(|value| value as f32)]);
    } else {
        gl.viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
    }
}
unsafe fn set_scissor_zero(gl: &glow::Context, enabled: bool) {
    let indexed = (gl.version().major, gl.version().minor) >= (4, 1)
        || gl.supported_extensions().contains("GL_ARB_viewport_array");
    if indexed {
        if enabled {
            gl.enable_draw_buffer(glow::SCISSOR_TEST, 0);
        } else {
            gl.disable_draw_buffer(glow::SCISSOR_TEST, 0);
        }
    } else if enabled {
        gl.enable(glow::SCISSOR_TEST);
    } else {
        gl.disable(glow::SCISSOR_TEST);
    }
}

struct UploadState {
    texture: Option<glow::NativeTexture>,
    buffer: Option<glow::NativeBuffer>,
    unpack: [i32; 4],
}
impl UploadState {
    unsafe fn save(gl: &glow::Context) -> Self {
        Self {
            texture: gl.get_parameter_texture(glow::TEXTURE_BINDING_2D),
            buffer: gl.get_parameter_buffer(glow::PIXEL_UNPACK_BUFFER_BINDING),
            unpack: [
                glow::UNPACK_ALIGNMENT,
                glow::UNPACK_ROW_LENGTH,
                glow::UNPACK_SKIP_PIXELS,
                glow::UNPACK_SKIP_ROWS,
            ]
            .map(|key| gl.get_parameter_i32(key)),
        }
    }
    unsafe fn restore(self, gl: &glow::Context) {
        gl.bind_texture(glow::TEXTURE_2D, self.texture);
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, self.buffer);
        for (key, value) in [
            glow::UNPACK_ALIGNMENT,
            glow::UNPACK_ROW_LENGTH,
            glow::UNPACK_SKIP_PIXELS,
            glow::UNPACK_SKIP_ROWS,
        ]
        .into_iter()
        .zip(self.unpack)
        {
            gl.pixel_store_i32(key, value);
        }
    }
}
struct DrawState {
    program: Option<glow::NativeProgram>,
    vao: Option<glow::NativeVertexArray>,
    active: u32,
    texture: Option<glow::NativeTexture>,
    sampler: Option<glow::NativeSampler>,
    viewport: [i32; 4],
    enabled: [bool; 6],
    clip_distances: Vec<bool>,
    blend: bool,
    scissor: bool,
    srgb: bool,
    mask: [bool; 4],
    clear: [f32; 4],
    polygon: i32,
}
impl DrawState {
    unsafe fn save(gl: &glow::Context) -> Self {
        let active = gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32;
        gl.active_texture(glow::TEXTURE0);
        let mut viewport = [0; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
        let mut polygon = [0; 2];
        gl.get_parameter_i32_slice(glow::POLYGON_MODE, &mut polygon);
        let mut clear = [0.0; 4];
        gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear);
        Self {
            clear,
            polygon: polygon[0],
            program: gl.get_parameter_program(glow::CURRENT_PROGRAM),
            vao: gl.get_parameter_vertex_array(glow::VERTEX_ARRAY_BINDING),
            active,
            texture: gl.get_parameter_texture(glow::TEXTURE_BINDING_2D),
            sampler: gl.get_parameter_sampler(glow::SAMPLER_BINDING),
            viewport,
            blend: gl.is_enabled(glow::BLEND),
            scissor: gl.is_enabled(glow::SCISSOR_TEST),
            enabled: DRAW_DISABLED.map(|cap| gl.is_enabled(cap)),
            clip_distances: (0..gl.get_parameter_i32(glow::MAX_CLIP_DISTANCES) as u32)
                .map(|index| gl.is_enabled(glow::CLIP_DISTANCE0 + index))
                .collect(),
            srgb: gl.is_enabled(glow::FRAMEBUFFER_SRGB),
            mask: gl.get_parameter_bool_array(glow::COLOR_WRITEMASK),
        }
    }
    unsafe fn restore(self, gl: &glow::Context) {
        gl.use_program(self.program);
        gl.bind_vertex_array(self.vao);
        gl.bind_texture(glow::TEXTURE_2D, self.texture);
        gl.bind_sampler(0, self.sampler);
        gl.active_texture(self.active);
        set_viewport_zero(gl, self.viewport);
        for (cap, enabled) in DRAW_DISABLED
            .into_iter()
            .zip(self.enabled)
            .chain([(glow::FRAMEBUFFER_SRGB, self.srgb)])
            .chain(
                self.clip_distances
                    .into_iter()
                    .enumerate()
                    .map(|(index, enabled)| (glow::CLIP_DISTANCE0 + index as u32, enabled)),
            )
        {
            if enabled {
                gl.enable(cap);
            } else {
                gl.disable(cap);
            }
        }
        gl.clear_color(self.clear[0], self.clear[1], self.clear[2], self.clear[3]);
        gl.polygon_mode(glow::FRONT_AND_BACK, self.polygon as u32);
        if self.blend {
            gl.enable_draw_buffer(glow::BLEND, 0);
        } else {
            gl.disable_draw_buffer(glow::BLEND, 0);
        }
        set_scissor_zero(gl, self.scissor);
        gl.color_mask_draw_buffer(0, self.mask[0], self.mask[1], self.mask[2], self.mask[3]);
    }
}
unsafe fn create_pipeline(
    gl: &glow::Context,
) -> Result<(glow::NativeProgram, glow::NativeVertexArray), GlError> {
    let program = gl.create_program().map_err(|_| GlError::ResourceCreation)?;
    let sources = [
        (glow::VERTEX_SHADER, "#version 330 core\nout vec2 uv; void main(){ vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2); uv=vec2(p.x,1.0-p.y); gl_Position=vec4(p*2.0-1.0,0.0,1.0); }"),
        (glow::FRAGMENT_SHADER, "#version 330 core\nin vec2 uv; out vec4 color; uniform sampler2D frame; void main(){color=vec4(texture(frame,uv).bgr,1.0);}"),
    ];
    for (kind, source) in sources {
        let shader = match gl.create_shader(kind) {
            Ok(shader) => shader,
            Err(_) => {
                gl.delete_program(program);
                return Err(GlError::ResourceCreation);
            }
        };
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            gl.delete_shader(shader);
            gl.delete_program(program);
            return Err(GlError::ShaderCompilation);
        }
        gl.attach_shader(program, shader);
        gl.delete_shader(shader);
    }
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        gl.delete_program(program);
        return Err(GlError::ShaderCompilation);
    }
    let vao = match gl.create_vertex_array() {
        Ok(vao) => vao,
        Err(_) => {
            gl.delete_program(program);
            return Err(GlError::ResourceCreation);
        }
    };
    Ok((program, vao))
}
