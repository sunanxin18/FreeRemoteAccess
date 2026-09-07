# IronRDP Connector

Abstract state machine to drive an RDP connection sequence.

This crate is part of the [IronRDP] project.

FreeRemoteDesk carries this pinned 0.10.0 source as a minimal local patch. The
only behavioral change is an opt-in `Config::support_dynvc_gfx_protocol` field;
when enabled, the connector sets the RDP early capability bit required for the
server to open the EGFX dynamic virtual channel. The application sets it only
when an exact EGFX decoder provider is attached, so legacy sessions keep the
upstream capability set.

[IronRDP]: https://github.com/Devolutions/IronRDP
