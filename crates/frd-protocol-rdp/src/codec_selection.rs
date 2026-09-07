//! Fail-closed ordering for negotiated RDP graphics codecs.
//!
//! The selector is intentionally independent from IronRDP capability-PDU types and from
//! decoder implementations.  A caller must provide three facts for a compressed codec:
//! the exact wire profile was confirmed by the server, an exact decoder is ready, and the
//! platform/server pair passed its production interoperability gate.  A local probe or a
//! codec name alone is never enough to move a codec ahead of the legacy paths.

/// A non-secret result used by the RDP adapter before it builds a graphics-pipeline session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RdpGraphicsCodec {
    /// The exact HEVC profile is carried by the negotiated wire-state owner.  This selector does
    /// not name one because the RDP HEVC wire profile has not yet been established.
    Hevc,
    H264Avc420,
    H264Avc444,
    RemoteFx,
    Bitmap,
}

/// Exact evidence required before a compressed codec can be selected.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ExactCodecGate {
    /// The server confirmed the exact wire/profile variant understood by the adapter.
    pub(crate) wire_profile_confirmed: bool,
    /// A decoder factory reported the exact codec/profile/chroma/output capability and is ready.
    pub(crate) decoder_ready: bool,
    /// The concrete platform/server pair passed first-frame, sustained-refresh and recovery
    /// interoperability checks.
    pub(crate) live_interoperable: bool,
    /// The current server session actually negotiated this codec.
    pub(crate) negotiated: bool,
}

impl ExactCodecGate {
    pub(crate) const fn eligible(self) -> bool {
        self.wire_profile_confirmed
            && self.decoder_ready
            && self.live_interoperable
            && self.negotiated
    }
}

/// Inputs to the production ordering.  `remote_fx` and `bitmap` are already validated by the
/// legacy RDP activation path; compressed codecs remain opt-in until every exact gate is true.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RdpCodecEligibility {
    pub(crate) hevc: ExactCodecGate,
    pub(crate) avc420: ExactCodecGate,
    pub(crate) avc444: ExactCodecGate,
    pub(crate) remote_fx: bool,
    pub(crate) bitmap: bool,
}

/// Choose the first codec that is both negotiated and production-eligible.
///
/// HEVC is deliberately first in the runtime order even though it is implemented last.  Until
/// its exact RDP wire profile and live gate are recorded, its default evidence is false and the
/// selector falls through to AVC/RemoteFX/Bitmap.
pub(crate) fn select_graphics_codec(eligibility: RdpCodecEligibility) -> Option<RdpGraphicsCodec> {
    if eligibility.hevc.eligible() {
        return Some(RdpGraphicsCodec::Hevc);
    }
    if eligibility.avc420.eligible() {
        return Some(RdpGraphicsCodec::H264Avc420);
    }
    if eligibility.avc444.eligible() {
        return Some(RdpGraphicsCodec::H264Avc444);
    }
    if eligibility.remote_fx {
        return Some(RdpGraphicsCodec::RemoteFx);
    }
    eligibility.bitmap.then_some(RdpGraphicsCodec::Bitmap)
}

#[cfg(test)]
mod tests {
    use super::{select_graphics_codec, ExactCodecGate, RdpCodecEligibility, RdpGraphicsCodec};
    fn ready_gate() -> ExactCodecGate {
        ExactCodecGate {
            wire_profile_confirmed: true,
            decoder_ready: true,
            live_interoperable: true,
            negotiated: true,
        }
    }

    #[test]
    fn default_evidence_falls_back_to_bitmap() {
        assert_eq!(
            select_graphics_codec(RdpCodecEligibility {
                bitmap: true,
                ..RdpCodecEligibility::default()
            }),
            Some(RdpGraphicsCodec::Bitmap)
        );
    }

    #[test]
    fn remotefx_is_preferred_over_bitmap_when_compressed_codecs_are_not_ready() {
        assert_eq!(
            select_graphics_codec(RdpCodecEligibility {
                remote_fx: true,
                bitmap: true,
                ..RdpCodecEligibility::default()
            }),
            Some(RdpGraphicsCodec::RemoteFx)
        );
    }

    #[test]
    fn hevc_requires_every_gate_and_is_first_when_ready() {
        let mut eligibility = RdpCodecEligibility {
            hevc: ready_gate(),
            avc420: ready_gate(),
            remote_fx: true,
            bitmap: true,
            ..RdpCodecEligibility::default()
        };
        assert_eq!(
            select_graphics_codec(eligibility),
            Some(RdpGraphicsCodec::Hevc)
        );

        eligibility.hevc.live_interoperable = false;
        assert_eq!(
            select_graphics_codec(eligibility),
            Some(RdpGraphicsCodec::H264Avc420)
        );
    }

    #[test]
    fn hevc_requires_each_independent_evidence_gate() {
        let mut eligibility = RdpCodecEligibility {
            hevc: ready_gate(),
            avc420: ready_gate(),
            remote_fx: true,
            bitmap: true,
            ..RdpCodecEligibility::default()
        };

        for missing_gate in [
            "wire_profile_confirmed",
            "decoder_ready",
            "live_interoperable",
            "negotiated",
        ] {
            eligibility.hevc = ready_gate();
            match missing_gate {
                "wire_profile_confirmed" => eligibility.hevc.wire_profile_confirmed = false,
                "decoder_ready" => eligibility.hevc.decoder_ready = false,
                "live_interoperable" => eligibility.hevc.live_interoperable = false,
                "negotiated" => eligibility.hevc.negotiated = false,
                _ => unreachable!("测试只枚举已知 HEVC 门禁"),
            }

            assert_eq!(
                select_graphics_codec(eligibility),
                Some(RdpGraphicsCodec::H264Avc420),
                "HEVC 缺少 {missing_gate} 时必须回退到 AVC420"
            );
        }
    }

    #[test]
    fn fallback_order_is_hevc_then_avc_then_remotefx_then_bitmap() {
        let mut eligibility = RdpCodecEligibility {
            hevc: ready_gate(),
            avc420: ready_gate(),
            avc444: ready_gate(),
            remote_fx: true,
            bitmap: true,
        };

        assert_eq!(
            select_graphics_codec(eligibility),
            Some(RdpGraphicsCodec::Hevc)
        );

        eligibility.hevc = ExactCodecGate::default();
        assert_eq!(
            select_graphics_codec(eligibility),
            Some(RdpGraphicsCodec::H264Avc420)
        );

        eligibility.avc420 = ExactCodecGate::default();
        assert_eq!(
            select_graphics_codec(eligibility),
            Some(RdpGraphicsCodec::H264Avc444)
        );

        eligibility.avc444 = ExactCodecGate::default();
        assert_eq!(
            select_graphics_codec(eligibility),
            Some(RdpGraphicsCodec::RemoteFx)
        );

        eligibility.remote_fx = false;
        assert_eq!(
            select_graphics_codec(eligibility),
            Some(RdpGraphicsCodec::Bitmap)
        );

        eligibility.bitmap = false;
        assert_eq!(select_graphics_codec(eligibility), None);
    }

    #[test]
    fn avc444_is_not_inferred_from_avc420_and_requires_its_own_gate() {
        let eligibility = RdpCodecEligibility {
            avc420: ExactCodecGate {
                wire_profile_confirmed: true,
                decoder_ready: true,
                negotiated: true,
                ..ExactCodecGate::default()
            },
            avc444: ready_gate(),
            remote_fx: true,
            bitmap: true,
            ..RdpCodecEligibility::default()
        };
        assert_eq!(
            select_graphics_codec(eligibility),
            Some(RdpGraphicsCodec::H264Avc444),
            "未通过 AVC420 live gate 时，只有独立确认的 AVC444 才能被选中"
        );
    }

    #[test]
    fn no_legacy_path_returns_none_instead_of_guessing_a_codec() {
        assert_eq!(select_graphics_codec(RdpCodecEligibility::default()), None);
    }
}
