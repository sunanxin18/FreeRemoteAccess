use std::collections::VecDeque;

use frd_protocol_api::{ClipboardPayload, SessionCapabilities};
use ironrdp::cliprdr::backend::CliprdrBackend;
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
    OwnedFormatDataResponse,
};
use ironrdp::cliprdr::{Client, CliprdrClient, CliprdrSvcMessages};
use ironrdp::core::impl_as_any;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalOffer {
    generation: u64,
    has_text: bool,
}

#[derive(Debug)]
struct LocalText {
    generation: u64,
    value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RemoteOffer {
    generation: u64,
    has_unicode: bool,
}

#[derive(Debug, Default)]
struct RemoteResponseOutcome {
    payload: Option<ClipboardPayload>,
    follow_up_generation: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ClipboardAdapter {
    channel_ready: bool,
    remote_unicode: bool,
    write_direction_ready: bool,
    next_generation: u64,
    pending_remote_offer: Option<RemoteOffer>,
    outstanding_remote_request: Option<RemoteOffer>,
    pending_local_offer: Option<LocalOffer>,
    pending_local_text: Option<LocalText>,
    accepted_local_text: Option<LocalText>,
    queued_local_text: Option<String>,
}

impl Default for ClipboardAdapter {
    fn default() -> Self {
        Self {
            channel_ready: false,
            remote_unicode: false,
            write_direction_ready: false,
            next_generation: 1,
            pending_remote_offer: None,
            outstanding_remote_request: None,
            pending_local_offer: None,
            pending_local_text: None,
            accepted_local_text: None,
            queued_local_text: None,
        }
    }
}

#[derive(Debug)]
enum ClipboardAction {
    AdvertiseInitial,
    AdvertiseQueuedLocal,
    RequestUnicode(u64),
    RespondUnicode(u64),
    RespondError,
    Publish(ClipboardPayload),
}

#[derive(Debug, Default)]
pub(crate) struct RdpClipboardBackend {
    adapter: ClipboardAdapter,
    actions: VecDeque<ClipboardAction>,
}

impl_as_any!(RdpClipboardBackend);

impl RdpClipboardBackend {
    fn take_action(&mut self) -> Option<ClipboardAction> {
        self.actions.pop_front()
    }

    fn reset(&mut self) {
        self.adapter.reset();
        self.actions.clear();
    }
}

impl CliprdrBackend for RdpClipboardBackend {
    fn temporary_directory(&self) -> &str {
        ""
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        self.adapter.observe_channel_ready();
        if let Some(generation) = self.adapter.remote_request_generation() {
            self.actions
                .push_back(ClipboardAction::RequestUnicode(generation));
        }
    }

    fn on_request_format_list(&mut self) {
        self.adapter.begin_initial_offer();
        self.actions.push_back(ClipboardAction::AdvertiseInitial);
    }

    fn on_format_list_response(&mut self, accepted: bool) {
        if self.adapter.observe_local_format_response(accepted) {
            self.actions
                .push_back(ClipboardAction::AdvertiseQueuedLocal);
        }
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
    }

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        if let Some(generation) = self.adapter.observe_remote_formats(formats) {
            if self.adapter.channel_ready {
                self.actions
                    .push_back(ClipboardAction::RequestUnicode(generation));
            }
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let action = if request.format == ClipboardFormatId::CF_UNICODETEXT {
            self.adapter
                .accepted_local_generation()
                .map(ClipboardAction::RespondUnicode)
                .unwrap_or(ClipboardAction::RespondError)
        } else {
            ClipboardAction::RespondError
        };
        self.actions.push_back(action);
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        let outcome = self.adapter.accept_remote_response(response);
        if let Some(payload) = outcome.payload {
            self.actions.push_back(ClipboardAction::Publish(payload));
        }
        if let Some(generation) = outcome.follow_up_generation {
            self.actions
                .push_back(ClipboardAction::RequestUnicode(generation));
        }
    }

    // File redirection is outside the product scope. These callbacks deliberately
    // remain inert, and no file-related capability flags are advertised above.
    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

pub(crate) enum ClipboardServiceAction {
    Wire(CliprdrSvcMessages<Client>),
    Publish(ClipboardPayload),
}

pub(crate) fn new_cliprdr() -> CliprdrClient {
    CliprdrClient::new(Box::<RdpClipboardBackend>::default())
}

pub(crate) fn capabilities(cliprdr: &CliprdrClient) -> SessionCapabilities {
    cliprdr
        .downcast_backend::<RdpClipboardBackend>()
        .map(|backend| backend.adapter.capabilities())
        .unwrap_or_default()
}

pub(crate) fn reset(cliprdr: &mut CliprdrClient) {
    if let Some(backend) = cliprdr.downcast_backend_mut::<RdpClipboardBackend>() {
        backend.reset();
    }
}

pub(crate) fn write_text(
    cliprdr: &mut CliprdrClient,
    payload: ClipboardPayload,
) -> ironrdp::pdu::PduResult<Option<CliprdrSvcMessages<Client>>> {
    let generation = cliprdr
        .downcast_backend_mut::<RdpClipboardBackend>()
        .and_then(|backend| backend.adapter.accept_local_payload(payload));
    let Some(generation) = generation else {
        return Ok(None);
    };
    let formats = [ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
    match cliprdr.initiate_copy(&formats) {
        Ok(messages) => Ok(Some(messages)),
        Err(error) => {
            if let Some(backend) = cliprdr.downcast_backend_mut::<RdpClipboardBackend>() {
                backend.adapter.reject_local_offer(generation);
            }
            Err(error)
        }
    }
}

pub(crate) fn next_service_action(
    cliprdr: &mut CliprdrClient,
) -> ironrdp::pdu::PduResult<Option<ClipboardServiceAction>> {
    let action = cliprdr
        .downcast_backend_mut::<RdpClipboardBackend>()
        .and_then(RdpClipboardBackend::take_action);
    let Some(action) = action else {
        return Ok(None);
    };
    match action {
        ClipboardAction::AdvertiseInitial => cliprdr
            .initiate_copy(&[])
            .map(ClipboardServiceAction::Wire)
            .map(Some),
        ClipboardAction::AdvertiseQueuedLocal => {
            let generation = cliprdr
                .downcast_backend_mut::<RdpClipboardBackend>()
                .and_then(|backend| backend.adapter.begin_queued_local_offer());
            let Some(generation) = generation else {
                return Ok(None);
            };
            let formats = [ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
            match cliprdr.initiate_copy(&formats) {
                Ok(messages) => Ok(Some(ClipboardServiceAction::Wire(messages))),
                Err(error) => {
                    if let Some(backend) = cliprdr.downcast_backend_mut::<RdpClipboardBackend>() {
                        backend.adapter.reject_local_offer(generation);
                    }
                    Err(error)
                }
            }
        }
        ClipboardAction::RequestUnicode(generation) => {
            let can_request = cliprdr
                .downcast_backend::<RdpClipboardBackend>()
                .is_some_and(|backend| backend.adapter.can_begin_remote_request(generation));
            if !can_request {
                return Ok(None);
            }
            let messages = cliprdr.initiate_paste(ClipboardFormatId::CF_UNICODETEXT)?;
            let began = cliprdr
                .downcast_backend_mut::<RdpClipboardBackend>()
                .is_some_and(|backend| backend.adapter.begin_remote_request(generation));
            if !began {
                return Ok(None);
            }
            Ok(Some(ClipboardServiceAction::Wire(messages)))
        }
        ClipboardAction::RespondUnicode(generation) => {
            let response = cliprdr
                .downcast_backend::<RdpClipboardBackend>()
                .and_then(|backend| backend.adapter.local_unicode_response(generation))
                .unwrap_or_else(OwnedFormatDataResponse::new_error);
            cliprdr
                .submit_format_data(response)
                .map(ClipboardServiceAction::Wire)
                .map(Some)
        }
        ClipboardAction::RespondError => cliprdr
            .submit_format_data(OwnedFormatDataResponse::new_error())
            .map(ClipboardServiceAction::Wire)
            .map(Some),
        ClipboardAction::Publish(payload) => Ok(Some(ClipboardServiceAction::Publish(payload))),
    }
}

impl ClipboardAdapter {
    fn allocate_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.checked_add(1).unwrap_or(1);
        generation
    }

    pub(crate) fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            clipboard_read: self.channel_ready && self.remote_unicode,
            clipboard_write: self.channel_ready && self.write_direction_ready,
            ..SessionCapabilities::default()
        }
    }

    pub(crate) fn begin_initial_offer(&mut self) -> u64 {
        let generation = self.allocate_generation();
        self.pending_local_offer = Some(LocalOffer {
            generation,
            has_text: false,
        });
        self.pending_local_text = None;
        self.accepted_local_text = None;
        self.queued_local_text = None;
        generation
    }

    pub(crate) fn observe_channel_ready(&mut self) {
        self.channel_ready = true;
    }

    pub(crate) fn observe_local_format_response(&mut self, accepted: bool) -> bool {
        let Some(offer) = self.pending_local_offer.take() else {
            return false;
        };
        let pending_text = self.pending_local_text.take();
        if accepted && self.channel_ready {
            self.write_direction_ready = true;
            self.accepted_local_text =
                pending_text.filter(|text| offer.has_text && text.generation == offer.generation);
        } else {
            self.accepted_local_text = None;
        }
        self.queued_local_text.is_some()
    }

    pub(crate) fn observe_remote_formats(&mut self, formats: &[ClipboardFormat]) -> Option<u64> {
        let generation = self.allocate_generation();
        self.remote_unicode = formats
            .iter()
            .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT);
        self.pending_remote_offer = Some(RemoteOffer {
            generation,
            has_unicode: self.remote_unicode,
        });
        if self.outstanding_remote_request.is_some() {
            None
        } else {
            self.remote_unicode.then_some(generation)
        }
    }

    pub(crate) fn accept_local_payload(&mut self, payload: ClipboardPayload) -> Option<u64> {
        if !self.capabilities().clipboard_write {
            return None;
        }
        let Ok(text) = std::str::from_utf8(payload.as_bytes()) else {
            return None;
        };
        if text.contains('\0') {
            return None;
        }
        if self.pending_local_offer.is_some() || self.queued_local_text.is_some() {
            self.queued_local_text = Some(text.to_owned());
            return None;
        }
        Some(self.begin_local_text_offer(text.to_owned()))
    }

    fn begin_local_text_offer(&mut self, text: String) -> u64 {
        let generation = self.allocate_generation();
        self.pending_local_offer = Some(LocalOffer {
            generation,
            has_text: true,
        });
        self.pending_local_text = Some(LocalText {
            generation,
            value: text,
        });
        self.accepted_local_text = None;
        generation
    }

    fn begin_queued_local_offer(&mut self) -> Option<u64> {
        if self.pending_local_offer.is_some() {
            return None;
        }
        let text = self.queued_local_text.take()?;
        Some(self.begin_local_text_offer(text))
    }

    pub(crate) fn reject_local_offer(&mut self, generation: u64) {
        if self
            .pending_local_offer
            .is_some_and(|offer| offer.generation == generation)
        {
            self.pending_local_offer = None;
            self.pending_local_text = None;
            self.accepted_local_text = None;
        }
    }

    pub(crate) fn accepted_local_generation(&self) -> Option<u64> {
        self.accepted_local_text
            .as_ref()
            .map(|text| text.generation)
    }

    pub(crate) fn local_unicode_response(
        &self,
        generation: u64,
    ) -> Option<OwnedFormatDataResponse> {
        if self.accepted_local_generation() != Some(generation) {
            return None;
        }
        let text = self.accepted_local_text.as_ref()?;
        Some(FormatDataResponse::new_unicode_string(&text.value))
    }

    pub(crate) fn remote_request_generation(&self) -> Option<u64> {
        if !self.channel_ready || self.outstanding_remote_request.is_some() {
            return None;
        }
        self.pending_remote_offer
            .filter(|offer| offer.has_unicode)
            .map(|offer| offer.generation)
    }

    pub(crate) fn can_begin_remote_request(&self, generation: u64) -> bool {
        self.remote_request_generation() == Some(generation)
    }

    pub(crate) fn begin_remote_request(&mut self, generation: u64) -> bool {
        if !self.can_begin_remote_request(generation) {
            return false;
        }
        self.outstanding_remote_request = self.pending_remote_offer.take();
        true
    }

    fn accept_remote_response(
        &mut self,
        response: FormatDataResponse<'_>,
    ) -> RemoteResponseOutcome {
        let Some(owner) = self.outstanding_remote_request.take() else {
            return RemoteResponseOutcome::default();
        };
        if let Some(follow_up) = self.pending_remote_offer {
            let follow_up_generation = if self.channel_ready && follow_up.has_unicode {
                Some(follow_up.generation)
            } else {
                self.pending_remote_offer = None;
                None
            };
            return RemoteResponseOutcome {
                payload: None,
                follow_up_generation,
            };
        }
        let payload = if owner.has_unicode && !response.is_error() {
            decode_strict_unicode_text(response.data())
                .map(|text| ClipboardPayload::new(text.into_bytes()))
        } else {
            None
        };
        RemoteResponseOutcome {
            payload,
            follow_up_generation: None,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

fn decode_strict_unicode_text(data: &[u8]) -> Option<String> {
    if data.len() < 2 || !data.len().is_multiple_of(2) || data[data.len() - 2..] != [0, 0] {
        return None;
    }
    let body = &data[..data.len() - 2];
    let mut units = Vec::new();
    units.try_reserve_exact(body.len() / 2).ok()?;
    for bytes in body.chunks_exact(2) {
        let unit = u16::from_le_bytes([bytes[0], bytes[1]]);
        if unit == 0 {
            return None;
        }
        units.push(unit);
    }
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use frd_protocol_api::ClipboardPayload;
    use ironrdp::cliprdr::pdu::{
        Capabilities, ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags,
        ClipboardPdu, ClipboardProtocolVersion, FormatDataResponse, FormatList, FormatListResponse,
    };
    use ironrdp::core::encode_vec;
    use ironrdp::svc::SvcProcessor;

    use super::{
        capabilities, new_cliprdr, next_service_action, write_text, ClipboardAdapter,
        ClipboardServiceAction, RdpClipboardBackend,
    };

    fn process_server_clipboard_pdu(
        cliprdr: &mut ironrdp::cliprdr::CliprdrClient,
        pdu: ClipboardPdu<'_>,
    ) {
        let bytes = encode_vec(&pdu).expect("server CLIPRDR PDU encodes");
        cliprdr
            .process(&bytes)
            .expect("server CLIPRDR PDU processes");
    }

    #[test]
    fn clipboard_capabilities_track_the_two_negotiated_text_directions_independently() {
        let mut adapter = ClipboardAdapter::default();

        adapter.begin_initial_offer();
        adapter.observe_channel_ready();
        adapter.observe_local_format_response(true);
        assert!(!adapter.capabilities().clipboard_read);
        assert!(adapter.capabilities().clipboard_write);

        adapter.observe_remote_formats(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
        assert!(adapter.capabilities().clipboard_read);
        assert!(adapter.capabilities().clipboard_write);

        let mut early_remote = ClipboardAdapter::default();
        early_remote
            .observe_remote_formats(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
        assert!(!early_remote.capabilities().clipboard_read);
        early_remote.observe_channel_ready();
        assert!(early_remote.capabilities().clipboard_read);
    }

    #[test]
    fn clipboard_unnegotiated_or_non_text_payload_is_ignored() {
        let mut adapter = ClipboardAdapter::default();
        assert!(adapter
            .accept_local_payload(ClipboardPayload::new(b"text".to_vec()))
            .is_none());

        adapter.begin_initial_offer();
        adapter.observe_channel_ready();
        adapter.observe_local_format_response(true);
        assert!(adapter
            .accept_local_payload(ClipboardPayload::new(vec![0xFF]))
            .is_none());
        assert!(adapter
            .accept_local_payload(ClipboardPayload::new(b"bad\0text".to_vec()))
            .is_none());
        adapter.observe_remote_formats(&[ClipboardFormat::new(ClipboardFormatId::CF_DIB)]);
        assert!(!adapter.capabilities().clipboard_read);
    }

    #[test]
    fn clipboard_maps_utf8_payloads_to_and_from_unicode_text_only() {
        let mut adapter = ClipboardAdapter::default();
        adapter.begin_initial_offer();
        adapter.observe_channel_ready();
        adapter.observe_local_format_response(true);
        let local_generation = adapter
            .accept_local_payload(ClipboardPayload::new(
                "FreeRemoteDesk 中文".as_bytes().to_vec(),
            ))
            .expect("valid local text begins an owned offer");
        assert!(adapter.local_unicode_response(local_generation).is_none());
        adapter.observe_local_format_response(true);

        let local = adapter
            .local_unicode_response(local_generation)
            .expect("accepted current offer is served");
        assert_eq!(
            local.to_unicode_string().expect("valid Unicode response"),
            "FreeRemoteDesk 中文"
        );

        let remote_generation = adapter
            .observe_remote_formats(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
            .expect("remote Unicode offer generation");
        assert!(adapter.begin_remote_request(remote_generation));
        let remote = adapter
            .accept_remote_response(FormatDataResponse::new_unicode_string("远程文本"))
            .payload
            .expect("Unicode text is published");
        assert_eq!(remote.as_bytes(), "远程文本".as_bytes());
    }

    #[test]
    fn clipboard_rejects_unsolicited_replayed_and_stale_owned_data() {
        let mut adapter = ClipboardAdapter::default();
        adapter.begin_initial_offer();
        adapter.observe_channel_ready();
        adapter.observe_local_format_response(true);

        let remote_generation = adapter
            .observe_remote_formats(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
            .expect("remote Unicode offer generation");
        assert!(adapter
            .accept_remote_response(FormatDataResponse::new_unicode_string("unsolicited"))
            .payload
            .is_none());
        assert!(adapter.begin_remote_request(remote_generation));
        assert!(adapter
            .accept_remote_response(FormatDataResponse::new_unicode_string("owned"))
            .payload
            .is_some());
        assert!(adapter
            .accept_remote_response(FormatDataResponse::new_unicode_string("replayed"))
            .payload
            .is_none());

        let first_local = adapter
            .accept_local_payload(ClipboardPayload::new(b"first".to_vec()))
            .expect("first local offer");
        adapter.observe_local_format_response(true);
        assert!(adapter.local_unicode_response(first_local).is_some());
        let second_local = adapter
            .accept_local_payload(ClipboardPayload::new(b"second".to_vec()))
            .expect("second local offer");
        assert!(adapter.local_unicode_response(first_local).is_none());
        assert!(adapter.local_unicode_response(second_local).is_none());
        adapter.observe_local_format_response(false);
        assert!(adapter.local_unicode_response(second_local).is_none());

        adapter.reset();
        assert!(!adapter.capabilities().clipboard_read);
        assert!(!adapter.capabilities().clipboard_write);
        assert!(adapter.local_unicode_response(second_local).is_none());
    }

    #[test]
    fn clipboard_requires_one_terminal_utf16_nul_and_no_trailing_data() {
        let mut adapter = ClipboardAdapter::default();
        adapter.begin_initial_offer();
        adapter.observe_channel_ready();
        adapter.observe_local_format_response(true);

        for malformed in [
            vec![0x41, 0x00, 0x00],
            vec![0x41, 0x00],
            vec![0x41, 0x00, 0x00, 0x00, 0x00, 0x00],
            vec![0x41, 0x00, 0x00, 0x00, 0x42, 0x00, 0x00, 0x00],
            vec![0x00, 0xD8, 0x00, 0x00],
        ] {
            let generation = adapter
                .observe_remote_formats(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
                .expect("remote Unicode offer generation");
            assert!(adapter.begin_remote_request(generation));
            assert!(adapter
                .accept_remote_response(FormatDataResponse::new_data(malformed))
                .payload
                .is_none());
        }
    }

    #[test]
    fn clipboard_real_processor_negotiates_and_owns_one_unicode_request() {
        let mut cliprdr = new_cliprdr();
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::Capabilities(Capabilities::new(
                ClipboardProtocolVersion::V2,
                ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES,
            )),
        );
        process_server_clipboard_pdu(&mut cliprdr, ClipboardPdu::MonitorReady);
        assert!(matches!(
            next_service_action(&mut cliprdr).expect("initial offer encodes"),
            Some(ClipboardServiceAction::Wire(_))
        ));
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatListResponse(FormatListResponse::Ok),
        );
        assert!(capabilities(&cliprdr).clipboard_write);

        let formats = [ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatList(
                FormatList::new_unicode(&formats, true).expect("format list encodes"),
            ),
        );
        assert!(capabilities(&cliprdr).clipboard_read);
        assert!(matches!(
            next_service_action(&mut cliprdr).expect("Unicode request encodes"),
            Some(ClipboardServiceAction::Wire(_))
        ));

        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatDataResponse(FormatDataResponse::new_unicode_string(
                "owned response",
            )),
        );
        let Some(ClipboardServiceAction::Publish(payload)) =
            next_service_action(&mut cliprdr).expect("owned response maps")
        else {
            panic!("owned Unicode response must publish once")
        };
        assert_eq!(payload.as_bytes(), b"owned response");

        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatDataResponse(FormatDataResponse::new_unicode_string("replay")),
        );
        assert!(
            next_service_action(&mut cliprdr)
                .expect("replay is ignored")
                .is_none(),
            "a response without an outstanding adapter request must not publish"
        );
    }

    #[test]
    fn clipboard_real_processor_serializes_crossed_local_offers() {
        let mut cliprdr = new_cliprdr();
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::Capabilities(Capabilities::new(
                ClipboardProtocolVersion::V2,
                ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES,
            )),
        );
        process_server_clipboard_pdu(&mut cliprdr, ClipboardPdu::MonitorReady);
        assert!(matches!(
            next_service_action(&mut cliprdr).expect("initial offer encodes"),
            Some(ClipboardServiceAction::Wire(_))
        ));
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatListResponse(FormatListResponse::Ok),
        );

        assert!(
            write_text(&mut cliprdr, ClipboardPayload::new(b"offer A".to_vec()))
                .expect("offer A encodes")
                .is_some()
        );
        let owner_a = cliprdr
            .downcast_backend::<RdpClipboardBackend>()
            .and_then(|backend| backend.adapter.pending_local_offer)
            .expect("offer A owns the wire exchange")
            .generation;
        assert!(
            write_text(&mut cliprdr, ClipboardPayload::new(b"offer B".to_vec()))
                .expect("offer B queues")
                .is_none(),
            "offer B must not be sent while offer A awaits its response"
        );

        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatListResponse(FormatListResponse::Ok),
        );
        let accepted_a = cliprdr
            .downcast_backend::<RdpClipboardBackend>()
            .and_then(|backend| backend.adapter.local_unicode_response(owner_a))
            .expect("A response accepts only offer A");
        assert_eq!(
            accepted_a.to_unicode_string().expect("offer A Unicode"),
            "offer A"
        );
        assert!(matches!(
            next_service_action(&mut cliprdr).expect("queued B starts after A response"),
            Some(ClipboardServiceAction::Wire(_))
        ));

        let owner_b = cliprdr
            .downcast_backend::<RdpClipboardBackend>()
            .and_then(|backend| backend.adapter.pending_local_offer)
            .expect("offer B now owns the wire exchange")
            .generation;
        assert_ne!(owner_a, owner_b);
        assert!(cliprdr
            .downcast_backend::<RdpClipboardBackend>()
            .and_then(|backend| backend.adapter.local_unicode_response(owner_b))
            .is_none());
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatListResponse(FormatListResponse::Ok),
        );
        let accepted_b = cliprdr
            .downcast_backend::<RdpClipboardBackend>()
            .and_then(|backend| backend.adapter.local_unicode_response(owner_b))
            .expect("B response accepts offer B");
        assert_eq!(
            accepted_b.to_unicode_string().expect("offer B Unicode"),
            "offer B"
        );
    }

    #[test]
    fn clipboard_real_processor_serializes_crossed_remote_requests() {
        let mut cliprdr = new_cliprdr();
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::Capabilities(Capabilities::new(
                ClipboardProtocolVersion::V2,
                ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES,
            )),
        );
        process_server_clipboard_pdu(&mut cliprdr, ClipboardPdu::MonitorReady);
        assert!(matches!(
            next_service_action(&mut cliprdr).expect("initial offer encodes"),
            Some(ClipboardServiceAction::Wire(_))
        ));
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatListResponse(FormatListResponse::Ok),
        );

        let formats = [ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
        let remote_offer = || {
            ClipboardPdu::FormatList(
                FormatList::new_unicode(&formats, true).expect("format list encodes"),
            )
        };
        process_server_clipboard_pdu(&mut cliprdr, remote_offer());
        assert!(matches!(
            next_service_action(&mut cliprdr).expect("request A encodes"),
            Some(ClipboardServiceAction::Wire(_))
        ));

        process_server_clipboard_pdu(&mut cliprdr, remote_offer());
        assert!(
            next_service_action(&mut cliprdr)
                .expect("offer B is coalesced behind request A")
                .is_none(),
            "request B must not be sent before response A"
        );
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatDataResponse(FormatDataResponse::new_unicode_string("response A")),
        );
        assert!(matches!(
            next_service_action(&mut cliprdr).expect("request B starts after response A"),
            Some(ClipboardServiceAction::Wire(_))
        ));
        process_server_clipboard_pdu(
            &mut cliprdr,
            ClipboardPdu::FormatDataResponse(FormatDataResponse::new_unicode_string("response B")),
        );
        let Some(ClipboardServiceAction::Publish(payload)) =
            next_service_action(&mut cliprdr).expect("response B publishes")
        else {
            panic!("only the response owned by request B may publish")
        };
        assert_eq!(payload.as_bytes(), b"response B");
    }
}
