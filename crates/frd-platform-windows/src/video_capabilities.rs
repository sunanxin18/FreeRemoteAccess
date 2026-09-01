use frd_media_api::{
    ChromaFormat, VideoBackendAvailability, VideoBackendId, VideoBackendKind,
    VideoCapabilityProvider, VideoCodec, VideoDecodeCapability, VideoDecodeQuery,
    VideoDecodeSupport, VideoPixelFormat, VideoProfile, VideoUnsupportedReason,
};

const HEVC_MAIN_GUID: u128 = 0x5b11d51b_2f4c_4452_bcc3_09f2a1160cc0;
const HEVC_MAIN10_GUID: u128 = 0x107af0e0_ef1a_4d19_aba8_67a163073d13;
const HEVC_MAIN_444_GUID: u128 = 0x4008018f_f537_4b36_98cf_61af8a2c1a33;
const WINDOWS_VIDEO_BACKEND_ID: &str = "windows-d3d12-video-probe";

pub struct WindowsVideoCapabilityProvider {
    adapters: Box<[WindowsVideoAdapter]>,
}

pub struct WindowsVideoAdapter {
    luid_hex: Box<str>,
    description: Box<str>,
    profile_guids: Box<[u128]>,
    errors: Box<[WindowsVideoProbeErrorCode]>,
    checker: Box<dyn DecodeFeatureChecker>,
}

trait DecodeFeatureChecker: Send + Sync {
    fn check(&self, check: NativeDecodeCheck) -> Result<bool, ()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsVideoProbeErrorCode {
    UnsupportedPlatform,
    DxgiFactoryUnavailable,
    AdapterUnavailable,
    AdapterDescriptionUnavailable,
    D3d12DeviceUnavailable,
    VideoDeviceUnavailable,
    ProfileCountUnavailable,
    ProfileEnumerationUnavailable,
    FeatureCheckFailed,
}

impl VideoCapabilityProvider for WindowsVideoCapabilityProvider {
    fn backend_id(&self) -> VideoBackendId {
        VideoBackendId::new(WINDOWS_VIDEO_BACKEND_ID)
    }

    fn backend_kind(&self) -> VideoBackendKind {
        VideoBackendKind::Native
    }

    fn availability(&self) -> VideoBackendAvailability {
        VideoBackendAvailability::ProbeOnly
    }

    fn query(&self, query: &VideoDecodeQuery) -> VideoDecodeSupport {
        if self.adapters.is_empty() {
            return VideoDecodeSupport::Unsupported(VideoUnsupportedReason::BackendUnavailable);
        }

        let mut strongest_reason = VideoUnsupportedReason::ProfileUnavailable;
        for adapter in &self.adapters {
            if adapter_has_capability_probe_failure(&adapter.errors) {
                strongest_reason = VideoUnsupportedReason::BackendUnavailable;
                continue;
            }
            let support = evaluate_adapter_query(
                &adapter.luid_hex,
                &adapter.description,
                &adapter.profile_guids,
                query,
                |check| adapter.checker.check(check),
            );
            if support.is_exact() {
                return support;
            }
            if let VideoDecodeSupport::Unsupported(reason) = support {
                if unsupported_reason_rank(reason) > unsupported_reason_rank(strongest_reason) {
                    strongest_reason = reason;
                }
            }
        }
        VideoDecodeSupport::Unsupported(strongest_reason)
    }
}

fn adapter_has_capability_probe_failure(errors: &[WindowsVideoProbeErrorCode]) -> bool {
    errors
        .iter()
        .any(|error| *error != WindowsVideoProbeErrorCode::AdapterDescriptionUnavailable)
}

impl WindowsVideoCapabilityProvider {
    pub fn probe() -> Result<Self, WindowsVideoProbeErrorCode> {
        #[cfg(windows)]
        {
            windows_probe::probe()
        }
        #[cfg(not(windows))]
        {
            Err(WindowsVideoProbeErrorCode::UnsupportedPlatform)
        }
    }

    pub fn adapters(&self) -> &[WindowsVideoAdapter] {
        &self.adapters
    }
}

impl WindowsVideoAdapter {
    pub fn luid_hex(&self) -> &str {
        &self.luid_hex
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn profile_guids_hex(&self) -> Box<[Box<str>]> {
        self.profile_guids
            .iter()
            .map(|guid| format!("0x{guid:032x}").into_boxed_str())
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    pub fn errors(&self) -> &[WindowsVideoProbeErrorCode] {
        &self.errors
    }
}

impl WindowsVideoProbeErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::DxgiFactoryUnavailable => "dxgi_factory_unavailable",
            Self::AdapterUnavailable => "adapter_unavailable",
            Self::AdapterDescriptionUnavailable => "adapter_description_unavailable",
            Self::D3d12DeviceUnavailable => "d3d12_device_unavailable",
            Self::VideoDeviceUnavailable => "video_device_unavailable",
            Self::ProfileCountUnavailable => "profile_count_unavailable",
            Self::ProfileEnumerationUnavailable => "profile_enumeration_unavailable",
            Self::FeatureCheckFailed => "feature_check_failed",
        }
    }
}

fn unsupported_reason_rank(reason: VideoUnsupportedReason) -> u8 {
    match reason {
        VideoUnsupportedReason::ProfileUnavailable => 0,
        VideoUnsupportedReason::CodecUnavailable => 1,
        VideoUnsupportedReason::ChromaUnavailable => 2,
        VideoUnsupportedReason::BitDepthUnavailable => 3,
        VideoUnsupportedReason::OutputFormatUnavailable => 4,
        VideoUnsupportedReason::DimensionsUnavailable => 5,
        VideoUnsupportedReason::BackendUnavailable => 6,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProfileIdentity {
    codec: VideoCodec,
    profile: VideoProfile,
    chroma: ChromaFormat,
    bit_depth: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeDecodeFormat {
    Nv12,
    P010,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeDecodeCheck {
    profile_guid: u128,
    format: NativeDecodeFormat,
    width: u32,
    height: u32,
    frame_rate_numerator: u32,
    frame_rate_denominator: u32,
}

fn profile_identity_from_guid(guid: u128) -> Option<ProfileIdentity> {
    match guid {
        HEVC_MAIN_GUID => Some(ProfileIdentity {
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain,
            chroma: ChromaFormat::Yuv420,
            bit_depth: 8,
        }),
        HEVC_MAIN10_GUID => Some(ProfileIdentity {
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain10,
            chroma: ChromaFormat::Yuv420,
            bit_depth: 10,
        }),
        HEVC_MAIN_444_GUID => Some(ProfileIdentity {
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain4448,
            chroma: ChromaFormat::Yuv444,
            bit_depth: 8,
        }),
        _ => None,
    }
}

fn select_profile_guid(
    _adapter_description: &str,
    available_profile_guids: &[u128],
    query: &VideoDecodeQuery,
) -> Result<u128, VideoUnsupportedReason> {
    let matching_profile = available_profile_guids
        .iter()
        .copied()
        .filter_map(|guid| profile_identity_from_guid(guid).map(|identity| (guid, identity)))
        .filter(|(_, identity)| identity.codec == query.codec && identity.profile == query.profile)
        .collect::<Vec<_>>();
    if matching_profile.is_empty() {
        return Err(VideoUnsupportedReason::ProfileUnavailable);
    }
    let matching_chroma = matching_profile
        .iter()
        .copied()
        .filter(|(_, identity)| identity.chroma == query.chroma)
        .collect::<Vec<_>>();
    if matching_chroma.is_empty() {
        return Err(VideoUnsupportedReason::ChromaUnavailable);
    }
    matching_chroma
        .into_iter()
        .find(|(_, identity)| identity.bit_depth == query.bit_depth)
        .map(|(guid, _)| guid)
        .ok_or(VideoUnsupportedReason::BitDepthUnavailable)
}

fn evaluate_adapter_query(
    _adapter_id: &str,
    adapter_description: &str,
    available_profile_guids: &[u128],
    query: &VideoDecodeQuery,
    mut check_feature_support: impl FnMut(NativeDecodeCheck) -> Result<bool, ()>,
) -> VideoDecodeSupport {
    let profile_guid =
        match select_profile_guid(adapter_description, available_profile_guids, query) {
            Ok(profile_guid) => profile_guid,
            Err(reason) => return VideoDecodeSupport::Unsupported(reason),
        };
    let Some(identity) = profile_identity_from_guid(profile_guid) else {
        return VideoDecodeSupport::Unsupported(VideoUnsupportedReason::ProfileUnavailable);
    };
    let (frame_rate_numerator, frame_rate_denominator) = query
        .frame_rate
        .map(|rate| (rate.numerator.get(), rate.denominator.get()))
        .unwrap_or((60, 1));

    let mut had_compatible_output = false;
    for output in query.preferred_outputs.iter().copied() {
        let format = match (identity.profile, output) {
            (VideoProfile::HevcMain, VideoPixelFormat::Nv12) => NativeDecodeFormat::Nv12,
            (VideoProfile::HevcMain10, VideoPixelFormat::P010) => NativeDecodeFormat::P010,
            _ => continue,
        };
        had_compatible_output = true;
        let check = NativeDecodeCheck {
            profile_guid,
            format,
            width: query.coded_size.width,
            height: query.coded_size.height,
            frame_rate_numerator,
            frame_rate_denominator,
        };
        match check_feature_support(check) {
            Ok(true) => {
                return VideoDecodeSupport::HardwareExact(VideoDecodeCapability {
                    backend_id: VideoBackendId::new(WINDOWS_VIDEO_BACKEND_ID),
                    codec: query.codec,
                    profile: query.profile,
                    chroma: query.chroma,
                    bit_depth: query.bit_depth,
                    max_coded_size: query.coded_size,
                    output_formats: vec![output].into_boxed_slice(),
                    requires_bitstream_conversion: false,
                });
            }
            Ok(false) => {}
            Err(()) => {
                return VideoDecodeSupport::Unsupported(VideoUnsupportedReason::BackendUnavailable)
            }
        }
    }

    if had_compatible_output {
        VideoDecodeSupport::Unsupported(VideoUnsupportedReason::DimensionsUnavailable)
    } else {
        VideoDecodeSupport::Unsupported(VideoUnsupportedReason::OutputFormatUnavailable)
    }
}

#[cfg(windows)]
mod windows_probe {
    use std::{ffi::c_void, mem::size_of};

    use windows::{
        core::{Interface, GUID},
        Win32::{
            Graphics::{
                Direct3D::D3D_FEATURE_LEVEL_11_0,
                Direct3D12::{D3D12CreateDevice, ID3D12Device},
                Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_RATIONAL},
                Dxgi::{
                    CreateDXGIFactory2, IDXGIAdapter4, IDXGIFactory6, DXGI_ADAPTER_FLAG3_SOFTWARE,
                    DXGI_CREATE_FACTORY_FLAGS, DXGI_ERROR_NOT_FOUND,
                    DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
                },
            },
            Media::MediaFoundation::{
                ID3D12VideoDevice, D3D12_BITSTREAM_ENCRYPTION_TYPE_NONE,
                D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILES,
                D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILE_COUNT,
                D3D12_FEATURE_DATA_VIDEO_DECODE_SUPPORT, D3D12_FEATURE_VIDEO_DECODE_PROFILES,
                D3D12_FEATURE_VIDEO_DECODE_PROFILE_COUNT, D3D12_FEATURE_VIDEO_DECODE_SUPPORT,
                D3D12_VIDEO_DECODE_CONFIGURATION, D3D12_VIDEO_DECODE_SUPPORT_FLAG_SUPPORTED,
                D3D12_VIDEO_FRAME_CODED_INTERLACE_TYPE_NONE,
            },
        },
    };

    use super::{
        DecodeFeatureChecker, NativeDecodeCheck, NativeDecodeFormat, WindowsVideoAdapter,
        WindowsVideoCapabilityProvider, WindowsVideoProbeErrorCode,
    };

    const MAX_DECODE_PROFILE_COUNT: u32 = 4_096;

    pub(super) fn probe() -> Result<WindowsVideoCapabilityProvider, WindowsVideoProbeErrorCode> {
        let factory: IDXGIFactory6 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }
            .map_err(|_| WindowsVideoProbeErrorCode::DxgiFactoryUnavailable)?;
        let mut adapters = Vec::new();
        let mut index = 0u32;

        loop {
            let adapter = match unsafe {
                factory.EnumAdapterByGpuPreference::<IDXGIAdapter4>(
                    index,
                    DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
                )
            } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(_) => break,
            };
            index = index.saturating_add(1);

            let Ok(desc) = (unsafe { adapter.GetDesc3() }) else {
                continue;
            };
            if desc.Flags.0 & DXGI_ADAPTER_FLAG3_SOFTWARE.0 != 0 {
                continue;
            }

            let luid_hex = format!(
                "0x{:08x}{:08x}",
                desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart
            )
            .into_boxed_str();
            let (description, description_error) = sanitize_description(&desc.Description);
            let mut errors = Vec::new();
            if let Some(error) = description_error {
                errors.push(error);
            }

            let mut d3d12_device: Option<ID3D12Device> = None;
            if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut d3d12_device) }
                .is_err()
            {
                errors.push(WindowsVideoProbeErrorCode::D3d12DeviceUnavailable);
                adapters.push(unavailable_adapter(luid_hex, description, errors));
                continue;
            }
            let Some(d3d12_device) = d3d12_device else {
                errors.push(WindowsVideoProbeErrorCode::D3d12DeviceUnavailable);
                adapters.push(unavailable_adapter(luid_hex, description, errors));
                continue;
            };
            let video_device: ID3D12VideoDevice = match d3d12_device.cast() {
                Ok(device) => device,
                Err(_) => {
                    errors.push(WindowsVideoProbeErrorCode::VideoDeviceUnavailable);
                    adapters.push(unavailable_adapter(luid_hex, description, errors));
                    continue;
                }
            };

            let profile_guids = match enumerate_profiles(&video_device) {
                Ok(profiles) => profiles,
                Err(error) => {
                    errors.push(error);
                    Box::default()
                }
            };
            adapters.push(WindowsVideoAdapter {
                luid_hex,
                description,
                profile_guids,
                errors: errors.into_boxed_slice(),
                checker: Box::new(D3d12FeatureChecker {
                    device: video_device,
                }),
            });
        }

        if adapters.is_empty() {
            return Err(WindowsVideoProbeErrorCode::AdapterUnavailable);
        }
        adapters.sort_by(|left, right| left.luid_hex.cmp(&right.luid_hex));
        Ok(WindowsVideoCapabilityProvider {
            adapters: adapters.into_boxed_slice(),
        })
    }

    fn enumerate_profiles(
        device: &ID3D12VideoDevice,
    ) -> Result<Box<[u128]>, WindowsVideoProbeErrorCode> {
        let mut count = D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILE_COUNT {
            NodeIndex: 0,
            ProfileCount: 0,
        };
        unsafe {
            device.CheckFeatureSupport(
                D3D12_FEATURE_VIDEO_DECODE_PROFILE_COUNT,
                (&mut count as *mut D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILE_COUNT).cast(),
                size_of::<D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILE_COUNT>() as u32,
            )
        }
        .map_err(|_| WindowsVideoProbeErrorCode::ProfileCountUnavailable)?;
        if count.ProfileCount > MAX_DECODE_PROFILE_COUNT {
            return Err(WindowsVideoProbeErrorCode::ProfileCountUnavailable);
        }
        if count.ProfileCount == 0 {
            return Ok(Box::default());
        }

        let mut profiles = vec![GUID::zeroed(); count.ProfileCount as usize];
        let mut data = D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILES {
            NodeIndex: 0,
            ProfileCount: count.ProfileCount,
            pProfiles: profiles.as_mut_ptr(),
        };
        unsafe {
            device.CheckFeatureSupport(
                D3D12_FEATURE_VIDEO_DECODE_PROFILES,
                (&mut data as *mut D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILES).cast(),
                size_of::<D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILES>() as u32,
            )
        }
        .map_err(|_| WindowsVideoProbeErrorCode::ProfileEnumerationUnavailable)?;
        let mut profile_guids = profiles
            .into_iter()
            .map(|guid| guid.to_u128())
            .collect::<Vec<_>>();
        profile_guids.sort_unstable();
        profile_guids.dedup();
        Ok(profile_guids.into_boxed_slice())
    }

    fn sanitize_description(
        description: &[u16; 128],
    ) -> (Box<str>, Option<WindowsVideoProbeErrorCode>) {
        let end = description
            .iter()
            .position(|code_unit| *code_unit == 0)
            .unwrap_or(description.len());
        let sanitized = String::from_utf16_lossy(&description[..end])
            .chars()
            .filter(|character| !character.is_control())
            .take(128)
            .collect::<String>();
        let sanitized = sanitized.trim();
        if sanitized.is_empty() {
            (
                "adapter_description_unavailable".into(),
                Some(WindowsVideoProbeErrorCode::AdapterDescriptionUnavailable),
            )
        } else {
            (sanitized.into(), None)
        }
    }

    fn unavailable_adapter(
        luid_hex: Box<str>,
        description: Box<str>,
        errors: Vec<WindowsVideoProbeErrorCode>,
    ) -> WindowsVideoAdapter {
        WindowsVideoAdapter {
            luid_hex,
            description,
            profile_guids: Box::default(),
            errors: errors.into_boxed_slice(),
            checker: Box::new(UnavailableFeatureChecker),
        }
    }

    struct UnavailableFeatureChecker;

    impl DecodeFeatureChecker for UnavailableFeatureChecker {
        fn check(&self, _check: NativeDecodeCheck) -> Result<bool, ()> {
            Err(())
        }
    }

    struct D3d12FeatureChecker {
        device: ID3D12VideoDevice,
    }

    impl DecodeFeatureChecker for D3d12FeatureChecker {
        fn check(&self, check: NativeDecodeCheck) -> Result<bool, ()> {
            let decode_format: DXGI_FORMAT = match check.format {
                NativeDecodeFormat::Nv12 => DXGI_FORMAT_NV12,
                NativeDecodeFormat::P010 => DXGI_FORMAT_P010,
            };
            let mut support = D3D12_FEATURE_DATA_VIDEO_DECODE_SUPPORT {
                NodeIndex: 0,
                Configuration: D3D12_VIDEO_DECODE_CONFIGURATION {
                    DecodeProfile: GUID::from_u128(check.profile_guid),
                    BitstreamEncryption: D3D12_BITSTREAM_ENCRYPTION_TYPE_NONE,
                    InterlaceType: D3D12_VIDEO_FRAME_CODED_INTERLACE_TYPE_NONE,
                },
                Width: check.width,
                Height: check.height,
                DecodeFormat: decode_format,
                FrameRate: DXGI_RATIONAL {
                    Numerator: check.frame_rate_numerator,
                    Denominator: check.frame_rate_denominator,
                },
                BitRate: 0,
                ..Default::default()
            };
            unsafe {
                self.device.CheckFeatureSupport(
                    D3D12_FEATURE_VIDEO_DECODE_SUPPORT,
                    (&mut support as *mut D3D12_FEATURE_DATA_VIDEO_DECODE_SUPPORT).cast::<c_void>(),
                    size_of::<D3D12_FEATURE_DATA_VIDEO_DECODE_SUPPORT>() as u32,
                )
            }
            .map_err(|_| ())?;
            Ok(support
                .SupportFlags
                .contains(D3D12_VIDEO_DECODE_SUPPORT_FLAG_SUPPORTED))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use frd_core::PixelSize;
    use frd_media_api::{
        ChromaFormat, VideoBackendAvailability, VideoCapabilityProvider, VideoCodec,
        VideoDecodeQuery, VideoDecodeSupport, VideoPixelFormat, VideoProfile, VideoRational,
        VideoUnsupportedReason,
    };

    use super::{
        evaluate_adapter_query, profile_identity_from_guid, select_profile_guid,
        DecodeFeatureChecker, NativeDecodeCheck, NativeDecodeFormat, ProfileIdentity,
        WindowsVideoAdapter, WindowsVideoCapabilityProvider,
    };

    const H264_GUID: u128 = 0x1b81be68_a0c7_11d3_b984_00c04f2e73c5;
    const HEVC_MAIN_GUID: u128 = 0x5b11d51b_2f4c_4452_bcc3_09f2a1160cc0;
    const HEVC_MAIN10_GUID: u128 = 0x107af0e0_ef1a_4d19_aba8_67a163073d13;
    const HEVC_MAIN_444_GUID: u128 = 0x4008018f_f537_4b36_98cf_61af8a2c1a33;

    #[test]
    fn known_hevc_profile_guids_map_to_exact_main_and_main10_identities() {
        assert_eq!(
            profile_identity_from_guid(HEVC_MAIN_GUID),
            Some(ProfileIdentity {
                codec: VideoCodec::Hevc,
                profile: VideoProfile::HevcMain,
                chroma: ChromaFormat::Yuv420,
                bit_depth: 8,
            })
        );
        assert_eq!(
            profile_identity_from_guid(HEVC_MAIN10_GUID),
            Some(ProfileIdentity {
                codec: VideoCodec::Hevc,
                profile: VideoProfile::HevcMain10,
                chroma: ChromaFormat::Yuv420,
                bit_depth: 10,
            })
        );
    }

    #[test]
    fn unknown_and_generic_h264_guids_do_not_claim_an_exact_codec_profile() {
        assert_eq!(profile_identity_from_guid(0x1234), None);
        assert_eq!(profile_identity_from_guid(H264_GUID), None);
    }

    #[test]
    fn adapter_name_cannot_turn_main_or_unknown_profiles_into_main444() {
        let query = query(
            VideoProfile::HevcMain4448,
            ChromaFormat::Yuv444,
            8,
            VideoPixelFormat::Yuv444P8,
        );

        assert_eq!(
            select_profile_guid(
                "Example GPU HEVC Main444 Super Decoder",
                &[HEVC_MAIN_GUID, HEVC_MAIN10_GUID, 0x1234],
                &query,
            ),
            Err(VideoUnsupportedReason::ProfileUnavailable)
        );
    }

    #[test]
    fn main444_query_selects_only_the_genuine_windows_main444_profile() {
        let query = query(
            VideoProfile::HevcMain4448,
            ChromaFormat::Yuv444,
            8,
            VideoPixelFormat::Yuv444P8,
        );

        assert_eq!(
            select_profile_guid("Generic adapter", &[HEVC_MAIN_444_GUID], &query),
            Ok(HEVC_MAIN_444_GUID)
        );
    }

    #[test]
    fn packed_ayuv_is_not_claimed_as_exact_planar_yuv444_output() {
        let query = query(
            VideoProfile::HevcMain4448,
            ChromaFormat::Yuv444,
            8,
            VideoPixelFormat::Yuv444P8,
        );

        assert_eq!(
            evaluate_adapter_query("0x1", "Adapter", &[HEVC_MAIN_444_GUID], &query, |_| panic!(
                "packed AYUV 不得作为 planar YUV444 exact output 查询"
            ),),
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::OutputFormatUnavailable)
        );
    }

    #[test]
    fn exact_feature_check_receives_profile_format_size_and_frame_rate_from_query() {
        let mut query = query(
            VideoProfile::HevcMain10,
            ChromaFormat::Yuv420,
            10,
            VideoPixelFormat::P010,
        );
        query.coded_size = PixelSize::new(3840, 2160).expect("测试尺寸有效");
        query.frame_rate = Some(VideoRational {
            numerator: NonZeroU32::new(60_000).expect("测试帧率有效"),
            denominator: NonZeroU32::new(1_001).expect("测试帧率有效"),
        });
        let mut observed = None;

        let support = evaluate_adapter_query(
            "0x0000000000000001",
            "Adapter",
            &[HEVC_MAIN10_GUID],
            &query,
            |check| {
                observed = Some(check);
                Ok(true)
            },
        );

        assert!(matches!(support, VideoDecodeSupport::HardwareExact(_)));
        assert_eq!(
            observed,
            Some(NativeDecodeCheck {
                profile_guid: HEVC_MAIN10_GUID,
                format: NativeDecodeFormat::P010,
                width: 3840,
                height: 2160,
                frame_rate_numerator: 60_000,
                frame_rate_denominator: 1_001,
            })
        );
    }

    #[test]
    fn exact_feature_check_rejection_is_not_reported_as_hardware_support() {
        let query = query(
            VideoProfile::HevcMain,
            ChromaFormat::Yuv420,
            8,
            VideoPixelFormat::Nv12,
        );

        assert_eq!(
            evaluate_adapter_query(
                "0x0000000000000001",
                "Adapter",
                &[HEVC_MAIN_GUID],
                &query,
                |_| Ok(false),
            ),
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::DimensionsUnavailable)
        );
    }

    #[test]
    fn exact_query_reports_chroma_bit_depth_and_output_mismatches_separately() {
        let wrong_chroma = query(
            VideoProfile::HevcMain,
            ChromaFormat::Yuv444,
            8,
            VideoPixelFormat::Nv12,
        );
        let wrong_depth = query(
            VideoProfile::HevcMain10,
            ChromaFormat::Yuv420,
            8,
            VideoPixelFormat::P010,
        );
        let wrong_output = query(
            VideoProfile::HevcMain,
            ChromaFormat::Yuv420,
            8,
            VideoPixelFormat::Yuv420P8,
        );

        assert_eq!(
            evaluate_adapter_query(
                "0x1",
                "Adapter",
                &[HEVC_MAIN_GUID],
                &wrong_chroma,
                |_| panic!("色度不匹配时不得调用平台 API"),
            ),
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::ChromaUnavailable)
        );
        assert_eq!(
            evaluate_adapter_query(
                "0x1",
                "Adapter",
                &[HEVC_MAIN10_GUID],
                &wrong_depth,
                |_| panic!("位深不匹配时不得调用平台 API"),
            ),
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::BitDepthUnavailable)
        );
        assert_eq!(
            evaluate_adapter_query(
                "0x1",
                "Adapter",
                &[HEVC_MAIN_GUID],
                &wrong_output,
                |_| panic!("输出格式不匹配时不得调用平台 API"),
            ),
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::OutputFormatUnavailable)
        );
    }

    #[test]
    fn provider_is_probe_only_and_empty_probe_is_stably_unavailable() {
        let provider = WindowsVideoCapabilityProvider {
            adapters: Box::default(),
        };
        let query = query(
            VideoProfile::HevcMain,
            ChromaFormat::Yuv420,
            8,
            VideoPixelFormat::Nv12,
        );

        assert_eq!(provider.availability(), VideoBackendAvailability::ProbeOnly);
        assert_eq!(
            provider.query(&query),
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::BackendUnavailable)
        );
    }

    #[test]
    fn provider_requires_both_enumerated_profile_and_exact_feature_support() {
        let provider = WindowsVideoCapabilityProvider {
            adapters: vec![WindowsVideoAdapter {
                luid_hex: "0x0000000000000001".into(),
                description: "Adapter".into(),
                profile_guids: vec![HEVC_MAIN10_GUID].into_boxed_slice(),
                errors: Box::default(),
                checker: Box::new(FixedChecker(true)),
            }]
            .into_boxed_slice(),
        };
        let supported = query(
            VideoProfile::HevcMain10,
            ChromaFormat::Yuv420,
            10,
            VideoPixelFormat::P010,
        );
        let absent_profile = query(
            VideoProfile::HevcMain,
            ChromaFormat::Yuv420,
            8,
            VideoPixelFormat::Nv12,
        );

        assert!(matches!(
            provider.query(&supported),
            VideoDecodeSupport::HardwareExact(_)
        ));
        assert_eq!(
            provider.query(&absent_profile),
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::ProfileUnavailable)
        );
    }

    #[test]
    fn provider_does_not_turn_profile_enumeration_failure_into_profile_unavailable() {
        let provider = WindowsVideoCapabilityProvider {
            adapters: vec![WindowsVideoAdapter {
                luid_hex: "0x0000000000000001".into(),
                description: "Adapter".into(),
                profile_guids: Box::default(),
                errors: vec![super::WindowsVideoProbeErrorCode::ProfileEnumerationUnavailable]
                    .into_boxed_slice(),
                checker: Box::new(FixedChecker(true)),
            }]
            .into_boxed_slice(),
        };
        let query = query(
            VideoProfile::HevcMain,
            ChromaFormat::Yuv420,
            8,
            VideoPixelFormat::Nv12,
        );

        assert_eq!(
            provider.query(&query),
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::BackendUnavailable)
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_probe_returns_sanitized_metadata_or_a_stable_error() {
        match WindowsVideoCapabilityProvider::probe() {
            Ok(provider) => {
                assert_eq!(provider.availability(), VideoBackendAvailability::ProbeOnly);
                assert!(!provider.adapters().is_empty());
                for adapter in provider.adapters() {
                    let luid = adapter.luid_hex();
                    assert_eq!(luid.len(), 18);
                    assert!(luid.starts_with("0x"));
                    assert!(luid[2..].bytes().all(|byte| byte.is_ascii_hexdigit()));
                    assert!(!adapter.description().chars().any(char::is_control));
                    let profile_guids = adapter.profile_guids_hex();
                    assert!(profile_guids.windows(2).all(|pair| pair[0] <= pair[1]));
                    assert!(adapter
                        .errors()
                        .iter()
                        .all(|error| !error.as_str().is_empty()));
                }
            }
            Err(error) => assert!(matches!(
                error,
                super::WindowsVideoProbeErrorCode::DxgiFactoryUnavailable
                    | super::WindowsVideoProbeErrorCode::AdapterUnavailable
            )),
        }
    }

    struct FixedChecker(bool);

    impl DecodeFeatureChecker for FixedChecker {
        fn check(&self, _check: NativeDecodeCheck) -> Result<bool, ()> {
            Ok(self.0)
        }
    }

    fn query(
        profile: VideoProfile,
        chroma: ChromaFormat,
        bit_depth: u8,
        output: VideoPixelFormat,
    ) -> VideoDecodeQuery {
        VideoDecodeQuery {
            codec: VideoCodec::Hevc,
            profile,
            chroma,
            bit_depth,
            coded_size: PixelSize::new(1920, 1080).expect("测试尺寸有效"),
            frame_rate: None,
            preferred_outputs: vec![output].into_boxed_slice(),
        }
    }
}
