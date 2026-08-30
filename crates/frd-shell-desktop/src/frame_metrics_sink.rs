use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use frd_render_wgpu::GpuFaultClass;

const METRIC_SCHEMA_VERSION: u16 = 1;
const MAX_METRIC_ROWS: usize = 16_384;
const METRIC_HEADER: &str = "schema_version,run_id,implementation,phase,event,batch_result,batch_failure_class,monotonic_us,session_id,generation,revision,source_updates,transactions,rectangles,batch_cpu_us,mailbox_age_us,scope_begins,scope_finishes,scope_polls,gpu_fault_code,process_cpu_total_us,process_cpu_delta_us,working_set_bytes,frame_response_ms,input_to_next_present_us";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SafeRunId(String);

impl SafeRunId {
    fn parse(value: OsString) -> Result<Self, MetricSinkError> {
        let value = value
            .into_string()
            .map_err(|_| MetricSinkError::InvalidConfiguration)?;
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(MetricSinkError::InvalidConfiguration);
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricSinkError {
    InvalidConfiguration,
    CreateFailed,
    WriteFailed,
    CapacityExceeded,
    InvalidObservation,
}

#[derive(Debug)]
enum MetricSinkConfiguration {
    Disabled,
    Enabled {
        path: PathBuf,
        run_id: SafeRunId,
        implementation: MetricImplementation,
    },
}

impl MetricSinkConfiguration {
    fn from_values(
        path: Option<OsString>,
        run_id: Option<OsString>,
        implementation: Option<OsString>,
    ) -> Result<Self, MetricSinkError> {
        let (path, run_id, implementation) = match (path, run_id, implementation) {
            (None, None, None) => return Ok(Self::Disabled),
            (Some(path), Some(run_id), Some(implementation)) => (path, run_id, implementation),
            _ => return Err(MetricSinkError::InvalidConfiguration),
        };
        let path_text = path
            .into_string()
            .map_err(|_| MetricSinkError::InvalidConfiguration)?;
        if path_text.is_empty() || is_device_namespace(&path_text) {
            return Err(MetricSinkError::InvalidConfiguration);
        }
        let path = PathBuf::from(path_text);
        validate_new_csv_path(&path)?;
        let run_id = SafeRunId::parse(run_id)?;
        let implementation = MetricImplementation::parse(implementation)?;
        Ok(Self::Enabled {
            path,
            run_id,
            implementation,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricImplementation {
    Serial,
    Candidate,
}

impl MetricImplementation {
    fn parse(value: OsString) -> Result<Self, MetricSinkError> {
        match value
            .into_string()
            .map_err(|_| MetricSinkError::InvalidConfiguration)?
            .as_str()
        {
            "serial" => Ok(Self::Serial),
            "candidate" => Ok(Self::Candidate),
            _ => Err(MetricSinkError::InvalidConfiguration),
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Serial => "serial",
            Self::Candidate => "candidate",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricPhase {
    VisibleWarmup,
    VisibleMeasurement,
    MinimizedWarmup,
    MinimizedMeasurement,
    Restore,
}

impl MetricPhase {
    fn code(self) -> &'static str {
        match self {
            Self::VisibleWarmup => "VisibleWarmup",
            Self::VisibleMeasurement => "VisibleMeasurement",
            Self::MinimizedWarmup => "MinimizedWarmup",
            Self::MinimizedMeasurement => "MinimizedMeasurement",
            Self::Restore => "Restore",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum MetricEventKind {
    PhaseBoundary,
    FrameResponse,
    SerialDrain,
    CandidateBatch,
    Presentation,
    InputToNextPresent,
    StableFault,
}

impl MetricEventKind {
    fn code(self) -> &'static str {
        match self {
            Self::PhaseBoundary => "PhaseBoundary",
            Self::FrameResponse => "FrameResponse",
            Self::SerialDrain => "SerialDrain",
            Self::CandidateBatch => "CandidateBatch",
            Self::Presentation => "Presentation",
            Self::InputToNextPresent => "InputToNextPresent",
            Self::StableFault => "StableFault",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum MetricBatchResult {
    Success,
    SerialFailure,
    CompileFailure,
    RendererFailure,
}

impl MetricBatchResult {
    fn code(self) -> &'static str {
        match self {
            Self::Success => "Success",
            Self::SerialFailure => "SerialFailure",
            Self::CompileFailure => "CompileFailure",
            Self::RendererFailure => "RendererFailure",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum MetricBatchFailureClass {
    Compiler,
    RendererPlanning,
    RendererExecution,
    Gpu,
}

impl MetricBatchFailureClass {
    fn code(self) -> &'static str {
        match self {
            Self::Compiler => "Compiler",
            Self::RendererPlanning => "RendererPlanning",
            Self::RendererExecution => "RendererExecution",
            Self::Gpu => "Gpu",
        }
    }
}

pub(crate) struct FrameMetricRow {
    pub(crate) run_id: SafeRunId,
    pub(crate) implementation: MetricImplementation,
    pub(crate) phase: MetricPhase,
    pub(crate) event: MetricEventKind,
    pub(crate) batch_result: Option<MetricBatchResult>,
    pub(crate) batch_failure_class: Option<MetricBatchFailureClass>,
    pub(crate) monotonic_us: u64,
    pub(crate) session_id: Option<u64>,
    pub(crate) generation: Option<u64>,
    pub(crate) revision: Option<u64>,
    pub(crate) source_updates: Option<u64>,
    pub(crate) transactions: Option<u64>,
    pub(crate) rectangles: Option<u64>,
    pub(crate) batch_cpu_us: Option<u64>,
    pub(crate) mailbox_age_us: Option<u64>,
    pub(crate) scope_begins: Option<u64>,
    pub(crate) scope_finishes: Option<u64>,
    pub(crate) scope_polls: Option<u64>,
    pub(crate) gpu_fault_code: Option<GpuFaultClass>,
    pub(crate) process_cpu_total_us: Option<u64>,
    pub(crate) process_cpu_delta_us: Option<u64>,
    pub(crate) working_set_bytes: Option<u64>,
    pub(crate) frame_response_ms: Option<u64>,
    pub(crate) input_to_next_present_us: Option<u64>,
}

pub(crate) struct FrameMetricsSink {
    writer: BufWriter<File>,
    run_id: SafeRunId,
    implementation: MetricImplementation,
    started_at: Instant,
    rows_written: usize,
    invalid: bool,
}

impl FrameMetricsSink {
    #[cfg(test)]
    pub(crate) fn open_for_test(
        path: PathBuf,
        started_at: Instant,
    ) -> Result<Self, MetricSinkError> {
        Self::open_enabled(
            path,
            SafeRunId("test_run".to_owned()),
            MetricImplementation::Serial,
            started_at,
        )
    }

    pub(crate) fn open_from_environment(
        started_at: Instant,
    ) -> Result<Option<Self>, MetricSinkError> {
        let configuration = MetricSinkConfiguration::from_values(
            std::env::var_os("FRD_FRAME_METRICS_PATH"),
            std::env::var_os("FRD_FRAME_METRICS_RUN_ID"),
            std::env::var_os("FRD_FRAME_METRICS_IMPLEMENTATION"),
        )?;
        match configuration {
            MetricSinkConfiguration::Disabled => Ok(None),
            MetricSinkConfiguration::Enabled {
                path,
                run_id,
                implementation,
            } => Self::open_enabled(path, run_id, implementation, started_at).map(Some),
        }
    }

    fn open_enabled(
        path: PathBuf,
        run_id: SafeRunId,
        implementation: MetricImplementation,
        started_at: Instant,
    ) -> Result<Self, MetricSinkError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| MetricSinkError::CreateFailed)?;
        let mut writer = BufWriter::new(file);
        writeln!(writer, "{METRIC_HEADER}").map_err(|_| MetricSinkError::WriteFailed)?;
        writer.flush().map_err(|_| MetricSinkError::WriteFailed)?;
        Ok(Self {
            writer,
            run_id,
            implementation,
            started_at,
            rows_written: 0,
            invalid: false,
        })
    }

    pub(crate) fn new_row(&self, phase: MetricPhase, event: MetricEventKind) -> FrameMetricRow {
        FrameMetricRow {
            run_id: self.run_id.clone(),
            implementation: self.implementation,
            phase,
            event,
            batch_result: None,
            batch_failure_class: None,
            monotonic_us: duration_us(self.started_at.elapsed()),
            session_id: None,
            generation: None,
            revision: None,
            source_updates: None,
            transactions: None,
            rectangles: None,
            batch_cpu_us: None,
            mailbox_age_us: None,
            scope_begins: None,
            scope_finishes: None,
            scope_polls: None,
            gpu_fault_code: None,
            process_cpu_total_us: None,
            process_cpu_delta_us: None,
            working_set_bytes: None,
            frame_response_ms: None,
            input_to_next_present_us: None,
        }
    }

    pub(crate) fn write_row(&mut self, row: FrameMetricRow) -> Result<(), MetricSinkError> {
        if self.invalid {
            return Err(MetricSinkError::InvalidObservation);
        }
        if self.rows_written >= MAX_METRIC_ROWS {
            self.invalid = true;
            return Err(MetricSinkError::CapacityExceeded);
        }
        if row.run_id != self.run_id
            || row.implementation != self.implementation
            || !valid_event_fields(&row)
        {
            self.invalid = true;
            return Err(MetricSinkError::InvalidObservation);
        }
        let fields = row_fields(&row);
        if writeln!(self.writer, "{}", fields.join(",")).is_err() {
            self.invalid = true;
            return Err(MetricSinkError::WriteFailed);
        }
        self.rows_written += 1;
        if row.event == MetricEventKind::PhaseBoundary {
            self.writer.flush().map_err(|_| {
                self.invalid = true;
                MetricSinkError::WriteFailed
            })?;
        }
        Ok(())
    }

    pub(crate) fn close(&mut self) -> Result<(), MetricSinkError> {
        self.writer.flush().map_err(|_| {
            self.invalid = true;
            MetricSinkError::WriteFailed
        })
    }
}

fn valid_event_fields(row: &FrameMetricRow) -> bool {
    match row.event {
        MetricEventKind::SerialDrain => {
            matches!(
                row.batch_result,
                Some(MetricBatchResult::Success | MetricBatchResult::SerialFailure)
            ) && (row.batch_result == Some(MetricBatchResult::Success))
                == row.batch_failure_class.is_none()
        }
        MetricEventKind::CandidateBatch => {
            row.batch_result == Some(MetricBatchResult::Success)
                && row.batch_failure_class.is_none()
        }
        MetricEventKind::StableFault => {
            matches!(
                row.batch_result,
                Some(
                    MetricBatchResult::SerialFailure
                        | MetricBatchResult::CompileFailure
                        | MetricBatchResult::RendererFailure
                )
            ) && row.batch_failure_class.is_some()
        }
        _ => row.batch_result.is_none() && row.batch_failure_class.is_none(),
    }
}

fn row_fields(row: &FrameMetricRow) -> Vec<String> {
    vec![
        METRIC_SCHEMA_VERSION.to_string(),
        row.run_id.0.clone(),
        row.implementation.code().to_owned(),
        row.phase.code().to_owned(),
        row.event.code().to_owned(),
        row.batch_result
            .map(MetricBatchResult::code)
            .unwrap_or("")
            .to_owned(),
        row.batch_failure_class
            .map(MetricBatchFailureClass::code)
            .unwrap_or("")
            .to_owned(),
        row.monotonic_us.to_string(),
        optional_u64(row.session_id),
        optional_u64(row.generation),
        optional_u64(row.revision),
        optional_u64(row.source_updates),
        optional_u64(row.transactions),
        optional_u64(row.rectangles),
        optional_u64(row.batch_cpu_us),
        optional_u64(row.mailbox_age_us),
        optional_u64(row.scope_begins),
        optional_u64(row.scope_finishes),
        optional_u64(row.scope_polls),
        row.gpu_fault_code
            .map(gpu_fault_code)
            .unwrap_or("")
            .to_owned(),
        optional_u64(row.process_cpu_total_us),
        optional_u64(row.process_cpu_delta_us),
        optional_u64(row.working_set_bytes),
        optional_u64(row.frame_response_ms),
        optional_u64(row.input_to_next_present_us),
    ]
}

fn optional_u64(value: Option<u64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn gpu_fault_code(fault: GpuFaultClass) -> &'static str {
    match fault {
        GpuFaultClass::Validation => "Validation",
        GpuFaultClass::OutOfMemory => "OutOfMemory",
        GpuFaultClass::Internal => "Internal",
        GpuFaultClass::DeviceLost => "DeviceLost",
        GpuFaultClass::ObservationIncomplete => "ObservationIncomplete",
    }
}

pub(crate) fn duration_us(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn is_device_namespace(path: &str) -> bool {
    let path = path.replace('/', "\\");
    path.starts_with(r"\\.\") || path.starts_with(r"\\?\") || path.starts_with(r"\??\")
}

fn validate_new_csv_path(path: &Path) -> Result<(), MetricSinkError> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("csv") {
        return Err(MetricSinkError::InvalidConfiguration);
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Err(MetricSinkError::InvalidConfiguration),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(MetricSinkError::InvalidConfiguration),
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let metadata =
        std::fs::symlink_metadata(parent).map_err(|_| MetricSinkError::InvalidConfiguration)?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err(MetricSinkError::InvalidConfiguration);
    }
    for ancestor in parent
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
    {
        let metadata = std::fs::symlink_metadata(ancestor)
            .map_err(|_| MetricSinkError::InvalidConfiguration)?;
        if is_reparse(&metadata) {
            return Err(MetricSinkError::InvalidConfiguration);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::time::Instant;

    use super::{
        FrameMetricRow, FrameMetricsSink, MetricBatchResult, MetricEventKind, MetricImplementation,
        MetricPhase, MetricSinkConfiguration, MetricSinkError, SafeRunId, METRIC_HEADER,
    };

    fn test_directory(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "frd-frame-metrics-{label}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn valid_values(directory: &std::path::Path) -> (OsString, OsString, OsString) {
        (
            directory.join("events.csv").into_os_string(),
            OsString::from("safe_run-1"),
            OsString::from("serial"),
        )
    }

    #[test]
    fn metric_sink_configuration_distinguishes_disabled_partial_and_enabled() {
        let directory = test_directory("truth-table");
        let (path, run_id, implementation) = valid_values(&directory);
        let values = [false, true];
        for has_path in values {
            for has_run_id in values {
                for has_implementation in values {
                    let result = MetricSinkConfiguration::from_values(
                        has_path.then(|| path.clone()),
                        has_run_id.then(|| run_id.clone()),
                        has_implementation.then(|| implementation.clone()),
                    );
                    match (has_path, has_run_id, has_implementation) {
                        (false, false, false) => {
                            assert!(matches!(result, Ok(MetricSinkConfiguration::Disabled)))
                        }
                        (true, true, true) => assert!(matches!(
                            result,
                            Ok(MetricSinkConfiguration::Enabled { .. })
                        )),
                        _ => assert_eq!(result.unwrap_err(), MetricSinkError::InvalidConfiguration),
                    }
                }
            }
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn safe_metric_sink_writes_only_the_fixed_header_and_typed_fields() {
        let directory = test_directory("schema");
        let path = directory.join("events.csv");
        let mut sink = FrameMetricsSink::open_enabled(
            path.clone(),
            SafeRunId("schema_run".to_owned()),
            MetricImplementation::Serial,
            Instant::now(),
        )
        .unwrap();
        sink.write_row(FrameMetricRow {
            run_id: SafeRunId("schema_run".to_owned()),
            implementation: MetricImplementation::Serial,
            phase: MetricPhase::VisibleMeasurement,
            event: MetricEventKind::SerialDrain,
            batch_result: Some(MetricBatchResult::Success),
            batch_failure_class: None,
            monotonic_us: 7,
            session_id: Some(1),
            generation: Some(2),
            revision: Some(3),
            source_updates: Some(4),
            transactions: Some(0),
            rectangles: Some(5),
            batch_cpu_us: Some(6),
            mailbox_age_us: Some(8),
            scope_begins: Some(1),
            scope_finishes: Some(1),
            scope_polls: Some(1),
            gpu_fault_code: None,
            process_cpu_total_us: None,
            process_cpu_delta_us: None,
            working_set_bytes: None,
            frame_response_ms: None,
            input_to_next_present_us: None,
        })
        .unwrap();
        sink.close().unwrap();
        let output = fs::read_to_string(&path).unwrap();
        let mut lines = output.lines();
        assert_eq!(lines.next(), Some(METRIC_HEADER));
        let row = lines.next().unwrap();
        assert_eq!(row.split(',').count(), METRIC_HEADER.split(',').count());
        assert!(!METRIC_HEADER.contains("endpoint"));
        assert!(!METRIC_HEADER.contains("password"));
        assert!(!METRIC_HEADER.contains("pixel"));
        assert!(!METRIC_HEADER.contains("error"));
        assert!(lines.next().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn metric_sink_configuration_rejects_empty_unknown_and_unsafe_values() {
        let directory = test_directory("invalid");
        let (path, run_id, implementation) = valid_values(&directory);
        let invalid = |path: OsString, run: OsString, implementation: OsString| {
            assert_eq!(
                MetricSinkConfiguration::from_values(Some(path), Some(run), Some(implementation))
                    .unwrap_err(),
                MetricSinkError::InvalidConfiguration
            );
        };
        invalid(path.clone(), OsString::from(""), implementation.clone());
        invalid(
            path.clone(),
            OsString::from("space unsafe"),
            implementation.clone(),
        );
        invalid(path.clone(), run_id.clone(), OsString::from("unknown"));
        invalid(
            directory.join("events.txt").into_os_string(),
            run_id.clone(),
            implementation.clone(),
        );
        fs::write(directory.join("existing.csv"), b"existing").unwrap();
        invalid(
            directory.join("existing.csv").into_os_string(),
            run_id.clone(),
            implementation.clone(),
        );
        invalid(
            directory.clone().into_os_string(),
            run_id.clone(),
            implementation.clone(),
        );
        invalid(
            directory
                .join("missing")
                .join("events.csv")
                .into_os_string(),
            run_id.clone(),
            implementation.clone(),
        );
        fs::write(directory.join("not-directory"), b"file").unwrap();
        invalid(
            directory
                .join("not-directory")
                .join("events.csv")
                .into_os_string(),
            run_id.clone(),
            implementation.clone(),
        );
        invalid(
            OsString::from(r"\\.\NUL\events.csv"),
            run_id.clone(),
            implementation.clone(),
        );

        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_dir;
            let real = directory.join("real");
            let link = directory.join("link");
            fs::create_dir(&real).unwrap();
            if symlink_dir(&real, &link).is_ok() {
                invalid(
                    link.join("events.csv").into_os_string(),
                    run_id,
                    implementation,
                );
            }
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
