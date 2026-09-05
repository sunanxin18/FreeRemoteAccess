//! Private compile-time seam for the pinned IronRDP surface used by later tasks.

use ironrdp::{
    connector::{ClientConnector, ConnectionResult},
    input::Database,
    session::{image::DecodedImage, ActiveStageBuilder, ActiveStageOutput},
};

use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;

use crate::config::RdpClientPlatformIdentity;

pub(crate) fn client_platform_type(
    client_platform: RdpClientPlatformIdentity,
) -> MajorPlatformType {
    match client_platform {
        RdpClientPlatformIdentity::Windows => MajorPlatformType::WINDOWS,
        RdpClientPlatformIdentity::Macintosh => MajorPlatformType::MACINTOSH,
        RdpClientPlatformIdentity::Ios => MajorPlatformType::IOS,
        RdpClientPlatformIdentity::Unix => MajorPlatformType::UNIX,
        RdpClientPlatformIdentity::Android => MajorPlatformType::ANDROID,
    }
}

#[allow(dead_code)]
type IronRdp017Seam = (
    Option<ClientConnector>,
    Option<ConnectionResult>,
    Option<DecodedImage>,
    Option<ActiveStageBuilder>,
    Option<ActiveStageOutput>,
    Option<Database>,
);
