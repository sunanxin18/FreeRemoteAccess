use std::ffi::OsStr;
use std::fmt;

#[cfg(test)]
use std::cell::RefCell;

use crate::hevc_access_unit::HevcAccessUnitError;
use crate::high_performance_video::AppleHighPerformanceVideoError;

const HP_MEDIA_DIAGNOSTICS_ENV: &str = "FRD_APPLE_HP_MEDIA_DIAGNOSTICS";
pub(crate) const VIDEO_RTP_PARSE_FATAL_CATEGORY: &str = "video_rtp_parse";

#[cfg(test)]
struct TestDiagnosticObserver {
    enabled: bool,
    lines: Vec<String>,
}

#[cfg(test)]
thread_local! {
    static TEST_DIAGNOSTIC_OBSERVER: RefCell<Option<TestDiagnosticObserver>> = const {
        RefCell::new(None)
    };
}

pub(crate) fn diagnostics_enabled_from(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

fn diagnostics_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = TEST_DIAGNOSTIC_OBSERVER
        .with(|observer| observer.borrow().as_ref().map(|observer| observer.enabled))
    {
        return enabled;
    }
    diagnostics_enabled_from(std::env::var_os(HP_MEDIA_DIAGNOSTICS_ENV).as_deref())
}

pub(crate) struct HpMediaFatalDiagnostic {
    category: &'static str,
}

impl HpMediaFatalDiagnostic {
    pub(crate) const fn new(category: &'static str) -> Self {
        Self { category }
    }
}

impl fmt::Display for HpMediaFatalDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "[apple-hp-media-fatal] category={}",
            self.category
        )
    }
}

pub(crate) fn emit_hp_media_fatal(category: &'static str) {
    if diagnostics_enabled() {
        let diagnostic = HpMediaFatalDiagnostic::new(category);
        #[cfg(test)]
        if TEST_DIAGNOSTIC_OBSERVER.with(|observer| {
            let mut observer = observer.borrow_mut();
            let Some(observer) = observer.as_mut() else {
                return false;
            };
            observer.lines.push(diagnostic.to_string());
            true
        }) {
            return;
        }
        eprintln!("{diagnostic}");
    }
}

#[cfg(test)]
pub(crate) fn with_test_diagnostic_observer<T>(
    enabled: bool,
    operation: impl FnOnce() -> T,
) -> (T, Vec<String>) {
    TEST_DIAGNOSTIC_OBSERVER.with(|observer| {
        let previous = observer.replace(Some(TestDiagnosticObserver {
            enabled,
            lines: Vec::new(),
        }));
        assert!(previous.is_none(), "测试诊断观察器不得嵌套");
        let result = operation();
        let observed = observer
            .replace(None)
            .expect("测试诊断观察器应保持安装状态");
        (result, observed.lines)
    })
}

pub(crate) const fn hevc_fatal_category(error: &HevcAccessUnitError) -> &'static str {
    match error {
        HevcAccessUnitError::InvalidReorderWindow { .. } => "hevc_invalid_reorder_window",
        HevcAccessUnitError::StaleGeneration { .. } => "hevc_stale_generation",
        HevcAccessUnitError::SsrcChanged { .. } => "hevc_ssrc_changed",
        HevcAccessUnitError::TimestampChangedBeforeMarker { .. } => {
            "hevc_timestamp_changed_before_marker"
        }
        HevcAccessUnitError::ReorderWindowExceeded { .. } => "hevc_reorder_window_exceeded",
        HevcAccessUnitError::ReorderPacketBudgetExceeded { .. } => {
            "hevc_reorder_packet_budget_exceeded"
        }
        HevcAccessUnitError::ReorderByteBudgetExceeded { .. } => {
            "hevc_reorder_byte_budget_exceeded"
        }
        HevcAccessUnitError::AccessUnitBudgetExceeded { .. } => "hevc_access_unit_budget_exceeded",
        HevcAccessUnitError::MarkerBeforeFuCompletion => "hevc_marker_before_fu_completion",
        HevcAccessUnitError::MissingInitialParameterSets => "hevc_missing_initial_parameter_sets",
        HevcAccessUnitError::MalformedLengthPrefixedAccessUnit => {
            "hevc_malformed_length_prefixed_access_unit"
        }
        HevcAccessUnitError::EmptyNalUnit => "hevc_empty_nal_unit",
        HevcAccessUnitError::Depacketize(_) => "hevc_depacketize",
    }
}

pub(crate) const fn adapter_fatal_category(error: &AppleHighPerformanceVideoError) -> &'static str {
    match error {
        AppleHighPerformanceVideoError::StaleGeneration => "adapter_stale_generation",
        AppleHighPerformanceVideoError::MalformedAccessUnit => "adapter_malformed_access_unit",
        AppleHighPerformanceVideoError::UnsupportedStreamConfig => {
            "adapter_unsupported_stream_config"
        }
        AppleHighPerformanceVideoError::InvalidStreamConfig => "adapter_invalid_stream_config",
        AppleHighPerformanceVideoError::StreamConfigChangedWithinGeneration => {
            "adapter_stream_config_changed"
        }
        AppleHighPerformanceVideoError::MediaPublicationFailed(
            frd_media_api::MediaPublishError::Full,
        ) => "video_queue_full",
        AppleHighPerformanceVideoError::MediaPublicationFailed(
            frd_media_api::MediaPublishError::Closed,
        ) => "video_worker_closed",
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use frd_media_api::MediaPublishError;

    use crate::hevc_access_unit::HevcAccessUnitError;
    use crate::hevc_rtp::HevcRtpError;
    use crate::high_performance_video::AppleHighPerformanceVideoError;

    use super::{
        adapter_fatal_category, diagnostics_enabled_from, hevc_fatal_category,
        HpMediaFatalDiagnostic, VIDEO_RTP_PARSE_FATAL_CATEGORY,
    };

    #[test]
    fn diagnostics_switch_is_exact_and_defaults_off() {
        assert!(!diagnostics_enabled_from(None));
        assert!(diagnostics_enabled_from(Some(OsStr::new("1"))));
        for disabled in ["", "0", "true", "TRUE", " 1", "1 "] {
            assert!(!diagnostics_enabled_from(Some(OsStr::new(disabled))));
        }
    }

    #[test]
    fn hevc_fatal_categories_are_closed_and_have_no_dynamic_error_fields() {
        let cases = [
            (
                HevcAccessUnitError::InvalidReorderWindow {
                    actual: 0,
                    maximum: 256,
                },
                "hevc_invalid_reorder_window",
            ),
            (
                HevcAccessUnitError::StaleGeneration {
                    expected: 1,
                    actual: 99,
                },
                "hevc_stale_generation",
            ),
            (
                HevcAccessUnitError::SsrcChanged {
                    previous: 0x1111_1111,
                    actual: 0x2222_2222,
                },
                "hevc_ssrc_changed",
            ),
            (
                HevcAccessUnitError::TimestampChangedBeforeMarker {
                    previous: 123,
                    actual: 456,
                },
                "hevc_timestamp_changed_before_marker",
            ),
            (
                HevcAccessUnitError::ReorderWindowExceeded { limit: 256 },
                "hevc_reorder_window_exceeded",
            ),
            (
                HevcAccessUnitError::ReorderPacketBudgetExceeded { limit: 256 },
                "hevc_reorder_packet_budget_exceeded",
            ),
            (
                HevcAccessUnitError::ReorderByteBudgetExceeded { limit: 1024 },
                "hevc_reorder_byte_budget_exceeded",
            ),
            (
                HevcAccessUnitError::AccessUnitBudgetExceeded { limit: 1024 },
                "hevc_access_unit_budget_exceeded",
            ),
            (
                HevcAccessUnitError::MarkerBeforeFuCompletion,
                "hevc_marker_before_fu_completion",
            ),
            (
                HevcAccessUnitError::MissingInitialParameterSets,
                "hevc_missing_initial_parameter_sets",
            ),
            (
                HevcAccessUnitError::MalformedLengthPrefixedAccessUnit,
                "hevc_malformed_length_prefixed_access_unit",
            ),
            (HevcAccessUnitError::EmptyNalUnit, "hevc_empty_nal_unit"),
            (
                HevcAccessUnitError::Depacketize(HevcRtpError::Truncated),
                "hevc_depacketize",
            ),
        ];

        for (error, expected) in cases {
            let category = hevc_fatal_category(&error);
            assert_eq!(category, expected);
            let line = HpMediaFatalDiagnostic::new(category).to_string();
            assert_eq!(line, format!("[apple-hp-media-fatal] category={expected}"));
            assert!(!line.contains("123"));
            assert!(!line.contains("456"));
            assert!(!line.contains("0x11111111"));
        }
    }

    #[test]
    fn adapter_and_publication_categories_are_closed() {
        let adapter_cases = [
            (
                AppleHighPerformanceVideoError::StaleGeneration,
                "adapter_stale_generation",
            ),
            (
                AppleHighPerformanceVideoError::MalformedAccessUnit,
                "adapter_malformed_access_unit",
            ),
            (
                AppleHighPerformanceVideoError::UnsupportedStreamConfig,
                "adapter_unsupported_stream_config",
            ),
            (
                AppleHighPerformanceVideoError::InvalidStreamConfig,
                "adapter_invalid_stream_config",
            ),
            (
                AppleHighPerformanceVideoError::StreamConfigChangedWithinGeneration,
                "adapter_stream_config_changed",
            ),
            (
                AppleHighPerformanceVideoError::MediaPublicationFailed(MediaPublishError::Full),
                "video_queue_full",
            ),
            (
                AppleHighPerformanceVideoError::MediaPublicationFailed(MediaPublishError::Closed),
                "video_worker_closed",
            ),
        ];

        for (error, expected) in adapter_cases {
            assert_eq!(adapter_fatal_category(&error), expected);
            assert_eq!(
                HpMediaFatalDiagnostic::new(expected).to_string(),
                format!("[apple-hp-media-fatal] category={expected}")
            );
        }
    }

    #[test]
    fn video_rtp_parse_category_is_static_and_closed() {
        assert_eq!(VIDEO_RTP_PARSE_FATAL_CATEGORY, "video_rtp_parse");
        assert_eq!(
            HpMediaFatalDiagnostic::new(VIDEO_RTP_PARSE_FATAL_CATEGORY).to_string(),
            "[apple-hp-media-fatal] category=video_rtp_parse"
        );
    }
}
