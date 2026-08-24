# FreeRemoteAccess vendor note

This directory contains `ironrdp-client` 0.1.0 from crates.io. The downloaded
crate archive had SHA-256
`e2a3b44f8af101cf5a8cc5bf80470d64099c95c553d5597dec55ff38a821243e`.
The release source is <https://crates.io/crates/ironrdp-client/0.1.0>.

FreeRemoteAccess carries a narrow local patch which separates the TCP connect
address from the RDP destination identity:

- `Config` and `ConfigBuilder` accept an optional pinned `SocketAddr`.
- direct TCP transport uses that socket address when present.
- TLS continues to use `Config::destination().name()` as the server identity.

No authentication or certificate-verification behavior is changed here. The
upstream project is <https://github.com/Devolutions/IronRDP> and declares
`MIT OR Apache-2.0`; this vendored copy is used under the MIT terms reproduced
in `LICENSE-MIT`.

Task 18 is expected to pin the IronRDP `f1d53c7` certificate API. That upgrade
must explicitly port this `connect_addr` separation to the selected git source;
it must not silently restore hostname resolution inside the transport.
