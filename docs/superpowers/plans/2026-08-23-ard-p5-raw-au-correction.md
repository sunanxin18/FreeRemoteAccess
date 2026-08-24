# ARD P5 Mode-4 Raw-AU Correction Plan

**Goal:** Remove the incorrect scheme-2 RFC 3640 prefix from the mode-4 audio
sender, verify the exact raw-AU wire contract, rebuild in the isolated target,
and repeat the bounded deterministic native-playback gate.

1. Add a failing transport test proving a mode-4 DTX access unit must remain
   byte-for-byte unchanged in the decrypted RTP payload.
2. Change only the PC-to-Mac send path to pass the raw access unit directly to
   RTP/SRTP; correct stale scheme-2 comments and keep microphone guards closed.
3. Run focused audio/transport tests, format, default and no-default suites,
   both builds, help output, and an isolated release build.
4. Review the exact source and release hashes.
5. Run one bounded live tone test while the user is present. Require native Mac
   audibility and clean close; do not interpret SRTCP alone as playback.
6. Only after that gate passes, return to the separately reviewed microphone
   cutover. Message-2 semantic offer/answer binding remains a fail-closed
   hardening task and is not silently claimed by this correction.
