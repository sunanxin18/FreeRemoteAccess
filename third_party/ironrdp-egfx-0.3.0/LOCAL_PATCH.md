# FreeRemoteDesk local patch

This directory vendors the pinned `ironrdp-egfx` 0.3.0 source used by the RDP
client. The upstream crate is retained under its Apache-2.0/MIT licenses.

FreeRemoteDesk's patch includes `GraphicsPipelineClient`'s
AVC420 presentation boundary: after decoding the macroblock-aligned bounding
frame, the client validates the wire `regionRects` surface-coordinate mask and
emits one `BitmapUpdate` for each exclusive region. The AVC metadata parser is
also corrected to model `RDPGFX_RECT16` with `ExclusiveRectangle`; the public
server-side `Avc420Region` helper keeps its inclusive bounds and converts them
at the encoding boundary. Empty or inverted exclusive rectangles are rejected
while decoding the PDU, before any payload is dispatched. The existing renderer contract is preserved. The decoder trait also exposes a default
health query so a fallible reset can stop the client before a new reset reaches
the handler. The failure is terminal for the current EGFX generation: trailing
PDUs in the same DVC payload are short-circuited and cannot recreate surfaces or
notify the handler. This prevents pixels outside the server's region mask from
being published while preserving the upstream payload shape and public handler/
renderer contracts.

The client handler additionally exposes a default no-op `on_frame_started(frame_id)`
callback. The existing StartFrame dispatch invokes it after establishing the current
frame ID, before bitmap dispatch and the existing `on_frame_complete` callback.
This supplies the outer EGFX frame lifecycle without duplicating ZGFX decompression
or parsing. Existing handlers remain source-compatible. Tests use encoded PDUs
through the DVC process path and check callback order, malformed-payload rejection,
the disabled-decoder gate, and default-handler compatibility.
