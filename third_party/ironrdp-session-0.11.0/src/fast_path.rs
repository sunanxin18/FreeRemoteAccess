use std::sync::Arc;

use ironrdp_bulk::BulkCompressor;
use ironrdp_core::{DecodeErrorKind, ReadCursor, WriteBuf, decode_cursor};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::legacy_native::{KernelError, NativeKernels, PackedDepth, Pixel32Format, PixelRowSource};
use ironrdp_graphics::pointer::{DecodedPointer, PointerBitmapTarget};
use ironrdp_graphics::rdp6::BitmapStreamDecoder;
use ironrdp_graphics::rle::RlePixelFormat;
use ironrdp_pdu::bitmap::BitmapUpdateData;
use ironrdp_pdu::codecs::rfx::FrameAcknowledgePdu;
use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdate, FastPathUpdatePdu, Fragmentation};
use ironrdp_pdu::geometry::{InclusiveRectangle, Rectangle as _};
use ironrdp_pdu::pointer::PointerUpdateData;
use ironrdp_pdu::rdp::capability_sets::{CODEC_ID_NONE, CODEC_ID_REMOTEFX, CodecId};
use ironrdp_pdu::rdp::headers::{CompressionFlags, ShareDataPdu};
use ironrdp_pdu::surface_commands::{FrameAction, FrameMarkerPdu, SurfaceCommand};
use tracing::{debug, trace, warn};

use crate::image::{CoverageCollector, DecodedImage};
use crate::palette::Palette;
use crate::pointer::PointerCache;
use crate::{SessionError, SessionErrorExt as _, SessionResult, custom_err, reason_err, rfx};

#[derive(Debug)]
pub enum UpdateKind {
    None,
    Region(InclusiveRectangle),
    PointerDefault,
    PointerHidden,
    PointerPosition { x: u16, y: u16 },
    PointerBitmap(Arc<DecodedPointer>),
}

pub struct Processor {
    complete_data: CompleteData,
    rfx_handler: rfx::DecodingContext,
    marker_processor: FrameMarkerProcessor,
    bitmap_stream_decoder: BitmapStreamDecoder,
    native_legacy_kernels: Option<NativeKernels>,
    pointer_cache: PointerCache,
    use_system_pointer: bool,
    mouse_pos_update: Option<(u16, u16)>,
    enable_server_pointer: bool,
    pointer_software_rendering: bool,
    /// Bulk decompressor for server-to-client compressed PDUs.
    /// `None` when compression was not negotiated.
    bulk_decompressor: Option<BulkCompressor>,
    /// Current 8bpp color palette. Updated by Palette fast-path updates.
    palette: Palette,
    #[cfg(feature = "qoiz")]
    zdctx: zstd_safe::DCtx<'static>,
}

impl Processor {
    /// 显式选择原生 legacy decoder；Bitmap 快慢路径与 RFX 共用同一能力。
    pub fn set_native_legacy_kernels(&mut self, kernels: NativeKernels) {
        self.native_legacy_kernels = Some(kernels);
        self.rfx_handler.set_native_legacy_kernels(kernels);
    }

    pub fn update_mouse_pos(&mut self, x: u16, y: u16) {
        self.mouse_pos_update = Some((x, y));
    }

    /// Process input fast path frame and return list of updates.
    pub fn process(
        &mut self,
        image: &mut DecodedImage,
        input: &[u8],
        output: &mut WriteBuf,
    ) -> SessionResult<Vec<UpdateKind>> {
        self.process_collected(image, input, output, &mut CoverageCollector::default())
    }

    pub(crate) fn process_collected(
        &mut self,
        image: &mut DecodedImage,
        input: &[u8],
        output: &mut WriteBuf,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut processor_updates = Vec::new();

        // 默认保留旧次序；native 的首个失败不能先消费 pending mouse 或改 cursor。
        if self.native_legacy_kernels.is_none() {
            if self.mouse_pos_update.is_some() {
                coverage.reserve_entry()?;
            }
            if let Some((x, y)) = self.mouse_pos_update.take() {
                if let Some(rect) = image.move_pointer_with_kernels(x, y, self.native_legacy_kernels.as_ref())? {
                    processor_updates.push(UpdateKind::Region(rect));
                }
            }
        }

        coverage.finish_updates(&processor_updates)?;
        let mut input = ReadCursor::new(input);

        let header = decode_cursor::<FastPathHeader>(&mut input).map_err(SessionError::decode)?;
        trace!(fast_path_header = ?header, "Received Fast-Path packet");

        let mut flush_pending_mouse = true;
        // A single FastPath output PDU can contain multiple updates.
        // Loop over all updates within the PDU payload.
        while !input.is_empty() {
            let update_result =
                self.process_single_update(&mut input, image, output, &mut flush_pending_mouse, coverage)?;
            coverage.finish_updates(&update_result)?;
            processor_updates.extend(update_result);
        }

        if flush_pending_mouse {
            if coverage.enabled {
                let mut mouse_updates = Vec::new();
                self.flush_native_mouse(image, &mut mouse_updates, coverage)?;
                coverage.finish_updates(&mouse_updates)?;
                processor_updates.extend(mouse_updates);
            } else {
                // collector-off 原路径复用已分配 updates，不新增 mouse Vec allocation。
                self.flush_native_mouse(image, &mut processor_updates, coverage)?;
            }
        }

        Ok(processor_updates)
    }

    fn flush_native_mouse(
        &mut self,
        image: &mut DecodedImage,
        updates: &mut Vec<UpdateKind>,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<()> {
        if self.native_legacy_kernels.is_some() {
            if self.mouse_pos_update.is_some() {
                coverage.reserve_entries(1 + updates.iter().filter(|u| matches!(u, UpdateKind::Region(_))).count())?;
            }
            if let Some((x, y)) = self.mouse_pos_update {
                if let Some(rect) = image.move_pointer_with_kernels(x, y, self.native_legacy_kernels.as_ref())? {
                    updates.push(UpdateKind::Region(rect));
                }
                self.mouse_pos_update = None;
            }
        }
        Ok(())
    }

    /// Process a single FastPath update from the cursor, advancing past it.
    fn process_single_update(
        &mut self,
        input: &mut ReadCursor<'_>,
        image: &mut DecodedImage,
        output: &mut WriteBuf,
        flush_pending_mouse: &mut bool,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut processor_updates = Vec::new();

        let update_pdu = decode_cursor::<FastPathUpdatePdu<'_>>(input).map_err(SessionError::decode)?;
        trace!(fast_path_update_fragmentation = ?update_pdu.fragmentation);

        // Decompress the payload if the server sent it compressed.
        let decompressed_data;
        let payload = if let Some(flags) = update_pdu.compression_flags {
            if flags.contains(CompressionFlags::COMPRESSED) || flags.contains(CompressionFlags::FLUSHED) {
                let bulk_flags =
                    u32::from(flags.bits()) | u32::from(update_pdu.compression_type.map_or(0, |ct| ct.as_u8()));

                if let Some(ref mut decompressor) = self.bulk_decompressor {
                    let decompressed = decompressor
                        .decompress(update_pdu.data, bulk_flags)
                        .map_err(|e| reason_err!("FastPath", "bulk decompression failed: {}", e))?;
                    // Copy decompressed data before accessing metrics (releases the mutable borrow).
                    decompressed_data = decompressed.to_vec();
                    debug!(
                        compressed_size = update_pdu.data.len(),
                        decompressed_size = decompressed_data.len(),
                        compression_type = ?update_pdu.compression_type,
                        compression_ratio = format_args!("{:.2}x", decompressor.compression_ratio()),
                        total_compressed = decompressor.total_compressed_bytes(),
                        total_uncompressed = decompressor.total_uncompressed_bytes(),
                        "Decompressed FastPath update"
                    );
                    decompressed_data.as_slice()
                } else {
                    warn!("Received compressed FastPath data but no decompressor is configured");
                    update_pdu.data
                }
            } else {
                // Compression flags present but COMPRESSED bit not set — pass data through.
                // Still need to inform the decompressor of FLUSHED/AT_FRONT flags even
                // without compressed payload.
                update_pdu.data
            }
        } else {
            update_pdu.data
        };

        let processed_complete_data = self.complete_data.process_data(payload, update_pdu.fragmentation);

        let update_code = update_pdu.update_code;

        let Some(data) = processed_complete_data else {
            return Ok(processor_updates);
        };

        let update = FastPathUpdate::decode_with_code(data.as_slice(), update_code);

        match update {
            Ok(FastPathUpdate::SurfaceCommands(surface_commands)) => {
                trace!("Received Surface Commands: {} pieces", surface_commands.len());
                let update_region =
                    self.process_surface_commands_collected(image, output, surface_commands, coverage)?;
                *flush_pending_mouse = true;
                if let Some(update_region) = update_region {
                    processor_updates.push(UpdateKind::Region(update_region));
                }
            }
            Ok(FastPathUpdate::Bitmap(bitmap_update)) => {
                trace!("Received bitmap update");
                let updates = self.process_bitmap_update_collected(image, bitmap_update, coverage)?;
                *flush_pending_mouse = true;
                processor_updates.extend(updates);
            }
            Ok(FastPathUpdate::Pointer(update)) => {
                let unmatched_cached = matches!(&update, PointerUpdateData::Cached(c) if self.enable_server_pointer && self.pointer_cache.get(usize::from(c.cache_index)).is_none());
                let updates = self.process_pointer_update_collected(image, update, coverage)?;
                *flush_pending_mouse = !unmatched_cached;
                processor_updates.extend(updates);
            }
            Ok(FastPathUpdate::Palette(palette_data)) => {
                trace!("Received palette update");
                self.palette.process_update(palette_data);
                *flush_pending_mouse = true;
            }
            Err(e) => {
                // FIXME: This seems to be a way of special-handling the error case in FastPathUpdate::decode_cursor_with_code
                // to ignore the unsupported update PDUs, but this is a fragile logic and the rationale behind it is not
                // obvious.
                if let DecodeErrorKind::InvalidField { field, reason } = e.kind() {
                    warn!(field, reason, "Received invalid Fast-Path update");
                    processor_updates.push(UpdateKind::None);
                    *flush_pending_mouse = false;
                } else {
                    return Err(custom_err!("Fast-Path", e));
                }
            }
        };

        Ok(processor_updates)
    }

    /// Process a bitmap update, shared between fast-path and slow-path pipelines.
    pub fn process_bitmap_update(
        &mut self,
        image: &mut DecodedImage,
        bitmap_update: BitmapUpdateData<'_>,
    ) -> SessionResult<Vec<UpdateKind>> {
        self.process_bitmap_update_collected(image, bitmap_update, &mut CoverageCollector::default())
    }

    pub(crate) fn process_bitmap_update_collected(
        &mut self,
        image: &mut DecodedImage,
        bitmap_update: BitmapUpdateData<'_>,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<Vec<UpdateKind>> {
        if coverage.enabled {
            coverage.reserve_entry()?;
        }
        if let Some(kernels) = self.native_legacy_kernels {
            return self.process_bitmap_update_native(image, bitmap_update, &kernels, coverage);
        }
        if coverage.enabled {
            return self.process_bitmap_update_compat(image, bitmap_update, coverage);
        }
        let mut buf = Vec::new();
        let mut update_kind = UpdateKind::None;

        for update in bitmap_update.rectangles {
            trace!("{update:?}");
            buf.clear();

            // Bitmap data is either compressed or uncompressed, depending
            // on whether the BITMAP_COMPRESSION flag is present in the
            // flags field.
            let update_rectangle = if update
                .compression_flags
                .contains(ironrdp_pdu::bitmap::Compression::BITMAP_COMPRESSION)
            {
                if update.bits_per_pixel == 32 {
                    // Compressed bitmaps at a color depth of 32 bpp are compressed using RDP 6.0
                    // Bitmap Compression and stored inside an RDP 6.0 Bitmap Compressed Stream
                    // structure ([MS-RDPEGDI] section 2.2.2.5.1).
                    debug!("32 bpp compressed RDP6_BITMAP_STREAM");

                    match self.bitmap_stream_decoder.decode_bitmap_stream_to_rgb24(
                        update.bitmap_data,
                        &mut buf,
                        usize::from(update.width),
                        usize::from(update.height),
                    ) {
                        Ok(()) => image.apply_rgb24(&buf, &update.rectangle, true)?,
                        Err(err) => {
                            warn!("Invalid RDP6_BITMAP_STREAM: {err}");
                            update.rectangle.clone()
                        }
                    }
                } else {
                    // Compressed bitmaps not in 32 bpp format are compressed using Interleaved
                    // RLE and encapsulated in an RLE Compressed Bitmap Stream structure (section
                    // 2.2.9.1.1.3.1.2.4).
                    debug!(bpp = update.bits_per_pixel, "Non-32 bpp compressed RLE_BITMAP_STREAM",);

                    match ironrdp_graphics::rle::decompress(
                        update.bitmap_data,
                        &mut buf,
                        usize::from(update.width),
                        usize::from(update.height),
                        usize::from(update.bits_per_pixel),
                    ) {
                        Ok(RlePixelFormat::Rgb16) => image.apply_rgb16_bitmap(&buf, &update.rectangle)?,
                        Ok(RlePixelFormat::Rgb15) => image.apply_rgb15_bitmap(&buf, &update.rectangle)?,
                        Ok(RlePixelFormat::Rgb24) => image.apply_bgr24_bitmap(&buf, &update.rectangle)?,
                        Ok(RlePixelFormat::Rgb8) => {
                            image.apply_rgb8_with_palette(&buf, &update.rectangle, self.palette.colors())?
                        }

                        Err(e) => {
                            warn!("Invalid RLE-compressed bitmap: {e}");
                            update.rectangle.clone()
                        }
                    }
                }
            } else {
                // Uncompressed bitmap data is formatted as a bottom-up, left-to-right series of
                // pixels. Each pixel is a whole number of bytes. Each row contains a multiple of
                // four bytes (including up to three bytes of padding, as necessary).
                // [MS-RDPBCGR] 2.2.9.1.1.3.1.2.2
                trace!("Uncompressed raw bitmap");

                let bpp = usize::from(update.bits_per_pixel);
                let width = usize::from(update.width);
                let bytes_per_pixel = bpp.div_ceil(8);
                let row_bytes = width * bytes_per_pixel;
                let padded_row_bytes = (row_bytes + 3) & !3;

                if padded_row_bytes != row_bytes {
                    // Strip per-row padding before passing to the bitmap apply functions,
                    // which expect tightly packed pixel data.
                    buf.clear();
                    for row in update.bitmap_data.chunks(padded_row_bytes) {
                        let end = row_bytes.min(row.len());
                        buf.extend_from_slice(&row[..end]);
                    }

                    match update.bits_per_pixel {
                        8 => image.apply_rgb8_with_palette(&buf, &update.rectangle, self.palette.colors())?,
                        15 => image.apply_rgb15_bitmap(&buf, &update.rectangle)?,
                        16 => image.apply_rgb16_bitmap(&buf, &update.rectangle)?,
                        24 => image.apply_bgr24_bitmap(&buf, &update.rectangle)?,
                        32 => image.apply_rgb32_bitmap(&buf, PixelFormat::BgrX32, &update.rectangle)?,
                        _ => {
                            warn!("Unsupported uncompressed bitmap depth: {bpp} bpp");
                            update.rectangle.clone()
                        }
                    }
                } else {
                    match update.bits_per_pixel {
                        8 => image.apply_rgb8_with_palette(
                            update.bitmap_data,
                            &update.rectangle,
                            self.palette.colors(),
                        )?,
                        15 => image.apply_rgb15_bitmap(update.bitmap_data, &update.rectangle)?,
                        16 => image.apply_rgb16_bitmap(update.bitmap_data, &update.rectangle)?,
                        24 => image.apply_bgr24_bitmap(update.bitmap_data, &update.rectangle)?,
                        32 => image.apply_rgb32_bitmap(update.bitmap_data, PixelFormat::BgrX32, &update.rectangle)?,
                        _ => {
                            warn!("Unsupported uncompressed bitmap depth: {bpp} bpp");
                            update.rectangle.clone()
                        }
                    }
                }
            };

            match update_kind {
                UpdateKind::Region(current) => update_kind = UpdateKind::Region(current.union(&update_rectangle)),
                _ => update_kind = UpdateKind::Region(update_rectangle),
            }
        }

        Ok(vec![update_kind])
    }

    fn process_bitmap_update_native(
        &mut self,
        image: &mut DecodedImage,
        bitmap_update: BitmapUpdateData<'_>,
        kernels: &NativeKernels,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut region: Option<InclusiveRectangle> = None;
        let mut decoded = Vec::new();
        for bitmap in bitmap_update.rectangles {
            let source_width = usize::from(bitmap.width);
            let source_height = usize::from(bitmap.height);
            let compressed = bitmap
                .compression_flags
                .contains(ironrdp_pdu::bitmap::Compression::BITMAP_COMPRESSION);
            let (format, bytes) = native_bitmap_format(bitmap.bits_per_pixel, compressed)?;
            let tight = source_width
                .checked_mul(bytes)
                .ok_or_else(|| custom_err!("native Bitmap geometry", KernelError::InvalidDimensions))?;
            let row_stride = if compressed {
                tight
            } else {
                tight
                    .checked_add(3)
                    .map(|n| n & !3)
                    .ok_or_else(|| custom_err!("native Bitmap geometry", KernelError::InvalidDimensions))?
            };
            image.native_bitmap_size(format, source_width, source_height, row_stride, &bitmap.rectangle)?;
            // 合法 out-of-image 保持整矩形 skip；不制造 empty sentinel 的原点 damage。
            if bitmap.rectangle.right >= image.width() || bitmap.rectangle.bottom >= image.height() {
                continue;
            }
            decoded.clear();
            let source = if compressed {
                if bitmap.bits_per_pixel == 32 {
                    self.bitmap_stream_decoder
                        .decode_bitmap_stream_to_rgb24_native(
                            bitmap.bitmap_data,
                            &mut decoded,
                            source_width,
                            source_height,
                            kernels,
                        )
                        .map_err(|e| custom_err!("decode native RDP6 bitmap", e))?;
                } else {
                    ironrdp_graphics::rle::decompress_native_complete(
                        bitmap.bitmap_data,
                        &mut decoded,
                        source_width,
                        source_height,
                        usize::from(bitmap.bits_per_pixel),
                        kernels,
                    )
                    .map_err(|e| custom_err!("decode complete native RLE bitmap", e))?;
                }
                decoded.as_slice()
            } else {
                bitmap.bitmap_data
            };
            coverage.reserve_rectangles(1)?;
            if let Some(current) = image.apply_bitmap_native(
                source,
                format,
                source_width,
                source_height,
                row_stride,
                true,
                &bitmap.rectangle,
                Some(self.palette.colors()),
                kernels,
            )? {
                coverage.content([bitmap.rectangle.clone()]);
                region = Some(region.map_or_else(|| current.clone(), |prior| prior.union(&current)));
            }
        }
        Ok(vec![region.map_or(UpdateKind::None, UpdateKind::Region)])
    }

    fn process_bitmap_update_compat(
        &mut self,
        image: &mut DecodedImage,
        bitmap_update: BitmapUpdateData<'_>,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut region: Option<InclusiveRectangle> = None;
        let mut decoded = Vec::new();
        for bitmap in bitmap_update.rectangles {
            let compressed = bitmap
                .compression_flags
                .contains(ironrdp_pdu::bitmap::Compression::BITMAP_COMPRESSION);
            let (format, bytes) = match native_bitmap_format(bitmap.bits_per_pixel, compressed) {
                Ok(v) => v,
                Err(_) => {
                    warn!("不支持的 Bitmap 深度：{}", bitmap.bits_per_pixel);
                    continue;
                }
            };
            let sw = usize::from(bitmap.width);
            let sh = usize::from(bitmap.height);
            let tight = sw
                .checked_mul(bytes)
                .ok_or_else(|| custom_err!("Bitmap geometry", KernelError::InvalidDimensions))?;
            let stride = if compressed {
                tight
            } else {
                tight
                    .checked_add(3)
                    .map(|n| n & !3)
                    .ok_or_else(|| custom_err!("Bitmap geometry", KernelError::InvalidDimensions))?
            };
            let expected = image.native_bitmap_size(format, sw, sh, stride, &bitmap.rectangle)?;
            // 合法图外整矩形 skip；不把 sentinel 误认成真实原点。
            if bitmap.rectangle.right >= image.width() || bitmap.rectangle.bottom >= image.height() {
                continue;
            }
            decoded.clear();
            let source = if compressed {
                if bitmap.bits_per_pixel == 32 {
                    sw.checked_mul(sh)
                        .and_then(|n| n.checked_mul(4))
                        .filter(|&n| n <= ironrdp_graphics::rle::MAX_NATIVE_BITMAP_BYTES && n <= isize::MAX as usize)
                        .ok_or_else(|| custom_err!("RDP6 plane resource limit", KernelError::InvalidDimensions))?;
                    match self.bitmap_stream_decoder.decode_bitmap_stream_to_rgb24_observed(
                        bitmap.bitmap_data,
                        &mut decoded,
                        sw,
                        sh,
                    ) {
                        Ok(Some(shape)) if shape == (sw, sh) => {}
                        Ok(_) => {
                            return Err(custom_err!("RDP6 完整源形状", KernelError::InvalidDimensions));
                        }
                        Err(e) => {
                            warn!("Invalid RDP6_BITMAP_STREAM: {e}");
                            region =
                                Some(region.map_or_else(|| bitmap.rectangle.clone(), |r| r.union(&bitmap.rectangle)));
                            continue;
                        }
                    }
                } else {
                    match ironrdp_graphics::rle::decompress_observed(
                        bitmap.bitmap_data,
                        &mut decoded,
                        sw,
                        sh,
                        usize::from(bitmap.bits_per_pixel),
                    ) {
                        Ok((_format, progress)) => {
                            if progress.written != expected {
                                return Err(custom_err!(
                                    "完整 portable RLE",
                                    ironrdp_graphics::rle::CompleteRleError::Incomplete {
                                        expected,
                                        written: progress.written,
                                        consumed: progress.consumed,
                                    }
                                ));
                            }
                        }
                        Err(e) => {
                            warn!("Invalid RLE-compressed bitmap: {e}");
                            region =
                                Some(region.map_or_else(|| bitmap.rectangle.clone(), |r| r.union(&bitmap.rectangle)));
                            continue;
                        }
                    }
                }
                decoded.as_slice()
            } else {
                bitmap.bitmap_data
            };
            coverage.reserve_rectangles(1)?;
            if let Some(damage) = image.apply_bitmap_compat(
                source,
                format,
                sw,
                sh,
                stride,
                true,
                &bitmap.rectangle,
                self.palette.colors(),
            )? {
                coverage.content([bitmap.rectangle]);
                region = Some(region.map_or_else(|| damage.clone(), |r| r.union(&damage)));
            }
        }
        Ok(vec![region.map_or(UpdateKind::None, UpdateKind::Region)])
    }

    /// Process a pointer update, shared between fast-path and slow-path pipelines.
    pub fn process_pointer_update(
        &mut self,
        image: &mut DecodedImage,
        update: PointerUpdateData<'_>,
    ) -> SessionResult<Vec<UpdateKind>> {
        self.process_pointer_update_collected(image, update, &mut CoverageCollector::default())
    }
    pub(crate) fn process_pointer_update_collected(
        &mut self,
        image: &mut DecodedImage,
        update: PointerUpdateData<'_>,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut processor_updates = Vec::new();

        if !self.enable_server_pointer {
            return Ok(processor_updates);
        }

        let bitmap_target = if self.pointer_software_rendering {
            PointerBitmapTarget::Software
        } else {
            PointerBitmapTarget::Accelerated
        };

        let server_position = matches!(&update, PointerUpdateData::SetPosition(_));
        match update {
            PointerUpdateData::SetHidden => {
                self.flush_native_mouse(image, &mut processor_updates, coverage)?;
                processor_updates.push(UpdateKind::PointerHidden);
                if self.pointer_software_rendering && !self.use_system_pointer {
                    self.use_system_pointer = true;
                    coverage.reserve_entries(
                        1 + processor_updates
                            .iter()
                            .filter(|u| matches!(u, UpdateKind::Region(_)))
                            .count(),
                    )?;
                    if let Some(rect) = image.hide_pointer_with_kernels(self.native_legacy_kernels.as_ref())? {
                        processor_updates.push(UpdateKind::Region(rect));
                    }
                }
            }
            PointerUpdateData::SetDefault => {
                self.flush_native_mouse(image, &mut processor_updates, coverage)?;
                processor_updates.push(UpdateKind::PointerDefault);
                if self.pointer_software_rendering && !self.use_system_pointer {
                    self.use_system_pointer = true;
                    coverage.reserve_entries(
                        1 + processor_updates
                            .iter()
                            .filter(|u| matches!(u, UpdateKind::Region(_)))
                            .count(),
                    )?;
                    if let Some(rect) = image.hide_pointer_with_kernels(self.native_legacy_kernels.as_ref())? {
                        processor_updates.push(UpdateKind::Region(rect));
                    }
                }
            }
            PointerUpdateData::SetPosition(position) => {
                if self.use_system_pointer || !self.pointer_software_rendering {
                    processor_updates.push(UpdateKind::PointerPosition {
                        x: position.x,
                        y: position.y,
                    });
                } else {
                    coverage.reserve_entries(
                        1 + processor_updates
                            .iter()
                            .filter(|u| matches!(u, UpdateKind::Region(_)))
                            .count(),
                    )?;
                    if let Some(rect) =
                        image.move_pointer_with_kernels(position.x, position.y, self.native_legacy_kernels.as_ref())?
                    {
                        processor_updates.push(UpdateKind::Region(rect));
                    }
                }
            }
            PointerUpdateData::Color(pointer) => {
                let cache_index = pointer.cache_index;

                let decoded_pointer = Arc::new(match self.native_legacy_kernels.as_ref() {
                    Some(kernels) => {
                        DecodedPointer::decode_color_pointer_attribute_native(&pointer, bitmap_target, kernels)
                            .map_err(|e| SessionError::custom("failed to decode color pointer attribute", e))?
                    }
                    None => DecodedPointer::decode_color_pointer_attribute(&pointer, bitmap_target)
                        .map_err(|e| SessionError::custom("failed to decode color pointer attribute", e))?,
                });

                self.flush_native_mouse(image, &mut processor_updates, coverage)?;
                let _ = self
                    .pointer_cache
                    .insert(usize::from(cache_index), Arc::clone(&decoded_pointer));

                if self.pointer_software_rendering {
                    coverage.reserve_entries(
                        1 + processor_updates
                            .iter()
                            .filter(|u| matches!(u, UpdateKind::Region(_)))
                            .count(),
                    )?;
                }
                if !self.pointer_software_rendering {
                    processor_updates.push(UpdateKind::PointerBitmap(Arc::clone(&decoded_pointer)));
                } else if let Some(rect) =
                    image.update_pointer_with_kernels(decoded_pointer, self.native_legacy_kernels.as_ref())?
                {
                    processor_updates.push(UpdateKind::Region(rect));
                }
            }
            PointerUpdateData::Cached(cached) => {
                let cache_index = cached.cache_index;

                if let Some(cached_pointer) = self.pointer_cache.get(usize::from(cache_index)) {
                    self.flush_native_mouse(image, &mut processor_updates, coverage)?;
                    // Disable system pointer
                    processor_updates.push(UpdateKind::PointerHidden);
                    self.use_system_pointer = false;
                    // Send graphics update
                    if self.pointer_software_rendering {
                        coverage.reserve_entries(
                            1 + processor_updates
                                .iter()
                                .filter(|u| matches!(u, UpdateKind::Region(_)))
                                .count(),
                        )?;
                    }
                    if !self.pointer_software_rendering {
                        processor_updates.push(UpdateKind::PointerBitmap(Arc::clone(&cached_pointer)));
                    } else if let Some(rect) =
                        image.update_pointer_with_kernels(cached_pointer, self.native_legacy_kernels.as_ref())?
                    {
                        processor_updates.push(UpdateKind::Region(rect));
                    } else {
                        // In case pointer was hidden previously
                        if let Some(rect) = image.show_pointer_with_kernels(self.native_legacy_kernels.as_ref())? {
                            processor_updates.push(UpdateKind::Region(rect));
                        }
                    }
                } else {
                    warn!("Cached pointer not found {}", cache_index);
                }
            }
            PointerUpdateData::New(pointer) => {
                let cache_index = pointer.color_pointer.cache_index;

                let decoded_pointer = Arc::new(match self.native_legacy_kernels.as_ref() {
                    Some(kernels) => DecodedPointer::decode_pointer_attribute_native(&pointer, bitmap_target, kernels)
                        .map_err(|e| SessionError::custom("failed to decode pointer attribute", e))?,
                    None => DecodedPointer::decode_pointer_attribute(&pointer, bitmap_target)
                        .map_err(|e| SessionError::custom("failed to decode pointer attribute", e))?,
                });

                self.flush_native_mouse(image, &mut processor_updates, coverage)?;
                let _ = self
                    .pointer_cache
                    .insert(usize::from(cache_index), Arc::clone(&decoded_pointer));

                if self.pointer_software_rendering {
                    coverage.reserve_entries(
                        1 + processor_updates
                            .iter()
                            .filter(|u| matches!(u, UpdateKind::Region(_)))
                            .count(),
                    )?;
                }
                if !self.pointer_software_rendering {
                    processor_updates.push(UpdateKind::PointerBitmap(Arc::clone(&decoded_pointer)));
                } else if let Some(rect) =
                    image.update_pointer_with_kernels(decoded_pointer, self.native_legacy_kernels.as_ref())?
                {
                    processor_updates.push(UpdateKind::Region(rect));
                }
            }
            PointerUpdateData::Large(pointer) => {
                let cache_index = pointer.cache_index;

                let decoded_pointer: Arc<DecodedPointer> = Arc::new(match self.native_legacy_kernels.as_ref() {
                    Some(kernels) => {
                        DecodedPointer::decode_large_pointer_attribute_native(&pointer, bitmap_target, kernels)
                            .map_err(|e| SessionError::custom("failed to decode large pointer attribute", e))?
                    }
                    None => DecodedPointer::decode_large_pointer_attribute(&pointer, bitmap_target)
                        .map_err(|e| SessionError::custom("failed to decode large pointer attribute", e))?,
                });

                self.flush_native_mouse(image, &mut processor_updates, coverage)?;
                let _ = self
                    .pointer_cache
                    .insert(usize::from(cache_index), Arc::clone(&decoded_pointer));

                if self.pointer_software_rendering {
                    coverage.reserve_entries(
                        1 + processor_updates
                            .iter()
                            .filter(|u| matches!(u, UpdateKind::Region(_)))
                            .count(),
                    )?;
                }
                if !self.pointer_software_rendering {
                    processor_updates.push(UpdateKind::PointerBitmap(Arc::clone(&decoded_pointer)));
                } else if let Some(rect) =
                    image.update_pointer_with_kernels(decoded_pointer, self.native_legacy_kernels.as_ref())?
                {
                    processor_updates.push(UpdateKind::Region(rect));
                }
            }
        };

        // 原顺序 local position 后 server position，server 必须赢；仅成功处理后消费。
        if server_position && self.native_legacy_kernels.is_some() {
            self.mouse_pos_update = None;
        }

        Ok(processor_updates)
    }

    #[allow(dead_code)] // 保留原私有入口供既有独立 harness 使用。
    fn process_surface_commands(
        &mut self,
        image: &mut DecodedImage,
        output: &mut WriteBuf,
        surface_commands: Vec<SurfaceCommand<'_>>,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        self.process_surface_commands_collected(image, output, surface_commands, &mut CoverageCollector::default())
    }

    fn process_surface_commands_collected(
        &mut self,
        image: &mut DecodedImage,
        output: &mut WriteBuf,
        surface_commands: Vec<SurfaceCommand<'_>>,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        coverage.reserve_entry()?;
        if coverage.enabled || self.native_legacy_kernels.is_some() {
            let kernels = self.native_legacy_kernels;
            return self.process_surface_commands_checked(image, output, surface_commands, kernels.as_ref(), coverage);
        }
        let mut update_rectangle = None;

        for command in surface_commands {
            match command {
                SurfaceCommand::SetSurfaceBits(bits) | SurfaceCommand::StreamSurfaceBits(bits) => {
                    let codec_id = CodecId::from_u8(bits.extended_bitmap_data.codec_id).ok_or_else(|| {
                        reason_err!(
                            "Fast-Path",
                            "unexpected codec ID: {:x}",
                            bits.extended_bitmap_data.codec_id
                        )
                    })?;

                    trace!(?codec_id, "Surface bits");

                    let destination = bits.destination;
                    // TODO(@pacmancoder): Correct rectangle conversion logic should
                    // be revisited when `rectangle_processing.rs` from
                    // `ironrdp-graphics` will be refactored to use generic `Rectangle`
                    // trait instead of hardcoded `InclusiveRectangle`.
                    let destination = InclusiveRectangle {
                        left: destination.left,
                        top: destination.top,
                        right: destination.right - 1,
                        bottom: destination.bottom - 1,
                    };
                    match codec_id {
                        CODEC_ID_NONE => {
                            let ext_data = bits.extended_bitmap_data;
                            let rectangle = match ext_data.bpp {
                                8 => {
                                    image.apply_rgb8_with_palette(ext_data.data, &destination, self.palette.colors())?
                                }
                                15 => image.apply_rgb15_bitmap(ext_data.data, &destination)?,
                                16 => image.apply_rgb16_bitmap(ext_data.data, &destination)?,
                                24 => image.apply_bgr24_bitmap(ext_data.data, &destination)?,
                                32 => image.apply_rgb32_bitmap(ext_data.data, PixelFormat::BgrX32, &destination)?,
                                bpp => {
                                    warn!("Unsupported surface CODEC_ID_NONE bpp: {bpp}");
                                    continue;
                                }
                            };
                            update_rectangle = update_rectangle
                                .map(|rect: InclusiveRectangle| rect.union(&rectangle))
                                .or(Some(rectangle));
                        }
                        CODEC_ID_REMOTEFX => {
                            let mut data = ReadCursor::new(bits.extended_bitmap_data.data);
                            while !data.is_empty() {
                                let (_frame_id, rectangle) = self.rfx_handler.decode(image, &destination, &mut data)?;
                                update_rectangle = update_rectangle
                                    .map(|rect: InclusiveRectangle| rect.union(&rectangle))
                                    .or(Some(rectangle));
                            }
                        }
                        #[cfg(feature = "qoi")]
                        ironrdp_pdu::rdp::capability_sets::CODEC_ID_QOI => {
                            qoi_apply(
                                image,
                                destination,
                                bits.extended_bitmap_data.data,
                                &mut update_rectangle,
                            )?;
                        }
                        #[cfg(feature = "qoiz")]
                        ironrdp_pdu::rdp::capability_sets::CODEC_ID_QOIZ => {
                            let compressed = &bits.extended_bitmap_data.data;
                            let mut input = zstd_safe::InBuffer::around(compressed);
                            let mut data = vec![0; compressed.len() * 4];
                            let mut pos = 0;
                            loop {
                                let mut output = zstd_safe::OutBuffer::around_pos(data.as_mut_slice(), pos);
                                self.zdctx
                                    .decompress_stream(&mut output, &mut input)
                                    .map_err(zstd_safe::get_error_name)
                                    .map_err(|e| reason_err!("zstd", "{}", e))?;
                                pos = output.pos();
                                if pos == output.capacity() {
                                    data.resize(data.capacity() * 2, 0);
                                } else {
                                    break;
                                }
                            }

                            qoi_apply(image, destination, &data, &mut update_rectangle)?;
                        }
                        _ => {
                            warn!("Unsupported codec ID: {}", bits.extended_bitmap_data.codec_id);
                        }
                    }
                }
                SurfaceCommand::FrameMarker(marker) => {
                    trace!(
                        "Frame marker: action {:?} with ID #{}",
                        marker.frame_action,
                        marker.frame_id.unwrap_or(0)
                    );
                    self.marker_processor.process(&marker, output)?;
                }
            }
        }

        Ok(Some(update_rectangle.unwrap_or_else(InclusiveRectangle::empty)))
    }
    fn process_surface_commands_checked(
        &mut self,
        image: &mut DecodedImage,
        output: &mut WriteBuf,
        commands: Vec<SurfaceCommand<'_>>,
        kernels: Option<&NativeKernels>,
        coverage: &mut CoverageCollector,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        let mut region: Option<InclusiveRectangle> = None;
        for command in commands {
            let (bits, set) = match command {
                SurfaceCommand::SetSurfaceBits(bits) => (bits, true),
                SurfaceCommand::StreamSurfaceBits(bits) => (bits, false),
                SurfaceCommand::FrameMarker(marker) => {
                    self.marker_processor.process(&marker, output)?;
                    continue;
                }
            };
            let ext = bits.extended_bitmap_data;
            let codec = CodecId::from_u8(ext.codec_id)
                .ok_or_else(|| reason_err!("native Surface", "未知 codec ID：{}", ext.codec_id))?;
            let destination = native_surface_rectangle(&bits.destination, ext.width, ext.height, set)?;
            let current = match codec {
                CODEC_ID_NONE => {
                    let (format, bytes) = native_bitmap_format(u16::from(ext.bpp), false)?;
                    let stride = usize::from(ext.width)
                        .checked_mul(bytes)
                        .ok_or_else(|| custom_err!("native Surface geometry", KernelError::InvalidDimensions))?;
                    // 固定 baseline 兼容约定：bottom-up tight BGR/BgrX，不按长度猜 padded 或 top-down。
                    coverage.reserve_rectangles(1)?;
                    let current = if let Some(kernels) = kernels {
                        image.apply_bitmap_native(
                            ext.data,
                            format,
                            usize::from(ext.width),
                            usize::from(ext.height),
                            stride,
                            true,
                            &destination,
                            Some(self.palette.colors()),
                            kernels,
                        )?
                    } else {
                        image.apply_bitmap_compat(
                            ext.data,
                            format,
                            usize::from(ext.width),
                            usize::from(ext.height),
                            stride,
                            true,
                            &destination,
                            self.palette.colors(),
                        )?
                    };
                    if current.is_some() {
                        coverage.content([destination.clone()]);
                    }
                    current
                }
                CODEC_ID_REMOTEFX => {
                    let mut data = ReadCursor::new(ext.data);
                    let mut frame_region: Option<InclusiveRectangle> = None;
                    while !data.is_empty() {
                        let (_, current) =
                            self.rfx_handler
                                .decode_region_collected(image, &destination, &mut data, coverage)?;
                        if let Some(current) = current {
                            frame_region = Some(frame_region.map_or_else(|| current.clone(), |r| r.union(&current)));
                        }
                    }
                    frame_region
                }
                #[cfg(feature = "qoi")]
                ironrdp_pdu::rdp::capability_sets::CODEC_ID_QOI => {
                    coverage.reserve_rectangles(1)?;
                    let current =
                        qoi_apply_checked(image, destination.clone(), ext.data, ext.width, ext.height, kernels)?;
                    if current.is_some() {
                        coverage.content([destination.clone()]);
                    }
                    current
                }
                #[cfg(feature = "qoiz")]
                ironrdp_pdu::rdp::capability_sets::CODEC_ID_QOIZ => {
                    let data = decompress_qoiz_native(
                        ext.data,
                        &mut self.zdctx,
                        ironrdp_graphics::rle::MAX_NATIVE_BITMAP_BYTES,
                    )?;
                    {
                        coverage.reserve_rectangles(1)?;
                        let current =
                            qoi_apply_checked(image, destination.clone(), &data, ext.width, ext.height, kernels)?;
                        if current.is_some() {
                            coverage.content([destination.clone()]);
                        }
                        current
                    }
                }
                _ => {
                    warn!("Unsupported codec ID: {}", ext.codec_id);
                    None
                }
            };
            if let Some(current) = current {
                region = Some(region.map_or_else(|| current.clone(), |prior| prior.union(&current)));
            }
        }
        // 默认约定仍能产生 empty sentinel damage；receipt 为空，不能推断为原点内容。
        Ok(if kernels.is_some() {
            region
        } else {
            Some(region.unwrap_or_else(InclusiveRectangle::empty))
        })
    }
}

fn native_bitmap_format(bpp: u16, compressed: bool) -> SessionResult<(PixelRowSource, usize)> {
    Ok(match bpp {
        8 => (PixelRowSource::Packed(PackedDepth::Index8), 1),
        15 => (PixelRowSource::Packed(PackedDepth::Rgb555), 2),
        16 => (PixelRowSource::Packed(PackedDepth::Rgb565), 2),
        24 => (PixelRowSource::Packed(PackedDepth::Bgr24), 3),
        32 if compressed => (PixelRowSource::Rgb24, 3),
        32 => (PixelRowSource::Raw32(Pixel32Format::BgrX), 4),
        _ => {
            return Err(custom_err!(
                "native Bitmap depth",
                ironrdp_graphics::rle::NativeRleError::Rle(ironrdp_graphics::rle::RleError::InvalidBpp {
                    bpp: usize::from(bpp)
                })
            ));
        }
    })
}
fn native_surface_rectangle(
    incoming: &ironrdp_pdu::geometry::ExclusiveRectangle,
    width: u16,
    height: u16,
    set: bool,
) -> SessionResult<InclusiveRectangle> {
    if width == 0 || height == 0 {
        return Err(custom_err!("native Surface geometry", KernelError::InvalidDimensions));
    }
    let (right, bottom) = if set {
        // Set 的 right/bottom SHOULD 忽略；不先对这些字段做减一或逆序检查。
        (
            incoming.left.checked_add(width - 1),
            incoming.top.checked_add(height - 1),
        )
    } else {
        if incoming.right <= incoming.left
            || incoming.bottom <= incoming.top
            || incoming.right - incoming.left > width
            || incoming.bottom - incoming.top > height
        {
            return Err(custom_err!("native Stream geometry", KernelError::InvalidDimensions));
        }
        (incoming.right.checked_sub(1), incoming.bottom.checked_sub(1))
    };
    Ok(InclusiveRectangle {
        left: incoming.left,
        top: incoming.top,
        right: right.ok_or_else(|| custom_err!("native Surface geometry", KernelError::InvalidDimensions))?,
        bottom: bottom.ok_or_else(|| custom_err!("native Surface geometry", KernelError::InvalidDimensions))?,
    })
}
#[cfg(all(test, feature = "qoi"))]
std::thread_local! {
    static OPTIONAL_STAGING_CALLS: std::cell::RefCell<Vec<(&'static str, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(feature = "qoi")]
fn checked_native_staging_size(required: usize, limit: usize) -> SessionResult<usize> {
    // 内部测试可用更小限额，任何调用都不能扩大统一256MiB/isize资源边界。
    if required == 0
        || required > isize::MAX as usize
        || required > limit.min(ironrdp_graphics::rle::MAX_NATIVE_BITMAP_BYTES)
    {
        return Err(custom_err!(
            "native codec resource limit",
            KernelError::InvalidDimensions
        ));
    }
    Ok(required)
}
#[cfg(feature = "qoi")]
fn checked_qoi_pixel_size(width: usize, height: usize, channels: usize) -> SessionResult<usize> {
    if width == 0 || height == 0 || !matches!(channels, 3 | 4) {
        return Err(custom_err!("native QOI geometry", KernelError::InvalidDimensions));
    }
    let required = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(channels))
        .ok_or_else(|| custom_err!("native QOI geometry", KernelError::InvalidDimensions))?;
    checked_native_staging_size(required, ironrdp_graphics::rle::MAX_NATIVE_BITMAP_BYTES)
}
#[cfg(feature = "qoiz")]
const NATIVE_QOIZ_INITIAL_BYTES: usize = 16 * 1024;
#[cfg(feature = "qoiz")]
fn resize_qoiz_staging(data: &mut Vec<u8>, required: usize, limit: usize) -> SessionResult<()> {
    let required = checked_native_staging_size(required, limit)?;
    #[cfg(test)]
    OPTIONAL_STAGING_CALLS.with(|v| v.borrow_mut().push(("qoiz_allocation", required)));
    data.try_reserve_exact(required.saturating_sub(data.len()))
        .map_err(|e| custom_err!("native QOIZ allocation", e))?;
    data.resize(required, 0);
    Ok(())
}
#[cfg(feature = "qoiz")]
fn decompress_qoiz_native(
    compressed: &[u8],
    context: &mut zstd_safe::DCtx<'_>,
    limit: usize,
) -> SessionResult<Vec<u8>> {
    // 源压缩长度是借用长度，不作为首次私有staging容量。生产limit固定统一资源门。
    let mut input = zstd_safe::InBuffer::around(compressed);
    let mut data = Vec::new();
    resize_qoiz_staging(&mut data, NATIVE_QOIZ_INITIAL_BYTES.min(limit), limit)?;
    let mut pos = 0;
    loop {
        let mut out = zstd_safe::OutBuffer::around_pos(data.as_mut_slice(), pos);
        context
            .decompress_stream(&mut out, &mut input)
            .map_err(zstd_safe::get_error_name)
            .map_err(|e| reason_err!("zstd", "{}", e))?;
        pos = out.pos();
        if pos < out.capacity() {
            break;
        }
        let next = data
            .len()
            .checked_mul(2)
            .ok_or_else(|| custom_err!("native QOIZ resource limit", KernelError::InvalidDimensions))?;
        resize_qoiz_staging(&mut data, next, limit)?;
    }
    data.truncate(pos);
    Ok(data)
}
#[cfg(feature = "qoi")]
#[allow(dead_code)] // 既有可选 wire harness 的同签名入口。
fn qoi_apply_native(
    image: &mut DecodedImage,
    destination: InclusiveRectangle,
    data: &[u8],
    declared_width: u16,
    declared_height: u16,
    kernels: &NativeKernels,
) -> SessionResult<Option<InclusiveRectangle>> {
    qoi_apply_checked(image, destination, data, declared_width, declared_height, Some(kernels))
}
#[cfg(feature = "qoi")]
fn qoi_apply_checked(
    image: &mut DecodedImage,
    destination: InclusiveRectangle,
    data: &[u8],
    declared_width: u16,
    declared_height: u16,
    kernels: Option<&NativeKernels>,
) -> SessionResult<Option<InclusiveRectangle>> {
    // 同一个decoder只解析一次header；库的decode_to_vec不能承担本地分配前资源门。
    let mut decoder = qoi::Decoder::new(data).map_err(|e| custom_err!("native QOI header", e))?;
    #[cfg(test)]
    OPTIONAL_STAGING_CALLS.with(|v| v.borrow_mut().push(("qoi_header", 0)));
    let header = *decoder.header();
    let (format, bytes) = match header.channels {
        qoi::Channels::Rgb => (PixelRowSource::Rgb24, 3),
        qoi::Channels::Rgba => (PixelRowSource::Raw32(Pixel32Format::RgbA), 4),
    };
    let width = usize::try_from(header.width)
        .map_err(|_| custom_err!("native QOI geometry", KernelError::InvalidDimensions))?;
    let height = usize::try_from(header.height)
        .map_err(|_| custom_err!("native QOI geometry", KernelError::InvalidDimensions))?;
    let size = checked_qoi_pixel_size(width, height, bytes)?;
    if width != usize::from(declared_width) || height != usize::from(declared_height) {
        return Err(custom_err!(
            "native QOI declared source shape",
            KernelError::InvalidDimensions
        ));
    }
    let stride = width
        .checked_mul(bytes)
        .ok_or_else(|| custom_err!("native QOI geometry", KernelError::InvalidDimensions))?;
    image.native_bitmap_size(format, width, height, stride, &destination)?;
    let mut decoded = Vec::new();
    #[cfg(test)]
    OPTIONAL_STAGING_CALLS.with(|v| v.borrow_mut().push(("qoi_allocation", size)));
    decoded
        .try_reserve_exact(size)
        .map_err(|e| custom_err!("native QOI allocation", e))?;
    decoded.resize(size, 0);
    #[cfg(test)]
    OPTIONAL_STAGING_CALLS.with(|v| v.borrow_mut().push(("qoi_decode", size)));
    let written = decoder
        .decode_to_buf(decoded.as_mut_slice())
        .map_err(|e| custom_err!("native QOI decode", e))?;
    if written != size {
        return Err(custom_err!("native QOI decoded size", KernelError::Length));
    }
    #[cfg(test)]
    OPTIONAL_STAGING_CALLS.with(|v| v.borrow_mut().push(("qoi_apply", size)));
    if let Some(kernels) = kernels {
        image.apply_bitmap_native(
            &decoded,
            format,
            width,
            height,
            stride,
            false,
            &destination,
            None,
            kernels,
        )
    } else {
        image.apply_bitmap_compat(
            &decoded,
            format,
            width,
            height,
            stride,
            false,
            &destination,
            &[[0; 3]; 256],
        )
    }
}

#[cfg(feature = "qoi")]
fn qoi_apply(
    image: &mut DecodedImage,
    destination: InclusiveRectangle,
    data: &[u8],
    update_rectangle: &mut Option<InclusiveRectangle>,
) -> SessionResult<()> {
    let (header, decoded) = qoi::decode_to_vec(data).map_err(|e| reason_err!("QOI decode", "{}", e))?;

    // Guard against a decoded buffer that doesn't match the destination
    // rectangle. `apply_rgb24`/`apply_rgba32` derive the row count from the
    // decoded length, and the only bounds check downstream (`rect_fits`)
    // validates the rectangle against the image, not the buffer against the
    // rectangle. A malformed/oversized QOI payload would otherwise drive the
    // per-row index past `self.data` and panic (client-side DoS).
    let channels = match header.channels {
        qoi::Channels::Rgb => 3,
        qoi::Channels::Rgba => 4,
    };
    let expected = usize::from(destination.width()) * usize::from(destination.height()) * channels;
    if decoded.len() != expected {
        return Err(reason_err!(
            "QOI decode",
            "decoded {} bytes, expected {} for {}x{} ({} channels)",
            decoded.len(),
            expected,
            destination.width(),
            destination.height(),
            channels
        ));
    }

    let rectangle = match header.channels {
        qoi::Channels::Rgb => image.apply_rgb24(&decoded, &destination, false)?,
        qoi::Channels::Rgba => image.apply_rgba32(&decoded, &destination, false)?,
    };

    *update_rectangle = update_rectangle
        .as_ref()
        .map(|rect: &InclusiveRectangle| rect.union(&rectangle))
        .or(Some(rectangle));
    Ok(())
}

pub struct ProcessorBuilder {
    pub io_channel_id: u16,
    pub user_channel_id: u16,
    pub share_id: u32,
    /// Ignore server pointer updates.
    pub enable_server_pointer: bool,
    /// Use software rendering mode for pointer bitmap generation. When this option is active,
    /// `UpdateKind::PointerBitmap` will not be generated. Remote pointer will be drawn
    /// via software rendering on top of the output image.
    pub pointer_software_rendering: bool,
    /// Bulk decompressor for server-to-client compressed PDUs.
    /// `None` when compression was not negotiated.
    pub bulk_decompressor: Option<BulkCompressor>,
}

impl ProcessorBuilder {
    pub fn build(self) -> Processor {
        Processor {
            complete_data: CompleteData::new(),
            rfx_handler: rfx::DecodingContext::new(),
            marker_processor: FrameMarkerProcessor::new(self.user_channel_id, self.io_channel_id, self.share_id),
            bitmap_stream_decoder: BitmapStreamDecoder::default(),
            native_legacy_kernels: None,
            pointer_cache: PointerCache::default(),
            use_system_pointer: true,
            mouse_pos_update: None,
            enable_server_pointer: self.enable_server_pointer,
            pointer_software_rendering: self.pointer_software_rendering,
            bulk_decompressor: self.bulk_decompressor,
            palette: Palette::system_default(),
            #[cfg(feature = "qoiz")]
            zdctx: zstd_safe::DCtx::default(),
        }
    }
}

#[derive(Debug, PartialEq)]
struct CompleteData {
    fragmented_data: Option<Vec<u8>>,
}

impl CompleteData {
    fn new() -> Self {
        Self { fragmented_data: None }
    }

    fn process_data(&mut self, data: &[u8], fragmentation: Fragmentation) -> Option<Vec<u8>> {
        match fragmentation {
            Fragmentation::Single => {
                self.check_data_is_empty();

                Some(data.to_vec())
            }
            Fragmentation::First => {
                self.check_data_is_empty();

                self.fragmented_data = Some(data.to_vec());

                None
            }
            Fragmentation::Next => {
                self.append_data(data);

                None
            }
            Fragmentation::Last => {
                self.append_data(data);

                self.fragmented_data.take()
            }
        }
    }

    fn check_data_is_empty(&mut self) {
        if self.fragmented_data.is_some() {
            warn!("Skipping pending Fast-Path Update internal multiple elements data");
            self.fragmented_data = None;
        }
    }

    fn append_data(&mut self, data: &[u8]) {
        if let Some(fragmented_data) = self.fragmented_data.as_mut() {
            fragmented_data.extend_from_slice(data);
        } else {
            warn!("Got unexpected Next fragmentation PDU without prior First fragmentation PDU");
        }
    }
}

struct FrameMarkerProcessor {
    user_channel_id: u16,
    io_channel_id: u16,
    share_id: u32,
}

impl FrameMarkerProcessor {
    fn new(user_channel_id: u16, io_channel_id: u16, share_id: u32) -> Self {
        Self {
            user_channel_id,
            io_channel_id,
            share_id,
        }
    }

    fn process(&mut self, marker: &FrameMarkerPdu, output: &mut WriteBuf) -> SessionResult<()> {
        match marker.frame_action {
            FrameAction::Begin => Ok(()),
            FrameAction::End => {
                ironrdp_pdu::rdp::headers::encode_share_data(
                    self.user_channel_id,
                    self.io_channel_id,
                    self.share_id,
                    ShareDataPdu::FrameAcknowledge(FrameAcknowledgePdu {
                        frame_id: marker.frame_id.unwrap_or(0),
                    }),
                    output,
                )
                .map_err(SessionError::encode)?;

                Ok(())
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/cursor_consumers.rs"]
mod cursor_consumers;
