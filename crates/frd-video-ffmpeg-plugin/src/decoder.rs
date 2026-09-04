use frd_video_ffmpeg::abi::{FrdStatus, RawFrdFfmpegApiV1};

#[cfg(any(feature = "native-ffmpeg", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SoftwareDecodeThreadPolicy {
    thread_count: i32,
    thread_type: i32,
}

#[cfg(any(feature = "native-ffmpeg", test))]
impl SoftwareDecodeThreadPolicy {
    fn for_stream(coded_width: u32, coded_height: u32, logical_cpus: usize) -> Self {
        let long_edge = coded_width.max(coded_height);
        let short_edge = coded_width.min(coded_height);
        if long_edge >= 2560 && short_edge >= 1440 && logical_cpus >= 2 {
            Self {
                thread_count: 2,
                thread_type: 1,
            }
        } else {
            Self {
                thread_count: 1,
                thread_type: 0,
            }
        }
    }
}

pub(crate) fn populate_api(output: *mut RawFrdFfmpegApiV1, output_size: usize) -> FrdStatus {
    if output.is_null() || output_size < std::mem::size_of::<RawFrdFfmpegApiV1>() {
        return FrdStatus::INVALID_ARGUMENT;
    }

    #[cfg(feature = "native-ffmpeg")]
    {
        native::populate_api(output)
    }
    #[cfg(not(feature = "native-ffmpeg"))]
    {
        FrdStatus::UNSUPPORTED
    }
}

#[cfg(test)]
mod software_decode_thread_policy_tests {
    use super::SoftwareDecodeThreadPolicy;

    #[test]
    fn sub_1440p_decode_stays_single_threaded() {
        assert_eq!(
            SoftwareDecodeThreadPolicy::for_stream(1920, 1080, 8),
            SoftwareDecodeThreadPolicy {
                thread_count: 1,
                thread_type: 0,
            }
        );
        assert_eq!(
            SoftwareDecodeThreadPolicy::for_stream(2560, 1439, 8),
            SoftwareDecodeThreadPolicy {
                thread_count: 1,
                thread_type: 0,
            }
        );
        assert_eq!(
            SoftwareDecodeThreadPolicy::for_stream(1439, 2560, 2),
            SoftwareDecodeThreadPolicy {
                thread_count: 1,
                thread_type: 0,
            }
        );
    }

    #[test]
    fn at_least_1440p_decode_uses_exactly_two_frame_threads() {
        assert_eq!(
            SoftwareDecodeThreadPolicy::for_stream(2560, 1440, 64),
            SoftwareDecodeThreadPolicy {
                thread_count: 2,
                thread_type: 1,
            }
        );
        assert_eq!(
            SoftwareDecodeThreadPolicy::for_stream(3840, 2160, 64),
            SoftwareDecodeThreadPolicy {
                thread_count: 2,
                thread_type: 1,
            }
        );
        assert_eq!(
            SoftwareDecodeThreadPolicy::for_stream(1440, 2560, 2),
            SoftwareDecodeThreadPolicy {
                thread_count: 2,
                thread_type: 1,
            }
        );
    }

    #[test]
    fn single_logical_cpu_forces_one_decode_thread_at_1440p() {
        assert_eq!(
            SoftwareDecodeThreadPolicy::for_stream(2560, 1440, 1),
            SoftwareDecodeThreadPolicy {
                thread_count: 1,
                thread_type: 0,
            }
        );
    }
}

#[cfg(feature = "native-ffmpeg")]
mod native {
    use super::SoftwareDecodeThreadPolicy;
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::ptr;
    use std::slice;

    use frd_video_ffmpeg::abi::{
        FrdDecodedFrame, FrdDecodedPlane, FrdDecoderHandle, FrdOwnedBuffer, FrdStatus,
        FrdVideoConfig, RawFrdFfmpegApiV1, FRD_API_CONTRACT_REQUIRED, FRD_BITSTREAM_ANNEX_B,
        FRD_CHROMA_YUV_444, FRD_CODEC_HEVC, FRD_FFMPEG_ABI_VERSION, FRD_FFMPEG_API_V1_ALIGNMENT,
        FRD_FFMPEG_API_V1_SIZE, FRD_FFMPEG_AVCODEC_MAJOR, FRD_PIXEL_FORMAT_YUV_444_P8,
        FRD_PROFILE_HEVC_MAIN_444_8, FRD_SUBMIT_RANDOM_ACCESS,
    };

    const MAX_DIMENSION: u32 = 8192;
    const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
    const MAX_ACCESS_UNIT_BYTES: usize = 64 * 1024 * 1024;
    const MAX_PARAMETER_SET_BYTES: usize = 1024 * 1024;
    const MAX_QUEUED_FRAMES: usize = 8;

    const NATIVE_OK: i32 = 0;
    const NATIVE_AGAIN: i32 = 1;
    const NATIVE_EOF: i32 = 2;

    #[repr(C)]
    struct NativeDecoder {
        _private: [u8; 0],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NativeFrameView {
        format: i32,
        width: i32,
        height: i32,
        timestamp_ticks: i64,
        data: [*const u8; 3],
        linesize: [i32; 3],
    }

    unsafe extern "C" {
        fn frd_native_avcodec_major() -> u32;
        fn frd_native_hevc_decoder_available() -> i32;
        fn frd_native_yuv444p_format() -> i32;
        fn frd_native_decoder_create_with_thread_policy(
            extradata: *const u8,
            extradata_len: usize,
            width: i32,
            height: i32,
            timebase: u32,
            thread_count: i32,
            thread_type: i32,
            output: *mut *mut NativeDecoder,
        ) -> i32;
        fn frd_native_decoder_submit(
            decoder: *mut NativeDecoder,
            data: *const u8,
            len: usize,
            timestamp_ticks: i64,
            random_access: i32,
        ) -> i32;
        fn frd_native_decoder_receive(
            decoder: *mut NativeDecoder,
            output: *mut NativeFrameView,
        ) -> i32;
        fn frd_native_decoder_flush(decoder: *mut NativeDecoder) -> i32;
        fn frd_native_decoder_destroy(decoder: *mut NativeDecoder);
    }

    struct DecoderState {
        native: *mut NativeDecoder,
        width: u32,
        height: u32,
        queued: VecDeque<OwnedFrame>,
        queued_bytes: usize,
    }

    impl Drop for DecoderState {
        fn drop(&mut self) {
            // SAFETY: this pointer was created for this state and is destroyed exactly once.
            unsafe { frd_native_decoder_destroy(self.native) };
            self.native = ptr::null_mut();
        }
    }

    struct OwnedFrame {
        timestamp_ticks: i64,
        planes: [Box<[u8]>; 3],
    }

    impl OwnedFrame {
        fn byte_len(&self) -> usize {
            self.planes.iter().map(|plane| plane.len()).sum()
        }
    }

    pub(super) fn populate_api(output: *mut RawFrdFfmpegApiV1) -> FrdStatus {
        // SAFETY: read-only queries into libraries already resolved by the dynamic loader.
        let available = unsafe {
            frd_native_avcodec_major() == FRD_FFMPEG_AVCODEC_MAJOR
                && frd_native_hevc_decoder_available() != 0
        };
        if !available {
            return FrdStatus::UNSUPPORTED;
        }
        let api = RawFrdFfmpegApiV1 {
            struct_size: FRD_FFMPEG_API_V1_SIZE,
            struct_alignment: FRD_FFMPEG_API_V1_ALIGNMENT,
            abi_version: FRD_FFMPEG_ABI_VERSION,
            avcodec_major: FRD_FFMPEG_AVCODEC_MAJOR,
            contract_flags: FRD_API_CONTRACT_REQUIRED,
            reserved: 0,
            create_decoder: create_decoder as *const () as usize,
            submit: submit as *const () as usize,
            receive: receive as *const () as usize,
            flush: flush as *const () as usize,
            destroy: destroy as *const () as usize,
            reclaim_frame: reclaim_frame as *const () as usize,
        };
        // SAFETY: caller provided aligned writable storage and its size was validated above.
        unsafe { output.write(api) };
        FrdStatus::OK
    }

    unsafe extern "C" fn create_decoder(
        config: *const FrdVideoConfig,
        output: *mut FrdDecoderHandle,
    ) -> FrdStatus {
        if output.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        // SAFETY: checked writable output; null means no ownership transfer on later failure.
        unsafe { output.write(ptr::null_mut()) };
        let Some(config) = (unsafe { config.as_ref() }) else {
            return FrdStatus::INVALID_ARGUMENT;
        };
        if let Err(status) = validate_config(config) {
            return status;
        }
        let extradata = match annex_b_extradata(config) {
            Ok(extradata) => extradata,
            Err(status) => return status,
        };
        let mut native = ptr::null_mut();
        let logical_cpus = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        let thread_policy = SoftwareDecodeThreadPolicy::for_stream(
            config.coded_width,
            config.coded_height,
            logical_cpus,
        );
        // SAFETY: scalar bounds and the owned extradata slice were validated above.
        let status = unsafe {
            frd_native_decoder_create_with_thread_policy(
                extradata.as_ptr(),
                extradata.len(),
                config.coded_width as i32,
                config.coded_height as i32,
                config.timebase,
                thread_policy.thread_count,
                thread_policy.thread_type,
                &mut native,
            )
        };
        if status != NATIVE_OK || native.is_null() {
            if !native.is_null() {
                // SAFETY: a non-null native output is always owned even on error.
                unsafe { frd_native_decoder_destroy(native) };
            }
            return native_status(status);
        }
        let state = Box::new(DecoderState {
            native,
            width: config.coded_width,
            height: config.coded_height,
            queued: VecDeque::new(),
            queued_bytes: 0,
        });
        // SAFETY: transfers exclusive ownership to the host's matching destroy callback.
        unsafe { output.write(Box::into_raw(state).cast::<c_void>()) };
        FrdStatus::OK
    }

    unsafe extern "C" fn submit(
        handle: FrdDecoderHandle,
        data: *const u8,
        len: usize,
        timestamp_ticks: i64,
        flags: u32,
    ) -> FrdStatus {
        // SAFETY: the ABI requires one live, exclusively owned and serialized handle.
        let Some(state) = (unsafe { handle.cast::<DecoderState>().as_mut() }) else {
            return FrdStatus::INVALID_ARGUMENT;
        };
        if data.is_null()
            || len == 0
            || len > MAX_ACCESS_UNIT_BYTES
            || flags & !FRD_SUBMIT_RANDOM_ACCESS != 0
        {
            return FrdStatus::INVALID_ARGUMENT;
        }
        // SAFETY: input is readable for the call; native send copies it into an AVPacket.
        let mut status = unsafe {
            frd_native_decoder_submit(
                state.native,
                data,
                len,
                timestamp_ticks,
                i32::from(flags & FRD_SUBMIT_RANDOM_ACCESS != 0),
            )
        };
        if status == NATIVE_AGAIN {
            if let Err(error) = drain_available(state) {
                return error;
            }
            // SAFETY: EAGAIN did not consume the packet; retry once after draining output.
            status = unsafe {
                frd_native_decoder_submit(
                    state.native,
                    data,
                    len,
                    timestamp_ticks,
                    i32::from(flags & FRD_SUBMIT_RANDOM_ACCESS != 0),
                )
            };
        }
        native_status(status)
    }

    unsafe extern "C" fn receive(
        handle: FrdDecoderHandle,
        output: *mut FrdDecodedFrame,
    ) -> FrdStatus {
        if output.is_null() {
            return FrdStatus::INVALID_ARGUMENT;
        }
        // SAFETY: checked output; every return publishes reclaimable zero or owned buffers.
        unsafe { output.write(FrdDecodedFrame::default()) };
        // SAFETY: the ABI requires one live, exclusively owned and serialized handle.
        let Some(state) = (unsafe { handle.cast::<DecoderState>().as_mut() }) else {
            return FrdStatus::INVALID_ARGUMENT;
        };
        if let Some(frame) = state.queued.pop_front() {
            state.queued_bytes -= frame.byte_len();
            // SAFETY: output is valid and plane ownership transfers to reclaim.
            unsafe { publish_frame(frame, state.width, state.height, output) };
            return FrdStatus::OK;
        }

        match receive_native(state) {
            Ok(frame) => {
                // SAFETY: same validated output ownership transfer.
                unsafe { publish_frame(frame, state.width, state.height, output) };
                FrdStatus::OK
            }
            Err(status) => status,
        }
    }

    unsafe extern "C" fn flush(handle: FrdDecoderHandle) -> FrdStatus {
        // SAFETY: the ABI requires one live, exclusively owned and serialized handle.
        let Some(state) = (unsafe { handle.cast::<DecoderState>().as_mut() }) else {
            return FrdStatus::INVALID_ARGUMENT;
        };
        // SAFETY: exclusive serialized access to the native decoder.
        let mut status = unsafe { frd_native_decoder_flush(state.native) };
        if status == NATIVE_AGAIN {
            if let Err(error) = drain_available(state) {
                return error;
            }
            // SAFETY: retry the null packet once after draining pending output.
            status = unsafe { frd_native_decoder_flush(state.native) };
        }
        native_status(status)
    }

    unsafe extern "C" fn destroy(handle: FrdDecoderHandle) {
        if !handle.is_null() {
            // SAFETY: handle came from create and host calls destroy exactly once.
            drop(unsafe { Box::from_raw(handle.cast::<DecoderState>()) });
        }
    }

    unsafe extern "C" fn reclaim_frame(_handle: FrdDecoderHandle, frame: *mut FrdDecodedFrame) {
        let Some(frame) = (unsafe { frame.as_mut() }) else {
            return;
        };
        for plane in &mut frame.planes {
            if !plane.buffer.data.is_null() && plane.buffer.len != 0 {
                let allocation =
                    ptr::slice_from_raw_parts_mut(plane.buffer.data.cast_mut(), plane.buffer.len);
                // SAFETY: every published buffer came from exactly one Box<[u8]>.
                drop(unsafe { Box::from_raw(allocation) });
            }
        }
        *frame = FrdDecodedFrame::default();
    }

    fn validate_config(config: &FrdVideoConfig) -> Result<(), FrdStatus> {
        if config.codec != FRD_CODEC_HEVC
            || config.profile != FRD_PROFILE_HEVC_MAIN_444_8
            || config.chroma != FRD_CHROMA_YUV_444
            || config.bit_depth != 8
            || config.bitstream_format != FRD_BITSTREAM_ANNEX_B
        {
            return Err(FrdStatus::UNSUPPORTED);
        }
        if config.coded_width == 0
            || config.coded_height == 0
            || config.coded_width > MAX_DIMENSION
            || config.coded_height > MAX_DIMENSION
            || config.timebase == 0
            || config.timebase > i32::MAX as u32
        {
            return Err(FrdStatus::INVALID_ARGUMENT);
        }
        checked_frame_bytes(config.coded_width, config.coded_height)?;
        Ok(())
    }

    fn annex_b_extradata(config: &FrdVideoConfig) -> Result<Vec<u8>, FrdStatus> {
        let sets = [config.vps, config.sps, config.pps];
        let capacity = sets.iter().try_fold(0usize, |total, set| {
            if set.data.is_null() || set.len < 2 || set.len > MAX_PARAMETER_SET_BYTES {
                return Err(FrdStatus::INVALID_ARGUMENT);
            }
            total
                .checked_add(4)
                .and_then(|total| total.checked_add(set.len))
                .ok_or(FrdStatus::INVALID_ARGUMENT)
        })?;
        let mut output = Vec::with_capacity(capacity);
        for set in sets {
            output.extend_from_slice(&[0, 0, 0, 1]);
            // SAFETY: each input was validated above and is copied before create returns.
            output.extend_from_slice(unsafe { slice::from_raw_parts(set.data, set.len) });
        }
        Ok(output)
    }

    fn drain_available(state: &mut DecoderState) -> Result<(), FrdStatus> {
        loop {
            match receive_native(state) {
                Ok(frame) => {
                    let bytes = frame.byte_len();
                    state.queued_bytes =
                        checked_queue_total(state.queued.len(), state.queued_bytes, bytes)?;
                    state.queued.push_back(frame);
                }
                Err(status)
                    if status == FrdStatus::NEED_MORE_DATA
                        || status == FrdStatus::END_OF_STREAM =>
                {
                    return Ok(())
                }
                Err(status) => return Err(status),
            }
        }
    }

    fn receive_native(state: &mut DecoderState) -> Result<OwnedFrame, FrdStatus> {
        let mut view = NativeFrameView::default();
        // SAFETY: exclusive native handle and writable local view.
        let status = unsafe { frd_native_decoder_receive(state.native, &mut view) };
        if status != NATIVE_OK {
            return Err(native_status(status));
        }
        copy_frame(state, view)
    }

    fn copy_frame(state: &DecoderState, view: NativeFrameView) -> Result<OwnedFrame, FrdStatus> {
        // SAFETY: returns a constant from the exact headers/library used by the bridge.
        if view.format != unsafe { frd_native_yuv444p_format() }
            || view.width <= 0
            || view.height <= 0
            || view.width as u32 != state.width
            || view.height as u32 != state.height
        {
            return Err(FrdStatus::DECODE_FAILED);
        }
        let frame_bytes = checked_frame_bytes(state.width, state.height)?;
        let width = usize::try_from(state.width).map_err(|_| FrdStatus::DECODE_FAILED)?;
        let height = usize::try_from(state.height).map_err(|_| FrdStatus::DECODE_FAILED)?;
        let mut planes = Vec::with_capacity(3);
        for index in 0..3 {
            let stride =
                usize::try_from(view.linesize[index]).map_err(|_| FrdStatus::DECODE_FAILED)?;
            if view.data[index].is_null() || stride < width {
                return Err(FrdStatus::DECODE_FAILED);
            }
            let mut plane = vec![0u8; width * height].into_boxed_slice();
            for row in 0..height {
                // SAFETY: FFmpeg guarantees each non-negative linesize row is readable; width was
                // checked not to exceed it and the frame stays referenced until next receive.
                let source = unsafe { view.data[index].add(row * stride) };
                // SAFETY: source is readable for width bytes and destination row is valid.
                unsafe {
                    ptr::copy_nonoverlapping(source, plane.as_mut_ptr().add(row * width), width)
                };
            }
            planes.push(plane);
        }
        debug_assert_eq!(
            planes.iter().map(|plane| plane.len()).sum::<usize>(),
            frame_bytes
        );
        let planes: [Box<[u8]>; 3] = planes.try_into().map_err(|_| FrdStatus::DECODE_FAILED)?;
        Ok(OwnedFrame {
            timestamp_ticks: view.timestamp_ticks,
            planes,
        })
    }

    unsafe fn publish_frame(
        frame: OwnedFrame,
        width: u32,
        height: u32,
        output: *mut FrdDecodedFrame,
    ) {
        let mut planes = [FrdDecodedPlane::default(); 3];
        for (target, allocation) in planes.iter_mut().zip(frame.planes) {
            let len = allocation.len();
            let data = Box::into_raw(allocation).cast::<u8>();
            *target = FrdDecodedPlane {
                width,
                height,
                stride_bytes: width,
                buffer: FrdOwnedBuffer { data, len },
            };
        }
        let decoded = FrdDecodedFrame {
            timestamp_ticks: frame.timestamp_ticks,
            pixel_format: FRD_PIXEL_FORMAT_YUV_444_P8,
            plane_count: 3,
            planes,
        };
        // SAFETY: caller supplied checked writable output and now owns plane allocations.
        unsafe { output.write(decoded) };
    }

    fn checked_frame_bytes(width: u32, height: u32) -> Result<usize, FrdStatus> {
        let pixels = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(FrdStatus::INVALID_ARGUMENT)?;
        let bytes = pixels.checked_mul(3).ok_or(FrdStatus::INVALID_ARGUMENT)?;
        if bytes > MAX_FRAME_BYTES {
            return Err(FrdStatus::INVALID_ARGUMENT);
        }
        Ok(bytes)
    }

    fn checked_queue_total(
        queued_frames: usize,
        queued_bytes: usize,
        next_bytes: usize,
    ) -> Result<usize, FrdStatus> {
        if queued_frames >= MAX_QUEUED_FRAMES {
            return Err(FrdStatus::DECODE_FAILED);
        }
        queued_bytes
            .checked_add(next_bytes)
            .filter(|total| *total <= MAX_FRAME_BYTES)
            .ok_or(FrdStatus::DECODE_FAILED)
    }

    fn native_status(status: i32) -> FrdStatus {
        match status {
            NATIVE_OK => FrdStatus::OK,
            NATIVE_AGAIN => FrdStatus::NEED_MORE_DATA,
            NATIVE_EOF => FrdStatus::END_OF_STREAM,
            -1 => FrdStatus::UNSUPPORTED,
            -2 => FrdStatus::INVALID_ARGUMENT,
            _ => FrdStatus::DECODE_FAILED,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use frd_video_ffmpeg::abi::FrdByteSlice;

        #[repr(C)]
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
        struct NativeThreadSettings {
            requested_thread_count: i32,
            requested_thread_type: i32,
            active_thread_count: i32,
            active_thread_type: i32,
        }

        unsafe extern "C" {
            fn frd_native_decoder_thread_settings(
                decoder: *const NativeDecoder,
                output: *mut NativeThreadSettings,
            ) -> i32;
        }

        #[test]
        fn native_1440p_decoder_activates_requested_two_frame_threads() {
            let extradata = main444_parameter_sets();
            let policy = super::super::SoftwareDecodeThreadPolicy::for_stream(2560, 1440, 8);
            let mut decoder = ptr::null_mut();

            // SAFETY: fixture bytes and output storage remain valid for the call.
            let status = unsafe {
                frd_native_decoder_create_with_thread_policy(
                    extradata.as_ptr(),
                    extradata.len(),
                    2560,
                    1440,
                    90_000,
                    policy.thread_count,
                    policy.thread_type,
                    &mut decoder,
                )
            };
            assert_eq!(status, NATIVE_OK);
            assert!(!decoder.is_null());

            let mut settings = NativeThreadSettings::default();
            // SAFETY: decoder is live and output is writable for this read-only query.
            assert_eq!(
                unsafe { frd_native_decoder_thread_settings(decoder, &mut settings) },
                NATIVE_OK
            );
            assert_eq!(
                settings,
                NativeThreadSettings {
                    requested_thread_count: 2,
                    requested_thread_type: 1,
                    active_thread_count: 2,
                    active_thread_type: 1,
                }
            );

            // SAFETY: decoder was created successfully and is destroyed exactly once.
            unsafe { frd_native_decoder_destroy(decoder) };
        }

        #[test]
        fn negative_linesize_is_rejected_before_plane_copy() {
            let state = test_state(2, 2);
            let bytes = [0u8; 4];
            let view = NativeFrameView {
                format: yuv444p(),
                width: 2,
                height: 2,
                timestamp_ticks: 1,
                data: [bytes.as_ptr(); 3],
                linesize: [-2, 2, 2],
            };

            assert!(matches!(
                copy_frame(&state, view),
                Err(FrdStatus::DECODE_FAILED)
            ));
        }

        #[test]
        fn non_yuv444p_frame_is_rejected_before_plane_copy() {
            let state = test_state(2, 2);
            let bytes = [0u8; 4];
            let view = NativeFrameView {
                format: yuv444p() + 1,
                width: 2,
                height: 2,
                timestamp_ticks: 1,
                data: [bytes.as_ptr(); 3],
                linesize: [2; 3],
            };

            assert!(matches!(
                copy_frame(&state, view),
                Err(FrdStatus::DECODE_FAILED)
            ));
        }

        #[test]
        fn decoded_size_mismatch_is_rejected_before_plane_copy() {
            let state = test_state(2, 2);
            let bytes = [0u8; 6];
            let view = NativeFrameView {
                format: yuv444p(),
                width: 3,
                height: 2,
                timestamp_ticks: 1,
                data: [bytes.as_ptr(); 3],
                linesize: [3; 3],
            };

            assert!(matches!(
                copy_frame(&state, view),
                Err(FrdStatus::DECODE_FAILED)
            ));
        }

        #[test]
        fn dimensions_and_timebase_above_fixed_limits_are_rejected() {
            let mut config = FrdVideoConfig {
                codec: FRD_CODEC_HEVC,
                profile: FRD_PROFILE_HEVC_MAIN_444_8,
                chroma: FRD_CHROMA_YUV_444,
                bit_depth: 8,
                coded_width: MAX_DIMENSION + 1,
                coded_height: 1,
                timebase: 90_000,
                bitstream_format: FRD_BITSTREAM_ANNEX_B,
                vps: FrdByteSlice {
                    data: ptr::NonNull::<u8>::dangling().as_ptr(),
                    len: 2,
                },
                sps: FrdByteSlice {
                    data: ptr::NonNull::<u8>::dangling().as_ptr(),
                    len: 2,
                },
                pps: FrdByteSlice {
                    data: ptr::NonNull::<u8>::dangling().as_ptr(),
                    len: 2,
                },
            };

            assert_eq!(validate_config(&config), Err(FrdStatus::INVALID_ARGUMENT));
            config.coded_width = 1;
            config.timebase = i32::MAX as u32 + 1;
            assert_eq!(validate_config(&config), Err(FrdStatus::INVALID_ARGUMENT));
        }

        #[test]
        fn queued_output_is_bounded_by_count_and_aggregate_bytes() {
            assert_eq!(
                checked_queue_total(MAX_QUEUED_FRAMES, 0, 1),
                Err(FrdStatus::DECODE_FAILED)
            );
            assert_eq!(
                checked_queue_total(0, MAX_FRAME_BYTES, 1),
                Err(FrdStatus::DECODE_FAILED)
            );
            assert_eq!(
                checked_queue_total(0, MAX_FRAME_BYTES - 1, 1),
                Ok(MAX_FRAME_BYTES)
            );
        }

        #[test]
        fn reclaim_accepts_partial_plugin_owned_frame_and_clears_it() {
            let allocation = vec![7u8; 4].into_boxed_slice();
            let len = allocation.len();
            let data = Box::into_raw(allocation).cast::<u8>();
            let mut frame = FrdDecodedFrame {
                plane_count: 1,
                planes: [
                    FrdDecodedPlane {
                        width: 2,
                        height: 2,
                        stride_bytes: 2,
                        buffer: FrdOwnedBuffer { data, len },
                    },
                    FrdDecodedPlane::default(),
                    FrdDecodedPlane::default(),
                ],
                ..FrdDecodedFrame::default()
            };

            // SAFETY: the buffer uses the same plugin allocator and transfers exactly once.
            unsafe { reclaim_frame(ptr::null_mut(), &mut frame) };

            assert_eq!(frame.plane_count, 0);
            assert!(frame
                .planes
                .iter()
                .all(|plane| plane.buffer.data.is_null() && plane.buffer.len == 0));
        }

        fn test_state(width: u32, height: u32) -> DecoderState {
            DecoderState {
                native: ptr::null_mut(),
                width,
                height,
                queued: VecDeque::new(),
                queued_bytes: 0,
            }
        }

        fn yuv444p() -> i32 {
            // SAFETY: constant query has no mutable state or ownership.
            unsafe { frd_native_yuv444p_format() }
        }

        fn main444_parameter_sets() -> &'static [u8] {
            let bitstream = include_bytes!("../tests/fixtures/apple-main444-idr.hevc");
            let mut start_codes = Vec::new();
            for (offset, bytes) in bitstream.windows(4).enumerate() {
                if bytes == [0, 0, 0, 1] {
                    start_codes.push(offset);
                }
            }
            assert_eq!(start_codes.len(), 4, "Main444 fixture 应含四个 Annex-B NAL");
            &bitstream[..start_codes[3]]
        }
    }
}
