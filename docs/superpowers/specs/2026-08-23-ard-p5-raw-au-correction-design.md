# ARD P5 Mode-4 Raw-AU Correction Design

**Status:** Approved correction derived from the definitive negative live gate
**Date:** 2026-08-23
**Scope:** PC-to-Mac deterministic tone only; Windows microphone remains closed

## Evidence decision

Local read-only AVConference analysis proves the following chain:

```text
RemoteMicrophone mode 4
  -> audio stream mode 7
  -> bundling scheme 3
  -> raw AAC-ELD access unit as RTP payload
  -> PT 101, timestamp step 480
```

`VCPacketBundler` adds the RFC 3640 AU header only for scheme 2. Scheme 3 copies
the encoded access unit byte-for-byte into the RTP payload. The authenticated
Apple Mac-to-PC capture agrees: its four-byte AAC-ELD DTX payload remains four
bytes. The separate synthetic mono RTP fixture also locks the local regression
boundary at 143 bytes rather than 147 bytes; it is not claimed as an Apple live
capture.

The current Rust PC-to-Mac sender incorrectly applies the scheme-2 four-byte
header before SRTP. Apple accepts the packet into RTP/JB accounting but then
receives a polluted raw AAC-ELD access unit. This is the strongest explanation
for zero-loss reception with no native sound.

## Correction

- `MediaTransport::send_audio_access_unit` sends the encoder access unit
  byte-for-byte to the RTP/SRTP layer.
- Keep payload type 101, timestamp increment 480, media direction 2, mode 4,
  SSRC/sequence/ROC handling, SRTP/SRTCP, and the 500-frame bound unchanged.
- Retain scheme-2 RFC 3640 code only as an explicitly non-mode-4 test helper if
  it remains useful; it must not be reachable from the mode-4 sender.
- Add a transport-level test that decrypts the emitted packet and proves exact
  raw payload equality, including the four-byte DTX access unit.
- Do not open or construct Windows microphone capture. A corrected deterministic
  tone must pass native Mac audibility before the microphone plan can resume.

## Live gate

The next bounded run may claim success only when authenticated in-range SRTCP,
stock receive/decode/playout evidence where available, user-confirmed Mac
audibility, and clean teardown all pass. Blank UDP video remains a separate
unimplemented decode path and must not be reported as rendered.
