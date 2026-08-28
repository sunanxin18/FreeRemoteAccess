//! Private compile-time seam for the pinned IronRDP surface used by later tasks.

use ironrdp::{
    connector::{ClientConnector, ConnectionResult},
    input::Database,
    session::{image::DecodedImage, ActiveStageBuilder, ActiveStageOutput},
};

#[allow(dead_code)]
type IronRdp017Seam = (
    Option<ClientConnector>,
    Option<ConnectionResult>,
    Option<DecodedImage>,
    Option<ActiveStageBuilder>,
    Option<ActiveStageOutput>,
    Option<Database>,
);
