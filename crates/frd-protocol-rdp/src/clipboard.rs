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

#[derive(Debug, Default)]
pub(crate) struct ClipboardAdapter {
    channel_ready: bool,
    remote_unicode: bool,
    local_format_accepted: bool,
    local_text: String,
}

#[derive(Debug)]
enum ClipboardAction {
    AdvertiseUnicode,
    RequestUnicode,
    RespondUnicode,
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
        if self.adapter.capabilities().clipboard_read {
            self.actions.push_back(ClipboardAction::RequestUnicode);
        }
    }

    fn on_request_format_list(&mut self) {
        self.actions.push_back(ClipboardAction::AdvertiseUnicode);
    }

    fn on_format_list_response(&mut self, accepted: bool) {
        self.adapter.observe_local_format_response(accepted);
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
    }

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        self.adapter.observe_remote_formats(formats);
        if self.adapter.capabilities().clipboard_read {
            self.actions.push_back(ClipboardAction::RequestUnicode);
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.actions
            .push_back(if request.format == ClipboardFormatId::CF_UNICODETEXT {
                ClipboardAction::RespondUnicode
            } else {
                ClipboardAction::RespondError
            });
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if let Some(payload) = self.adapter.accept_remote_response(response) {
            self.actions.push_back(ClipboardAction::Publish(payload));
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

pub(crate) fn write_text(
    cliprdr: &mut CliprdrClient,
    payload: ClipboardPayload,
) -> ironrdp::pdu::PduResult<Option<CliprdrSvcMessages<Client>>> {
    let accepted = cliprdr
        .downcast_backend_mut::<RdpClipboardBackend>()
        .is_some_and(|backend| backend.adapter.accept_local_payload(payload));
    if !accepted {
        return Ok(None);
    }
    let formats = [ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
    cliprdr.initiate_copy(&formats).map(Some)
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
        ClipboardAction::AdvertiseUnicode => {
            let formats = [ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
            cliprdr
                .initiate_copy(&formats)
                .map(ClipboardServiceAction::Wire)
                .map(Some)
        }
        ClipboardAction::RequestUnicode => cliprdr
            .initiate_paste(ClipboardFormatId::CF_UNICODETEXT)
            .map(ClipboardServiceAction::Wire)
            .map(Some),
        ClipboardAction::RespondUnicode => {
            let response = cliprdr
                .downcast_backend::<RdpClipboardBackend>()
                .map(|backend| backend.adapter.local_unicode_response())
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
    pub(crate) fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            clipboard_read: self.channel_ready && self.remote_unicode,
            clipboard_write: self.channel_ready && self.local_format_accepted,
            ..SessionCapabilities::default()
        }
    }

    pub(crate) fn observe_channel_ready(&mut self) {
        self.channel_ready = true;
    }

    pub(crate) fn observe_local_format_response(&mut self, accepted: bool) {
        self.local_format_accepted = self.channel_ready && accepted;
    }

    pub(crate) fn observe_remote_formats(&mut self, formats: &[ClipboardFormat]) {
        self.remote_unicode = formats
            .iter()
            .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT);
    }

    pub(crate) fn accept_local_payload(&mut self, payload: ClipboardPayload) -> bool {
        if !self.capabilities().clipboard_write {
            return false;
        }
        let Ok(text) = std::str::from_utf8(payload.as_bytes()) else {
            return false;
        };
        self.local_text.clear();
        self.local_text.push_str(text);
        true
    }

    pub(crate) fn local_unicode_response(&self) -> OwnedFormatDataResponse {
        FormatDataResponse::new_unicode_string(&self.local_text)
    }

    pub(crate) fn accept_remote_response(
        &self,
        response: FormatDataResponse<'_>,
    ) -> Option<ClipboardPayload> {
        if !self.capabilities().clipboard_read || response.is_error() {
            return None;
        }
        let text = response.to_unicode_string().ok()?;
        Some(ClipboardPayload::new(text.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use frd_protocol_api::ClipboardPayload;
    use ironrdp::cliprdr::pdu::{ClipboardFormat, ClipboardFormatId, FormatDataResponse};

    use super::ClipboardAdapter;

    #[test]
    fn clipboard_capabilities_track_the_two_negotiated_text_directions_independently() {
        let mut adapter = ClipboardAdapter::default();

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
        assert!(!adapter.accept_local_payload(ClipboardPayload::new(b"text".to_vec())));

        adapter.observe_channel_ready();
        adapter.observe_local_format_response(true);
        assert!(!adapter.accept_local_payload(ClipboardPayload::new(vec![0xFF])));
        adapter.observe_remote_formats(&[ClipboardFormat::new(ClipboardFormatId::CF_DIB)]);
        assert!(!adapter.capabilities().clipboard_read);
    }

    #[test]
    fn clipboard_maps_utf8_payloads_to_and_from_unicode_text_only() {
        let mut adapter = ClipboardAdapter::default();
        adapter.observe_channel_ready();
        adapter.observe_local_format_response(true);
        assert!(adapter.accept_local_payload(ClipboardPayload::new(
            "FreeRemoteDesk 中文".as_bytes().to_vec(),
        )));

        let local = adapter.local_unicode_response();
        assert_eq!(
            local.to_unicode_string().expect("valid Unicode response"),
            "FreeRemoteDesk 中文"
        );

        adapter.observe_remote_formats(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
        let remote = adapter
            .accept_remote_response(FormatDataResponse::new_unicode_string("远程文本"))
            .expect("Unicode text is published");
        assert_eq!(remote.as_bytes(), "远程文本".as_bytes());
    }
}
