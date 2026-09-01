use std::num::NonZeroU32;

use frd_core::PixelSize;
use frd_media_api::{
    ChromaFormat, VideoCapabilityProvider, VideoCodec, VideoDecodeQuery, VideoDecodeSupport,
    VideoPixelFormat, VideoProfile, VideoRational, VideoUnsupportedReason,
};
use frd_platform_windows::WindowsVideoCapabilityProvider;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputMode {
    Human,
    Json,
}

struct NamedQuery {
    name: &'static str,
    query: VideoDecodeQuery,
}

fn parse_output_mode(args: &[&str]) -> Result<OutputMode, &'static str> {
    match args {
        [] => Ok(OutputMode::Human),
        ["--json"] => Ok(OutputMode::Json),
        _ => Err("invalid_arguments"),
    }
}

fn default_queries() -> Vec<NamedQuery> {
    vec![
        named_query(
            "h264_high_420_8",
            VideoCodec::H264,
            VideoProfile::H264High,
            ChromaFormat::Yuv420,
            8,
            VideoPixelFormat::Nv12,
        ),
        named_query(
            "hevc_main_420_8",
            VideoCodec::Hevc,
            VideoProfile::HevcMain,
            ChromaFormat::Yuv420,
            8,
            VideoPixelFormat::Nv12,
        ),
        named_query(
            "hevc_main10_420_10",
            VideoCodec::Hevc,
            VideoProfile::HevcMain10,
            ChromaFormat::Yuv420,
            10,
            VideoPixelFormat::P010,
        ),
        named_query(
            "hevc_main444_444_8",
            VideoCodec::Hevc,
            VideoProfile::HevcMain4448,
            ChromaFormat::Yuv444,
            8,
            VideoPixelFormat::Yuv444P8,
        ),
    ]
}

fn named_query(
    name: &'static str,
    codec: VideoCodec,
    profile: VideoProfile,
    chroma: ChromaFormat,
    bit_depth: u8,
    output: VideoPixelFormat,
) -> NamedQuery {
    NamedQuery {
        name,
        query: VideoDecodeQuery {
            codec,
            profile,
            chroma,
            bit_depth,
            coded_size: PixelSize::new(1920, 1080).expect("固定探针尺寸非零"),
            frame_rate: Some(VideoRational {
                numerator: NonZeroU32::new(60).expect("固定探针帧率非零"),
                denominator: NonZeroU32::new(1).expect("固定探针帧率分母非零"),
            }),
            preferred_outputs: vec![output].into_boxed_slice(),
        },
    }
}

fn support_label(support: &VideoDecodeSupport) -> &'static str {
    match support {
        VideoDecodeSupport::HardwareExact(_) => "hardware_exact",
        VideoDecodeSupport::SoftwareExact(_) => "software_exact",
        VideoDecodeSupport::Unsupported(reason) => match reason {
            VideoUnsupportedReason::BackendUnavailable => "backend_unavailable",
            VideoUnsupportedReason::CodecUnavailable => "codec_unavailable",
            VideoUnsupportedReason::ProfileUnavailable => "profile_unavailable",
            VideoUnsupportedReason::ChromaUnavailable => "chroma_unavailable",
            VideoUnsupportedReason::BitDepthUnavailable => "bit_depth_unavailable",
            VideoUnsupportedReason::DimensionsUnavailable => "dimensions_unavailable",
            VideoUnsupportedReason::OutputFormatUnavailable => "output_format_unavailable",
        },
    }
}

#[derive(Serialize)]
struct ProbeReport {
    schema: &'static str,
    backend_id: &'static str,
    availability: &'static str,
    probe_status: &'static str,
    error_code: Option<&'static str>,
    adapters: Vec<AdapterReport>,
    queries: Vec<QueryReport>,
}

#[derive(Serialize)]
struct AdapterReport {
    luid: String,
    description: String,
    profile_guids: Vec<String>,
    errors: Vec<&'static str>,
}

#[derive(Serialize)]
struct QueryReport {
    name: &'static str,
    codec: &'static str,
    profile: &'static str,
    chroma: &'static str,
    bit_depth: u8,
    coded_size: SizeReport,
    frame_rate: Option<RationalReport>,
    preferred_outputs: Vec<&'static str>,
    support: &'static str,
}

#[derive(Serialize)]
struct SizeReport {
    width: u32,
    height: u32,
}

#[derive(Serialize)]
struct RationalReport {
    numerator: u32,
    denominator: u32,
}

fn collect_report() -> ProbeReport {
    let named_queries = default_queries();
    match WindowsVideoCapabilityProvider::probe() {
        Ok(provider) => {
            let adapters = provider
                .adapters()
                .iter()
                .map(|adapter| AdapterReport {
                    luid: adapter.luid_hex().to_owned(),
                    description: adapter.description().to_owned(),
                    profile_guids: adapter
                        .profile_guids_hex()
                        .iter()
                        .map(|guid| guid.to_string())
                        .collect(),
                    errors: adapter
                        .errors()
                        .iter()
                        .map(|error| error.as_str())
                        .collect(),
                })
                .collect();
            let queries = named_queries
                .iter()
                .map(|named| query_report(named, &provider.query(&named.query)))
                .collect();
            ProbeReport {
                schema: "frd.windows_video_capabilities.v1",
                backend_id: "windows-d3d12-video-probe",
                availability: "probe_only",
                probe_status: "ok",
                error_code: None,
                adapters,
                queries,
            }
        }
        Err(error) => ProbeReport {
            schema: "frd.windows_video_capabilities.v1",
            backend_id: "windows-d3d12-video-probe",
            availability: "probe_only",
            probe_status: error.as_str(),
            error_code: Some(error.as_str()),
            adapters: Vec::new(),
            queries: named_queries
                .iter()
                .map(|named| {
                    query_report(
                        named,
                        &VideoDecodeSupport::Unsupported(
                            VideoUnsupportedReason::BackendUnavailable,
                        ),
                    )
                })
                .collect(),
        },
    }
}

fn query_report(named: &NamedQuery, support: &VideoDecodeSupport) -> QueryReport {
    QueryReport {
        name: named.name,
        codec: codec_label(named.query.codec),
        profile: profile_label(named.query.profile),
        chroma: chroma_label(named.query.chroma),
        bit_depth: named.query.bit_depth,
        coded_size: SizeReport {
            width: named.query.coded_size.width,
            height: named.query.coded_size.height,
        },
        frame_rate: named.query.frame_rate.map(|rate| RationalReport {
            numerator: rate.numerator.get(),
            denominator: rate.denominator.get(),
        }),
        preferred_outputs: named
            .query
            .preferred_outputs
            .iter()
            .copied()
            .map(output_label)
            .collect(),
        support: support_label(support),
    }
}

fn codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::Hevc => "hevc",
    }
}

fn profile_label(profile: VideoProfile) -> &'static str {
    match profile {
        VideoProfile::H264Baseline => "h264_baseline",
        VideoProfile::H264Main => "h264_main",
        VideoProfile::H264High => "h264_high",
        VideoProfile::HevcMain => "hevc_main",
        VideoProfile::HevcMain10 => "hevc_main10",
        VideoProfile::HevcMain4448 => "hevc_main444_8",
        VideoProfile::CodecSpecific { .. } => "codec_specific",
    }
}

fn chroma_label(chroma: ChromaFormat) -> &'static str {
    match chroma {
        ChromaFormat::Monochrome => "monochrome",
        ChromaFormat::Yuv420 => "yuv420",
        ChromaFormat::Yuv422 => "yuv422",
        ChromaFormat::Yuv444 => "yuv444",
    }
}

fn output_label(output: VideoPixelFormat) -> &'static str {
    match output {
        VideoPixelFormat::Yuv420P8 => "yuv420p8",
        VideoPixelFormat::Yuv444P8 => "yuv444p8",
        VideoPixelFormat::Nv12 => "nv12",
        VideoPixelFormat::P010 => "p010",
    }
}

fn render_json(report: &ProbeReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn render_human(report: &ProbeReport) -> String {
    let mut output = format!(
        "Windows D3D12 Video 能力探针（只读，{}）\n探针状态：{}\n适配器：{}\n",
        report.availability,
        report.probe_status,
        report.adapters.len()
    );
    for adapter in &report.adapters {
        output.push_str(&format!(
            "- {} [{}]，profiles={}，errors={}\n",
            adapter.description,
            adapter.luid,
            adapter.profile_guids.len(),
            if adapter.errors.is_empty() {
                "none".to_owned()
            } else {
                adapter.errors.join(",")
            }
        ));
    }
    output.push_str("查询结果：\n");
    for query in &report.queries {
        output.push_str(&format!("- {}: {}\n", query.name, query.support));
    }
    output
}

fn main() {
    let owned_args = std::env::args().skip(1).collect::<Vec<_>>();
    let args = owned_args.iter().map(String::as_str).collect::<Vec<_>>();
    let mode = match parse_output_mode(&args) {
        Ok(mode) => mode,
        Err(code) => {
            eprintln!("参数无效：{code}；仅支持无参数或 --json");
            std::process::exit(2);
        }
    };
    let report = collect_report();
    match mode {
        OutputMode::Human => print!("{}", render_human(&report)),
        OutputMode::Json => match render_json(&report) {
            Ok(json) => println!("{json}"),
            Err(_) => {
                eprintln!("JSON 输出失败：serialization_failed");
                std::process::exit(1);
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use frd_media_api::{
        ChromaFormat, VideoDecodeSupport, VideoPixelFormat, VideoProfile, VideoUnsupportedReason,
    };

    use super::{
        collect_report, default_queries, parse_output_mode, render_json, support_label, OutputMode,
    };

    #[test]
    fn default_queries_cover_exact_h264_main_main10_and_main444_contracts() {
        let queries = default_queries();

        assert_eq!(queries.len(), 4);
        assert_eq!(queries[0].name, "h264_high_420_8");
        assert_eq!(queries[0].query.profile, VideoProfile::H264High);
        assert_eq!(queries[1].query.profile, VideoProfile::HevcMain);
        assert_eq!(queries[2].query.profile, VideoProfile::HevcMain10);
        assert_eq!(queries[2].query.bit_depth, 10);
        assert_eq!(
            queries[2].query.preferred_outputs.as_ref(),
            &[VideoPixelFormat::P010]
        );
        assert_eq!(queries[3].query.profile, VideoProfile::HevcMain4448);
        assert_eq!(queries[3].query.chroma, ChromaFormat::Yuv444);
        assert_eq!(
            queries[3].query.preferred_outputs.as_ref(),
            &[VideoPixelFormat::Yuv444P8]
        );
    }

    #[test]
    fn command_line_accepts_only_no_argument_or_json() {
        assert_eq!(parse_output_mode(&[]), Ok(OutputMode::Human));
        assert_eq!(parse_output_mode(&["--json"]), Ok(OutputMode::Json));
        assert_eq!(
            parse_output_mode(&["host.example"]),
            Err("invalid_arguments")
        );
        assert_eq!(
            parse_output_mode(&["--json", "secret"]),
            Err("invalid_arguments")
        );
    }

    #[test]
    fn support_labels_are_stable_and_do_not_embed_platform_errors() {
        assert_eq!(
            support_label(&VideoDecodeSupport::Unsupported(
                VideoUnsupportedReason::ProfileUnavailable
            )),
            "profile_unavailable"
        );
        assert_eq!(
            support_label(&VideoDecodeSupport::Unsupported(
                VideoUnsupportedReason::BackendUnavailable
            )),
            "backend_unavailable"
        );
    }

    #[test]
    fn json_report_is_deterministic_structured_and_excludes_host_identity() {
        let report = collect_report();
        let first = render_json(&report).expect("JSON 序列化成功");
        let second = render_json(&report).expect("JSON 序列化成功");
        let value: serde_json::Value = serde_json::from_str(&first).expect("JSON 结构有效");

        assert_eq!(first, second);
        assert_eq!(
            value["schema"],
            serde_json::Value::String("frd.windows_video_capabilities.v1".into())
        );
        assert_eq!(
            value["availability"],
            serde_json::Value::String("probe_only".into())
        );
        assert!(value["adapters"].is_array());
        assert_eq!(value["queries"].as_array().map(Vec::len), Some(4));
        for variable in ["USERNAME", "COMPUTERNAME"] {
            if let Ok(secret) = std::env::var(variable) {
                if !secret.is_empty() {
                    assert!(!first.contains(&secret));
                }
            }
        }
    }
}
