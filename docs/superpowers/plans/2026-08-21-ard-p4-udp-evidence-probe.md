# ARD P4 UDP Evidence Probe Implementation Plan

> **Superseded correction (2026-08-22):** `0x08` is SetServerScaling and
> `3fe6`/`3fed`/`3fee` are IEEE-754 scale prefixes, not subtypes. Do not execute
> the older pseudo-subtype experiments below. The current evidence boundary and
> implementation path are defined in
> `../specs/2026-08-22-ard-media-audio-p3-p6-design.md`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a fail-closed fixture and UDP capture harness that can recover
the real Apple `3fe6`, `0x1c`, and UDP negotiation evidence without guessing
production protocol fields.

**Architecture:** A small Python module owns exact `0x08` parsing, explicit
response profiles, and sanitized fixture persistence. The fake server consumes
that module, while a separate UDP sink records opaque datagrams. Read-only
Ghidra scripts supply slice-specific static candidates before any response
fixture is promoted.

**Tech Stack:** Python 3 standard library and `unittest`, existing Python SRP
helpers, Ghidra 12.1.2 Java scripts, Rust 2021 regression matrix.

**Spec:** `docs/superpowers/specs/2026-08-21-ard-p4-udp-evidence-probe.md`

## Global Constraints

- Treat `3fe6` as unknown until an exact static consumer or real reply proves
  its semantics.
- Unknown `0x08` subtypes fail closed and never reuse another subtype template.
- Do not infer or implement `0x1c`, SRTP, reliable-UDP, audio, or video fields
  in `src/` during this plan.
- Persist raw bytes plus sanitized, relative metadata; never persist targets,
  credentials, command lines, encryption keys, or absolute paths.
- Read the fake-server password only from `FRD_FAKE_SS_PASSWORD`.
- This checkout has no `.git`; each task ends with recorded RED/GREEN commands
  rather than commits.

---

### Task 1: Exact Query Parsing and Fail-Closed Response Profiles

**Files:**
- Create: `ard_re/udp_probe.py`
- Create: `ard_re/test_udp_probe.py`

**Interfaces:**
- Produces `Query08(subtype: int, echo: bytes, frame: bytes)`.
- Produces `parse_query08(frame: bytes) -> Query08 | None`.
- Produces `ResponseTemplates.from_pickle_data(data: dict)`.
- Produces `Query08Responder(profile: str, templates: ResponseTemplates)` and
  `response_for(query: Query08) -> bytes | None`.

- [ ] **Step 1: Write failing parser and response tests**

Add literal tests for an exact `3fe6` request, eleven/thirteen-byte rejection,
wrong-prefix rejection, exact known-template substitution, and unknown subtype
returning `None`. The unknown-subtype test must fail if the responder falls
through to `3fee`.

```python
def test_unknown_subtype_never_reuses_known_template(self):
    responder = Query08Responder("captured-known-only", self.templates)
    query = parse_query08(bytes.fromhex("08003fe60000000000000000"))
    self.assertIsNotNone(query)
    self.assertIsNone(responder.response_for(query))
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `python -m unittest ard_re.test_udp_probe -v`

Expected: import failure because `ard_re.udp_probe` does not exist.

- [ ] **Step 3: Implement the minimal parser and responder**

Implement exact-length parsing, profiles `observe-only` and
`captured-known-only`, template length/offset validation, and a subtype map
containing only `0x3fed` and `0x3fee`.

- [ ] **Step 4: Run the focused test and verify GREEN**

Run: `python -m unittest ard_re.test_udp_probe -v`

Expected: all Task 1 tests pass.

### Task 2: Sanitized Decrypted Fixture Store

**Files:**
- Modify: `ard_re/udp_probe.py`
- Modify: `ard_re/test_udp_probe.py`

**Interfaces:**
- Produces `FixtureStore(root: Path, session_label: str)`.
- Produces `record(direction: str, payload: bytes, classification: str) -> Path`.
- Appends `manifest.jsonl` entries containing only `sequence`, `session`,
  `direction`, `classification`, `length`, `sha256`, and `file`.

- [ ] **Step 1: Write failing persistence and sanitation tests**

Use `tempfile.TemporaryDirectory`. Assert exact binary round-trip, a literal
SHA-256 value, monotonically increasing filenames, relative manifest paths,
and rejection of session labels or classifications containing an IPv4 address,
path separator, newline, or credential-like delimiter.

- [ ] **Step 2: Run the fixture tests and verify RED**

Run: `python -m unittest ard_re.test_udp_probe.FixtureStoreTests -v`

Expected: failure because `FixtureStore` is not defined.

- [ ] **Step 3: Implement atomic fixture persistence**

Create the root directory, validate neutral labels, write payload to a
sequence-prefixed `.bin` file, and append one compact JSON object with relative
POSIX filename. Use `hashlib.sha256` and never serialize environment values.

- [ ] **Step 4: Run the fixture tests and verify GREEN**

Run: `python -m unittest ard_re.test_udp_probe.FixtureStoreTests -v`

Expected: all fixture tests pass.

### Task 3: Integrate Evidence Capture into the Fake Server

**Files:**
- Modify: `ard_re/fake_ss.py`
- Create: `ard_re/test_fake_ss_udp_probe.py`
- Modify: `ard_re/test_secret_sanitation.py`

**Interfaces:**
- `FakeServer(password, response_profile, fixture_store)` receives injected
  non-network dependencies in tests.
- `observe_client_frame(data: bytes) -> tuple[Query08 | None, Path]` records
  every decrypted frame and queues only exact `0x08` queries.
- CLI accepts `--port`, `--response-profile`, and `--fixture-dir`; password is
  required through `FRD_FAKE_SS_PASSWORD`.

- [ ] **Step 1: Write failing integration tests**

Instantiate the real fake-server class without listening. Feed exact `3fed`,
`3fe6`, malformed `0x08`, and `0x1c`-prefixed frames. Assert each is persisted
once, only exact queries are queued, `3fed` receives a response, and `3fe6`
does not. Add a subprocess CLI test proving a positional password is rejected
and missing `FRD_FAKE_SS_PASSWORD` fails without echoing a value.

- [ ] **Step 2: Run integration tests and verify RED**

Run: `python -m unittest ard_re.test_fake_ss_udp_probe -v`

Expected: failures because dependency injection and explicit profiles do not
exist and argv still accepts a password.

- [ ] **Step 3: Implement the minimal fake-server integration**

Replace accumulated-byte substring counting with an exact-query queue populated
from decrypted frames. Route responses through `Query08Responder`, preserve
existing known templates, record every decrypted client frame, and migrate the
CLI to `argparse` plus `FRD_FAKE_SS_PASSWORD`.

- [ ] **Step 4: Run integration and security tests and verify GREEN**

Run: `python -m unittest ard_re.test_fake_ss_udp_probe ard_re.test_local_config ard_re.test_secret_sanitation -v`

Expected: all tests pass and no credential appears in output.

### Task 4: Opaque UDP Datagram Sink

**Files:**
- Create: `ard_re/udp_sink.py`
- Create: `ard_re/test_udp_sink.py`

**Interfaces:**
- Produces `UdpSink(bind_host: str, port: int, store: FixtureStore,
  timeout: float, max_datagrams: int)`.
- `bound_port() -> int` returns the selected port after bind.
- `receive() -> int` records up to the configured count and returns the number
  captured; timeout returns normally.

- [ ] **Step 1: Write failing real-socket tests**

Bind the sink to loopback port zero, send two literal datagrams from a real UDP
socket, and assert byte-exact fixture order and count. Add timeout and maximum
count tests; no socket mock is permitted.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `python -m unittest ard_re.test_udp_sink -v`

Expected: import failure because `ard_re.udp_sink` does not exist.

- [ ] **Step 3: Implement the minimal sink and CLI**

Use `socket.socket(AF_INET, SOCK_DGRAM)`, bind once, record payloads as
`direction="udp-in"`, close through a context manager, and expose non-secret
CLI flags for bind host, port, timeout, maximum count, output directory, and
session label.

- [ ] **Step 4: Run the socket tests and verify GREEN**

Run: `python -m unittest ard_re.test_udp_sink -v`

Expected: all real-socket tests pass.

### Task 5: Slice-Specific Ghidra Evidence and Operator Runbook

**Files:**
- Create: `ard_re/FindScalarRefs.java`
- Create: `ard_re/P4_UDP_EVIDENCE.md`
- Modify: `ard_re/NOTES.md`

**Interfaces:**
- `FindScalarRefs.java 0x3fe6` prints program name, language/compiler identity,
  instruction address, containing function, mnemonic, and operand text.
- `P4_UDP_EVIDENCE.md` separates verified facts, static candidates, dynamic
  observations, rejected hypotheses, and the next unmet acceptance gate.

- [ ] **Step 1: Verify the missing static tool is RED**

Run headless Ghidra with `-readOnly -noanalysis -postScript FindScalarRefs.java 0x3fe6`.

Expected: failure because the script does not exist.

- [ ] **Step 2: Implement and run the scalar-reference script**

Iterate real instructions and their scalar operands. Print only exact scalar
matches and containing functions, preceded by `currentProgram.getName()`,
language ID, compiler spec ID, and executable format. Run it independently on
each imported Mach-O slice; do not merge zero-hit and positive-hit conclusions.

- [ ] **Step 3: Trace property writers and `0x1c` callees**

Run `FindStringRefs.java` for `audioStreamUDPPort`, video UDP port properties,
their setters, `startAVCMediaStreams`, and media configuration. Dump containing
functions and the server callees at the already recovered `0x1c` path. Record
missing references and malformed GhidraMCP status explicitly.

- [ ] **Step 4: Write the evidence ledger and operator commands**

Document the exact fake-server environment variables and non-secret arguments,
the UDP sink startup command, the single required Mac reconnection action, and
fixture hashes after capture. Use placeholders such as `<MAC_HOST>` and never
include a real target or credential.

- [ ] **Step 5: Run the complete verification matrix**

Run: `python -m unittest discover -s ard_re -p "test_*.py" -v`

Run: `cargo fmt -- --check`

Run: `cargo test --no-default-features`

Run: `cargo test`

Run: `cargo build --no-default-features`

Run: `cargo build --release`

Expected: all commands exit zero. Report existing compiler warnings separately
from failures. Do not claim live UDP or `0x1c` success until the operator run
has produced the corresponding fixtures.
