# FreeRemoteDesk local patch

This directory vendors the pinned `ironrdp-egfx` 0.3.0 source used by the RDP
client. The upstream crate is retained under its Apache-2.0/MIT licenses.

FreeRemoteDesk's patch is intentionally limited to `GraphicsPipelineClient`'s
AVC420 presentation boundary: after decoding the macroblock-aligned bounding
frame, the client validates the wire `regionRects` surface-coordinate mask and
emits one `BitmapUpdate` for each exclusive region. The AVC metadata parser is
also corrected to model `RDPGFX_RECT16` with `ExclusiveRectangle`; the public
server-side `Avc420Region` helper keeps its inclusive bounds and converts them
at the encoding boundary. Empty or inverted exclusive rectangles are rejected
while decoding the PDU, before any payload is dispatched. The public handler and
renderer contracts do not change. The decoder trait also exposes a default
health query so a fallible reset can stop the client before a new reset reaches
the handler. This prevents pixels outside the server's region mask from being
published while preserving the upstream payload shape and public handler/
renderer contracts.
