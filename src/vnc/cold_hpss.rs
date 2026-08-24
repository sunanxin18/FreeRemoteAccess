use anyhow::{bail, Context, Result};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::vnc::client::{self, RfbConn, VncClient};
use crate::vnc::dynamic_resolution::DisplaySize;
use crate::vnc::hpss::{self, Media};
use crate::vnc::mvs_capture_v2::MvsCaptureV2Geometry;
use crate::vnc::mvs_capture_v2_writer::{
    ArmedConfig, CaptureState, MvsCaptureV2Writer, WriterDecision,
};
use crate::vnc::mvs_stream::MvsRect;
use crate::vnc::session::SessionEncodingProfile;

const COLD_DISPLAY_NAME: &str = "FreeRemoteDesk Cold Capture";
const RECORDING_READ_POLL: Duration = Duration::from_millis(100);

pub type CaptureGeometry = MvsCaptureV2Geometry;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ColdFrameClass {
    Continuation,
    MvsBegin {
        rect: MvsRect,
        total: u32,
        body: Vec<u8>,
    },
    InvalidMvs(MvsRect),
    GeometryTransition,
    NonMvs,
}

pub fn connect_deadline_opts(
    addr: &SocketAddr,
    deadline: Instant,
    username: &str,
    password: &str,
    profile: SessionEncodingProfile,
) -> Result<VncClient> {
    let client = client::connect_deadline_opts(addr, deadline, username, password, profile)?;
    if !client.conn.is_encrypted() {
        bail!("cold authentication");
    }
    Ok(client)
}

pub(crate) trait ColdSessionIo {
    fn is_encrypted(&self) -> bool;
    fn set_read_timeout(&mut self, timeout: Duration) -> Result<()>;
    fn write_all(&mut self, bytes: &[u8]) -> Result<()>;
    fn read_frame_step(&mut self) -> Result<Option<Vec<u8>>>;
    fn is_cancelled(&mut self) -> bool {
        false
    }
}

impl ColdSessionIo for RfbConn {
    fn is_encrypted(&self) -> bool {
        RfbConn::is_encrypted(self)
    }

    fn set_read_timeout(&mut self, timeout: Duration) -> Result<()> {
        RfbConn::set_read_timeout(self, Some(timeout))
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        RfbConn::write_all(self, bytes)
    }

    fn read_frame_step(&mut self) -> Result<Option<Vec<u8>>> {
        RfbConn::read_app_frame_step(self)
    }
}

pub fn run_authenticated_cold_session(
    conn: &mut RfbConn,
    writer: &mut MvsCaptureV2Writer,
    requested: CaptureGeometry,
) -> Result<()> {
    run_authenticated_cold_session_io(conn, writer, requested)
}

fn run_authenticated_cold_session_io<I: ColdSessionIo>(
    io: &mut I,
    writer: &mut MvsCaptureV2Writer,
    requested: CaptureGeometry,
) -> Result<()> {
    if !io.is_encrypted() {
        let _ = writer.authentication_failed()?;
        bail!("cold session encryption required");
    }
    require_continue(writer.arm(ArmedConfig {
        committed: requested,
        requested,
    })?)?;
    require_continue(writer.begin_trigger()?)?;

    write_trigger(
        io,
        writer,
        &hpss::build_set_display_config(COLD_DISPLAY_NAME),
        1,
    )?;
    let display_size =
        DisplaySize::new(requested.width, requested.height).context("cold display geometry")?;
    write_trigger(io, writer, &hpss::build_display_query(display_size), 2)?;
    write_trigger(
        io,
        writer,
        &crate::vnc::protocol::msg_fb_update_request(
            false,
            0,
            0,
            requested.width,
            requested.height,
        ),
        4,
    )?;
    require_continue(writer.write_recording_gate()?)?;
    if io.set_read_timeout(RECORDING_READ_POLL).is_err() {
        return finish_after_read_failure(writer);
    }

    let mut pending_remaining = None;
    loop {
        match writer.poll_recording()? {
            WriterDecision::Continue => {}
            WriterDecision::Finalized(CaptureState::Clean) => return Ok(()),
            WriterDecision::Finalized(_) => bail!("cold session aborted"),
        }
        if io.is_cancelled() {
            return match writer.cancel()? {
                WriterDecision::Finalized(CaptureState::Clean) => Ok(()),
                WriterDecision::Finalized(_) => bail!("cold session cancelled"),
                WriterDecision::Continue => bail!("cold cancellation state"),
            };
        }

        let frame = match io.read_frame_step() {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(error) if client::is_timeout(&error) => continue,
            Err(_) => return finish_after_read_failure(writer),
        };
        let decision = process_frame(writer, &mut pending_remaining, requested, &frame)?;
        match decision {
            WriterDecision::Continue => {}
            WriterDecision::Finalized(CaptureState::Clean) => return Ok(()),
            WriterDecision::Finalized(_) => bail!("cold session aborted"),
        }
    }
}

fn finish_after_read_failure(writer: &mut MvsCaptureV2Writer) -> Result<()> {
    match writer.read_failed()? {
        WriterDecision::Finalized(CaptureState::Clean) => Ok(()),
        WriterDecision::Finalized(_) => bail!("cold session read"),
        WriterDecision::Continue => bail!("cold session read state"),
    }
}

fn write_trigger<I: ColdSessionIo>(
    io: &mut I,
    writer: &mut MvsCaptureV2Writer,
    message: &[u8],
    bit: u32,
) -> Result<()> {
    if io.write_all(message).is_err() {
        let _ = writer.trigger_failed()?;
        bail!("cold trigger write");
    }
    require_continue(writer.trigger_write_succeeded(bit)?)
}

fn require_continue(decision: WriterDecision) -> Result<()> {
    match decision {
        WriterDecision::Continue => Ok(()),
        WriterDecision::Finalized(_) => bail!("cold session finalized before recording"),
    }
}

fn process_frame(
    writer: &mut MvsCaptureV2Writer,
    pending_remaining: &mut Option<usize>,
    committed: CaptureGeometry,
    frame: &[u8],
) -> Result<WriterDecision> {
    match classify_cold_frame(frame, pending_remaining.is_some(), committed) {
        ColdFrameClass::Continuation => {
            let prior = pending_remaining.context("cold pending provenance")?;
            let decision = writer.accept_mvs_continuation(frame)?;
            if matches!(decision, WriterDecision::Continue) {
                *pending_remaining = prior
                    .checked_sub(frame.len())
                    .filter(|remaining| *remaining != 0);
            }
            Ok(decision)
        }
        ColdFrameClass::MvsBegin { rect, total, body } => {
            let decision = writer.accept_mvs_begin(rect, total, &body)?;
            if matches!(decision, WriterDecision::Continue) {
                *pending_remaining = (total as usize)
                    .checked_sub(body.len())
                    .filter(|remaining| *remaining != 0);
            }
            Ok(decision)
        }
        ColdFrameClass::InvalidMvs(rect) => writer.reject_mvs_envelope(Some(rect)),
        ColdFrameClass::GeometryTransition => writer.generation_transition_attempted(),
        ColdFrameClass::NonMvs => writer.accept_non_mvs(),
    }
}

pub(crate) fn classify_cold_frame(
    frame: &[u8],
    pending: bool,
    committed: CaptureGeometry,
) -> ColdFrameClass {
    if pending {
        return ColdFrameClass::Continuation;
    }
    match hpss::parse_media(frame) {
        Ok(Media::Mvs {
            x,
            y,
            w,
            h,
            total,
            body,
        }) => ColdFrameClass::MvsBegin {
            rect: MvsRect {
                x,
                y,
                width: w,
                height: h,
            },
            total,
            body,
        },
        Ok(Media::State(encoding)) if encoding == hpss::encoding::SERVER_STATE => {
            match hpss::parse_server_state_geometry(frame) {
                Ok(state) if state.width != committed.width || state.height != committed.height => {
                    ColdFrameClass::GeometryTransition
                }
                _ => ColdFrameClass::NonMvs,
            }
        }
        Ok(_) => ColdFrameClass::NonMvs,
        Err(_) => hpss::mvs_envelope_candidate_rect(frame)
            .map(ColdFrameClass::InvalidMvs)
            .unwrap_or(ColdFrameClass::NonMvs),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_cold_frame, connect_deadline_opts, run_authenticated_cold_session,
        run_authenticated_cold_session_io, CaptureGeometry, ColdFrameClass, ColdSessionIo,
    };
    use crate::vnc::mvs::{MvsDecodeDecision, MvsDecodeState};
    use crate::vnc::mvs_capture_v2::{
        read_mvs_capture_v2_strict_cold, read_mvs_capture_v2_structural, MvsCaptureV2TerminalKind,
        MvsCaptureV2TerminalReason,
    };
    use crate::vnc::mvs_capture_v2_writer::{
        CaptureClock, CaptureFinalFile, CaptureIntoInnerResult, CaptureSink, CreatedConfig,
        MvsCaptureV2Writer,
    };
    use crate::vnc::mvs_stream::MvsRect;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io::{self, Cursor};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct TempCapturePath(PathBuf);

    impl Drop for TempCapturePath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn cold_hpss_public_integration_entry_points_exist() {
        let _ = connect_deadline_opts;
        let _ = run_authenticated_cold_session;
    }

    #[test]
    fn pending_input_owns_every_next_application_frame_as_continuation() {
        let port_announcement = port_announcement_fixture();
        assert_eq!(
            classify_cold_frame(&port_announcement, true, geometry(640, 480)),
            ColdFrameClass::Continuation
        );
    }

    #[test]
    fn idle_classification_distinguishes_mvs_geometry_and_non_mvs_without_udp_side_effects() {
        let mvs = mvs_frame(4, &[2, 1]);
        assert!(matches!(
            classify_cold_frame(&mvs, false, geometry(640, 480)),
            ColdFrameClass::MvsBegin { total: 4, ref body, .. } if body == &[2, 1]
        ));
        assert_eq!(
            classify_cold_frame(&port_announcement_fixture(), false, geometry(640, 480)),
            ColdFrameClass::NonMvs
        );
        assert_eq!(
            classify_cold_frame(&server_state(640, 480), false, geometry(640, 480)),
            ColdFrameClass::NonMvs
        );
        assert_eq!(
            classify_cold_frame(&server_state(800, 600), false, geometry(640, 480)),
            ColdFrameClass::GeometryTransition
        );
    }

    #[test]
    fn lifecycle_trigger_order_pending_ownership_and_mvs_only_ordinals_are_frozen() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let sink = MemorySink {
            bytes: Rc::clone(&bytes),
            trace: Rc::clone(&trace),
            fail_write_label_once: None,
            fail_flush_after_label_once: None,
        };
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(ZeroClock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();
        let mut io = FakeSessionIo {
            trace: Rc::clone(&trace),
            frames: VecDeque::from([
                vec![0],
                port_announcement_fixture().to_vec(),
                mvs_frame_with_rect(3, &[2], 0, 0),
                vec![0xaa, 0xbb],
            ]),
            writes: Vec::new(),
            encrypted: true,
            fail_set_timeout: false,
            fail_write_at: None,
            cancel_after_reads: None,
            reads: 0,
        };

        run_authenticated_cold_session_io(&mut io, &mut writer, geometry(640, 480)).unwrap();

        assert_eq!(io.writes.len(), 3);
        assert_eq!(io.writes[0][0], 0x1d);
        assert_eq!(io.writes[1][0], 0x09);
        assert_eq!(io.writes[2][0], 0x03);
        let trace = trace.borrow();
        assert_eq!(
            &trace[..11],
            &[
                "created",
                "flush",
                "armed",
                "flush",
                "sync",
                "trigger-1d",
                "trigger-09",
                "trigger-03",
                "triggered",
                "recording",
                "flush",
            ]
        );
        drop(trace);
        let mut reader = Cursor::new(bytes.borrow().clone());
        let strict = read_mvs_capture_v2_strict_cold(&mut reader).unwrap();
        assert_eq!(strict.records.len(), 1);
        assert_eq!(strict.records[0].first_source_frame_ordinal, 0);
        assert_eq!(strict.records[0].last_source_frame_ordinal, 1);
        assert_eq!(strict.records[0].record.payload, [2, 0xaa, 0xbb]);
        assert_eq!(strict.terminal.source_mvs_frame_count, 2);
    }

    #[test]
    fn committed_geometry_transition_aborts_without_starting_another_transport() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(MemorySink {
                bytes: Rc::clone(&bytes),
                trace: Rc::clone(&trace),
                fail_write_label_once: None,
                fail_flush_after_label_once: None,
            }),
            Box::new(ZeroClock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();
        let mut io = FakeSessionIo {
            trace,
            frames: VecDeque::from([server_state(800, 600)]),
            writes: Vec::new(),
            encrypted: true,
            fail_set_timeout: false,
            fail_write_at: None,
            cancel_after_reads: None,
            reads: 0,
        };

        assert!(
            run_authenticated_cold_session_io(&mut io, &mut writer, geometry(640, 480)).is_err()
        );

        let mut reader = Cursor::new(bytes.borrow().clone());
        let structural = read_mvs_capture_v2_structural(&mut reader).unwrap();
        assert_eq!(
            structural.terminal.reason,
            MvsCaptureV2TerminalReason::InvalidGeometry
        );
        assert!(structural.records.is_empty());
        assert!(structural.gaps.is_empty());
    }

    #[test]
    fn unencrypted_session_finalizes_as_authentication_failure() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(MemorySink {
                bytes: Rc::clone(&bytes),
                trace: Rc::clone(&trace),
                fail_write_label_once: None,
                fail_flush_after_label_once: None,
            }),
            Box::new(ZeroClock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();
        let mut io = FakeSessionIo {
            trace,
            frames: VecDeque::new(),
            writes: Vec::new(),
            encrypted: false,
            fail_set_timeout: false,
            fail_write_at: None,
            cancel_after_reads: None,
            reads: 0,
        };

        assert!(
            run_authenticated_cold_session_io(&mut io, &mut writer, geometry(640, 480)).is_err()
        );

        let mut reader = Cursor::new(bytes.borrow().clone());
        let structural = read_mvs_capture_v2_structural(&mut reader).unwrap();
        assert_eq!(
            structural.terminal.reason,
            MvsCaptureV2TerminalReason::CredentialOrAuthenticationFailure
        );
    }

    #[test]
    fn recording_timeout_configuration_failure_finalizes_as_read_failure() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(MemorySink {
                bytes: Rc::clone(&bytes),
                trace: Rc::clone(&trace),
                fail_write_label_once: None,
                fail_flush_after_label_once: None,
            }),
            Box::new(ZeroClock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();
        let mut io = FakeSessionIo {
            trace,
            frames: VecDeque::new(),
            writes: Vec::new(),
            encrypted: true,
            fail_set_timeout: true,
            fail_write_at: None,
            cancel_after_reads: None,
            reads: 0,
        };

        assert!(
            run_authenticated_cold_session_io(&mut io, &mut writer, geometry(640, 480)).is_err()
        );

        let mut reader = Cursor::new(bytes.borrow().clone());
        let structural = read_mvs_capture_v2_structural(&mut reader).unwrap();
        assert_eq!(
            structural.terminal.reason,
            MvsCaptureV2TerminalReason::ReadFailure
        );
    }

    #[test]
    fn cancellation_is_checked_after_deadline_and_lifetime_but_before_next_read() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(MemorySink {
                bytes: Rc::clone(&bytes),
                trace: Rc::clone(&trace),
                fail_write_label_once: None,
                fail_flush_after_label_once: None,
            }),
            Box::new(ZeroClock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();
        let mut io = FakeSessionIo {
            trace,
            frames: VecDeque::from([mvs_frame_with_rect(3, &[0], 1, 1)]),
            writes: Vec::new(),
            encrypted: true,
            fail_set_timeout: false,
            fail_write_at: None,
            cancel_after_reads: Some(1),
            reads: 0,
        };

        assert!(
            run_authenticated_cold_session_io(&mut io, &mut writer, geometry(640, 480)).is_err()
        );

        let mut reader = Cursor::new(bytes.borrow().clone());
        let structural = read_mvs_capture_v2_structural(&mut reader).unwrap();
        assert_eq!(
            structural.terminal.reason,
            MvsCaptureV2TerminalReason::OperatorCancellation
        );
        assert_eq!(structural.gaps.len(), 1);
        assert_eq!(structural.gaps[0].reason, 12);
        assert_eq!(structural.terminal.source_mvs_frame_count, 1);
    }

    #[test]
    fn malformed_mvs_envelope_emits_exact_assembler_gap_and_aborts() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(MemorySink {
                bytes: Rc::clone(&bytes),
                trace: Rc::clone(&trace),
                fail_write_label_once: None,
                fail_flush_after_label_once: None,
            }),
            Box::new(ZeroClock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();
        let mut malformed = vec![0_u8; 16];
        malformed[0..4].copy_from_slice(&1_u32.to_be_bytes());
        malformed[4..6].copy_from_slice(&7_u16.to_be_bytes());
        malformed[6..8].copy_from_slice(&8_u16.to_be_bytes());
        malformed[8..10].copy_from_slice(&9_u16.to_be_bytes());
        malformed[10..12].copy_from_slice(&10_u16.to_be_bytes());
        malformed[12..16].copy_from_slice(&crate::vnc::hpss::encoding::MVS.to_be_bytes());
        let mut io = FakeSessionIo {
            trace,
            frames: VecDeque::from([malformed]),
            writes: Vec::new(),
            encrypted: true,
            fail_set_timeout: false,
            fail_write_at: None,
            cancel_after_reads: None,
            reads: 0,
        };

        assert!(
            run_authenticated_cold_session_io(&mut io, &mut writer, geometry(640, 480)).is_err()
        );

        let structural =
            read_mvs_capture_v2_structural(&mut Cursor::new(bytes.borrow().clone())).unwrap();
        assert_eq!(
            structural.terminal.reason,
            MvsCaptureV2TerminalReason::AssemblerFailure
        );
        assert_eq!(structural.terminal.source_mvs_frame_count, 1);
        assert_eq!(structural.gaps.len(), 1);
        let gap = &structural.gaps[0];
        assert_eq!((gap.reason, gap.stage), (1, 1));
        assert_eq!(
            (
                gap.first_source_frame_ordinal,
                gap.last_source_frame_ordinal
            ),
            (0, 0)
        );
        assert_eq!((gap.declared_total, gap.accumulated_bytes), (0, 0));
        assert_eq!(
            gap.rect,
            crate::vnc::mvs_stream::MvsRect {
                x: 7,
                y: 8,
                width: 9,
                height: 10,
            }
        );
    }

    #[test]
    fn each_trigger_write_failure_selects_260_before_any_read() {
        for fail_write_at in 0..3 {
            let trace = Rc::new(RefCell::new(Vec::new()));
            let bytes = Rc::new(RefCell::new(Vec::new()));
            let mut writer = MvsCaptureV2Writer::new(
                Box::new(MemorySink {
                    bytes: Rc::clone(&bytes),
                    trace: Rc::clone(&trace),
                    fail_write_label_once: None,
                    fail_flush_after_label_once: None,
                }),
                Box::new(ZeroClock),
                CreatedConfig {
                    deadline_ms: 5_000,
                    record_limit: 1,
                },
            )
            .unwrap();
            let mut io = FakeSessionIo {
                trace,
                frames: VecDeque::new(),
                writes: Vec::new(),
                encrypted: true,
                fail_set_timeout: false,
                fail_write_at: Some(fail_write_at),
                cancel_after_reads: None,
                reads: 0,
            };

            assert!(
                run_authenticated_cold_session_io(&mut io, &mut writer, geometry(640, 480))
                    .is_err()
            );
            assert_eq!(io.reads, 0);
            assert_eq!(io.writes.len(), fail_write_at + 1);
            let structural =
                read_mvs_capture_v2_structural(&mut Cursor::new(bytes.borrow().clone())).unwrap();
            assert_eq!(
                structural.terminal.reason,
                MvsCaptureV2TerminalReason::TriggerFailure
            );
            assert!(structural.gaps.is_empty());
        }
    }

    #[test]
    fn recording_write_and_flush_failures_select_260_before_any_read() {
        for (fail_write_label_once, fail_flush_after_label_once) in
            [(Some("recording"), None), (None, Some("recording"))]
        {
            let trace = Rc::new(RefCell::new(Vec::new()));
            let bytes = Rc::new(RefCell::new(Vec::new()));
            let mut writer = MvsCaptureV2Writer::new(
                Box::new(MemorySink {
                    bytes: Rc::clone(&bytes),
                    trace: Rc::clone(&trace),
                    fail_write_label_once,
                    fail_flush_after_label_once,
                }),
                Box::new(ZeroClock),
                CreatedConfig {
                    deadline_ms: 5_000,
                    record_limit: 1,
                },
            )
            .unwrap();
            let mut io = FakeSessionIo {
                trace,
                frames: VecDeque::new(),
                writes: Vec::new(),
                encrypted: true,
                fail_set_timeout: false,
                fail_write_at: None,
                cancel_after_reads: None,
                reads: 0,
            };

            assert!(
                run_authenticated_cold_session_io(&mut io, &mut writer, geometry(640, 480))
                    .is_err()
            );
            assert_eq!(io.reads, 0);
            let structural =
                read_mvs_capture_v2_structural(&mut Cursor::new(bytes.borrow().clone())).unwrap();
            assert_eq!(
                structural.terminal.reason,
                MvsCaptureV2TerminalReason::TriggerFailure
            );
            assert!(structural.gaps.is_empty());
        }
    }

    fn geometry(width: u16, height: u16) -> CaptureGeometry {
        CaptureGeometry { width, height }
    }

    fn mvs_frame(total: u32, body: &[u8]) -> Vec<u8> {
        mvs_frame_with_rect(total, body, 1, 1)
    }

    fn mvs_frame_with_rect(total: u32, body: &[u8], width: u16, height: u16) -> Vec<u8> {
        let mut frame = vec![0u8; 16];
        frame[0..4].copy_from_slice(&1u32.to_be_bytes());
        frame[8..10].copy_from_slice(&width.to_be_bytes());
        frame[10..12].copy_from_slice(&height.to_be_bytes());
        frame[12..16].copy_from_slice(&crate::vnc::hpss::encoding::MVS.to_be_bytes());
        frame.extend_from_slice(&total.to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }

    fn server_state(width: u16, height: u16) -> Vec<u8> {
        let mut frame = vec![0u8; 94];
        frame[0..4].copy_from_slice(&1u32.to_be_bytes());
        frame[12..16].copy_from_slice(&crate::vnc::hpss::encoding::SERVER_STATE.to_be_bytes());
        frame[16..18].copy_from_slice(&76u16.to_be_bytes());
        frame[18..20].copy_from_slice(&1u16.to_be_bytes());
        frame[20..22].copy_from_slice(&width.to_be_bytes());
        frame[22..24].copy_from_slice(&height.to_be_bytes());
        frame
    }

    fn port_announcement_fixture() -> [u8; 54] {
        let mut frame = [0u8; 54];
        frame[0..4].copy_from_slice(&1u32.to_be_bytes());
        frame[12..16].copy_from_slice(&0x03f2i32.to_be_bytes());
        frame[16..18].copy_from_slice(&36u16.to_be_bytes());
        frame[18..20].copy_from_slice(&1u16.to_be_bytes());
        frame[20..22].copy_from_slice(&1u16.to_be_bytes());
        frame[26..28].copy_from_slice(&5900u16.to_be_bytes());
        frame[28..32].copy_from_slice(&1u32.to_be_bytes());
        frame
    }

    struct ZeroClock;

    impl CaptureClock for ZeroClock {
        fn elapsed_micros(&self) -> anyhow::Result<u64> {
            Ok(0)
        }
    }

    struct MemorySink {
        bytes: Rc<RefCell<Vec<u8>>>,
        trace: Rc<RefCell<Vec<&'static str>>>,
        fail_write_label_once: Option<&'static str>,
        fail_flush_after_label_once: Option<&'static str>,
    }

    impl CaptureSink for MemorySink {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            let label = if bytes.starts_with(b"FRDMVS02") {
                "created"
            } else {
                match u16::from_be_bytes(bytes[4..6].try_into().unwrap()) {
                    0x0002 => "armed",
                    0x0003 => "triggered",
                    0x0004 => "recording",
                    0x0020 => "record",
                    0x00fe | 0x00ff => "terminal",
                    _ => "event",
                }
            };
            self.trace.borrow_mut().push(label);
            if self.fail_write_label_once == Some(label) {
                self.fail_write_label_once = None;
                return Err(io::Error::other("injected event write failure"));
            }
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            let last = self.trace.borrow().last().copied();
            self.trace.borrow_mut().push("flush");
            if self.fail_flush_after_label_once == last {
                self.fail_flush_after_label_once = None;
                return Err(io::Error::other("injected event flush failure"));
            }
            Ok(())
        }

        fn sync_data(&mut self) -> io::Result<()> {
            self.trace.borrow_mut().push("sync");
            Ok(())
        }

        fn into_final_file(self: Box<Self>) -> CaptureIntoInnerResult {
            CaptureIntoInnerResult::Ready(Box::new(MemoryFinalFile))
        }
    }

    struct MemoryFinalFile;

    impl CaptureFinalFile for MemoryFinalFile {
        fn sync_data(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn relinquish(self: Box<Self>) -> io::Result<()> {
            Ok(())
        }
    }

    struct FakeSessionIo {
        trace: Rc<RefCell<Vec<&'static str>>>,
        frames: VecDeque<Vec<u8>>,
        writes: Vec<Vec<u8>>,
        encrypted: bool,
        fail_set_timeout: bool,
        fail_write_at: Option<usize>,
        cancel_after_reads: Option<usize>,
        reads: usize,
    }

    impl ColdSessionIo for FakeSessionIo {
        fn is_encrypted(&self) -> bool {
            self.encrypted
        }

        fn set_read_timeout(&mut self, _timeout: Duration) -> anyhow::Result<()> {
            self.trace.borrow_mut().push("poll-timeout");
            if self.fail_set_timeout {
                anyhow::bail!("injected timeout configuration failure");
            }
            Ok(())
        }

        fn write_all(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
            self.trace.borrow_mut().push(match bytes[0] {
                0x1d => "trigger-1d",
                0x09 => "trigger-09",
                0x03 => "trigger-03",
                _ => "trigger-other",
            });
            self.writes.push(bytes.to_vec());
            if self.fail_write_at == Some(self.writes.len() - 1) {
                anyhow::bail!("injected trigger write failure");
            }
            Ok(())
        }

        fn is_cancelled(&mut self) -> bool {
            self.cancel_after_reads
                .is_some_and(|limit| self.reads >= limit)
        }

        fn read_frame_step(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
            self.trace.borrow_mut().push("read");
            self.reads += 1;
            Ok(self.frames.pop_front())
        }
    }

    #[test]
    fn loopback_mock_produces_strict_cold_capture() {
        const WIDTH: u16 = 640;
        const HEIGHT: u16 = 480;
        const KEY: [u8; 16] = [0x31; 16];
        const IV: [u8; 16] = [0x42; 16];

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let capture_path = std::env::temp_dir().join(format!(
            "freeremotedesk-task6-loopback-{}-{unique}.mvs",
            std::process::id()
        ));
        assert!(
            !capture_path.exists(),
            "loopback capture path must be create-new"
        );
        let _capture_cleanup = TempCapturePath(capture_path.clone());

        // Independent hand literals: these are not produced by the builders
        // exercised on the client side.
        let mut expected_set_config = vec![0u8; 308];
        expected_set_config[..14].copy_from_slice(&[
            0x1d, 0x00, 0x01, 0x30, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x28,
        ]);
        expected_set_config[14..41].copy_from_slice(b"FreeRemoteDesk Cold Capture");
        let expected_display_query: [u8; 16] = [
            0x09, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x80,
            0x01, 0xe0,
        ];
        let expected_full_request: [u8; 10] =
            [0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x80, 0x01, 0xe0];

        let mut tables = vec![2];
        tables.extend(std::iter::repeat_n(1, 128));
        let full = vec![0, 5, 0, 0, 0, 8, 0xd3, 0x68, 0x00, 0x13, 0x68];
        let partial = vec![1, 1, 1, 0x40, 0x01, 0xb5, 0xd9, 0xcc];
        let fragmented = vec![0, 0, 0, 0, 0, 8, 0x83, 0x68, 0x6d];
        let server_tables = tables.clone();
        let server_full = full.clone();
        let server_partial = partial.clone();
        let server_fragmented = fragmented.clone();

        let mut writer = MvsCaptureV2Writer::create_new(
            &capture_path,
            CreatedConfig {
                deadline_ms: 15_000,
                record_limit: 4,
            },
        )
        .unwrap();
        let absolute_deadline = writer.absolute_deadline().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::scope(|scope| {
            let server = scope.spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut peer = crate::vnc::client::RfbConn::new(stream);
                peer.set_crypto(crate::vnc::session::SessionCrypto::from_key_iv(KEY, IV));

                assert_eq!(peer.read_app_frame().unwrap(), expected_set_config);
                assert_eq!(peer.read_app_frame().unwrap(), expected_display_query);
                assert_eq!(peer.read_app_frame().unwrap(), expected_full_request);

                let frames = [
                    vec![
                        crate::vnc::protocol::apple_session::SERVER_KEEPALIVE_MESSAGE_TYPE,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                    ],
                    server_state(WIDTH, HEIGHT),
                    mvs_frame_with_rect(server_tables.len() as u32, &server_tables, 0, 0),
                    port_announcement_fixture().to_vec(),
                    mvs_frame_with_rect(server_full.len() as u32, &server_full, 8, 8),
                    vec![
                        crate::vnc::protocol::apple_session::SERVER_KEEPALIVE_MESSAGE_TYPE,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                    ],
                    mvs_frame_with_rect(server_partial.len() as u32, &server_partial, 8, 8),
                    server_state(WIDTH, HEIGHT),
                    mvs_frame_with_rect(
                        server_fragmented.len() as u32,
                        &server_fragmented[..4],
                        8,
                        8,
                    ),
                    server_fragmented[4..].to_vec(),
                ];
                for frame in frames {
                    peer.write_all(&frame).unwrap();
                }
            });

            let stream = TcpStream::connect(address).unwrap();
            let mut connection =
                crate::vnc::client::RfbConn::new_with_deadline(stream, absolute_deadline);
            connection
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            connection.set_crypto(crate::vnc::session::SessionCrypto::from_key_iv(KEY, IV));
            run_authenticated_cold_session(
                &mut connection,
                &mut writer,
                CaptureGeometry {
                    width: WIDTH,
                    height: HEIGHT,
                },
            )
            .unwrap();
            server.join().unwrap();
        });

        let structural = {
            let mut file = std::fs::File::open(&capture_path).unwrap();
            read_mvs_capture_v2_structural(&mut file).unwrap()
        };
        let strict = {
            let mut file = std::fs::File::open(&capture_path).unwrap();
            read_mvs_capture_v2_strict_cold(&mut file).unwrap()
        };

        assert_eq!(structural.terminal.kind, MvsCaptureV2TerminalKind::Clean);
        assert_eq!(
            structural.terminal.reason,
            MvsCaptureV2TerminalReason::RecordLimit
        );
        assert!(structural.surfaces.is_empty());
        assert!(structural.gaps.is_empty());
        assert_eq!(structural.committed_surface, structural.requested_surface);
        assert_eq!(strict.committed_surface.width, WIDTH);
        assert_eq!(strict.committed_surface.height, HEIGHT);
        assert_eq!(strict.requested_surface, strict.committed_surface);

        assert_eq!(strict.records.len(), 4);
        let expected_records = [
            (
                MvsRect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                &tables,
            ),
            (
                MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                &full,
            ),
            (
                MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                &partial,
            ),
            (
                MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                &fragmented,
            ),
        ];
        for (actual, (expected_rect, expected_payload)) in
            strict.records.iter().zip(expected_records)
        {
            assert_eq!(actual.record.rect, expected_rect);
            assert_eq!(&actual.record.payload, expected_payload);
        }
        assert_eq!(
            strict
                .records
                .iter()
                .map(|record| (
                    record.first_source_frame_ordinal,
                    record.last_source_frame_ordinal,
                ))
                .collect::<Vec<_>>(),
            [(0, 0), (1, 1), (2, 2), (3, 4)]
        );
        assert!(strict.records.iter().all(|record| record.generation == 0));
        assert!(strict.records.iter().all(|record| {
            let rect = record.record.rect;
            (rect.width == 0 && rect.height == 0)
                || (u32::from(rect.x) + u32::from(rect.width) <= u32::from(WIDTH)
                    && u32::from(rect.y) + u32::from(rect.height) <= u32::from(HEIGHT))
        }));

        let terminal = &strict.terminal;
        assert_eq!(terminal.record_count, 4);
        assert_eq!(terminal.type2_count, 1);
        assert_eq!(terminal.type0_count, 2);
        assert_eq!(terminal.type1_count, 1);
        assert_eq!(terminal.source_mvs_frame_count, 5);
        assert_eq!(terminal.gap_count, 0);
        assert_eq!(strict.record_limit, 4);
        assert_eq!(strict.deadline_ms, 15_000);

        // The complete capture is reopened and checked byte-for-byte before any
        // downstream classification or decoder state is constructed.
        let mut decoder = MvsDecodeState::new(0);
        decoder
            .install_tables(0, &strict.records[0].record.payload)
            .unwrap();
        let mut decoded_pixel_records = 0usize;
        for record in &strict.records[1..] {
            match decoder
                .prepare(
                    0,
                    &record.record.payload,
                    record.record.rect.width,
                    record.record.rect.height,
                )
                .unwrap()
            {
                MvsDecodeDecision::Prepared(prepared) => {
                    let decoded = decoder.commit(prepared).unwrap();
                    assert!(!decoded.rgb.is_empty());
                    decoded_pixel_records += 1;
                }
                MvsDecodeDecision::PreparedOpaque(prepared) => {
                    let before = decoded_pixel_records;
                    decoder.commit_opaque(prepared).unwrap();
                    assert_eq!(
                        decoded_pixel_records, before,
                        "type-1 opaque commit must not publish decoded pixels"
                    );
                }
                decision => panic!("unexpected downstream MVS decision: {decision:?}"),
            }
        }
        assert_eq!(decoded_pixel_records, 2);
    }
}
