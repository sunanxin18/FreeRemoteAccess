# Cold MVS Capture V2 Design

## Status and purpose

This document defines a client-only, cold-start MVS capture path for the
existing username/password HPSS client. Its output must prove, under the local
writer contract, that capture was created before a new TCP session, armed and
durably flushed before the first HPSS trigger, and terminated without a hidden
assembler gap.

The design addresses the boundary established by the existing
`FRDMVS01` audit: V1 preserves complete MVS records and rectangles, but it has
no session-start, generation, gap, or terminal provenance. A V1 file remains
useful historical input; it can never be promoted to cold-start evidence.

This feature does not install or run anything on the Mac, use Apple ID, start a
GUI, invoke a server-side helper, or enable UDP media. It changes no Apple wire
semantics. It only makes the local client's capture provenance explicit and
strictly readable.

## Approaches considered

### 1. Selected: one `FRDMVS02` framed event log plus `stdin-v1`

Use one create-new file containing a fixed header, ordered lifecycle/data
events, and exactly one terminal event. A dedicated capture subcommand receives
all connection material in one bounded binary stdin frame. This couples the
cold-start attestation, records, gaps, and terminal counts in the same object;
editing, truncating, concatenating, or partially copying the file invalidates
strict reading. A complete byte-for-byte copy preserves, but does not upgrade,
the original provenance.

This is selected because it provides the smallest self-contained artifact and
lets tests inject I/O failures at every state transition. It retains the
existing MVS record payload unchanged and requires no Cargo dependency. A
byte-for-byte copy retains the same provenance; editing, truncating,
concatenating, or relabelling bytes cannot create a new session identity.

### 2. Rejected: unchanged `FRDMVS01` plus a JSON sidecar

A sidecar would be easy to inspect, but the two files have no atomic identity.
They can be separated, mismatched, or copied independently, and a sidecar
cannot represent the exact position of an assembler gap without introducing a
second ordering system. A digest would bind bytes but still leave lifecycle
ordering split across files.

### 3. Rejected: extend or restamp `FRDMVS01`

Appending lifecycle markers to V1 would make old readers interpret control
data as records or trailing corruption. A wrapper may truthfully carry V1
bytes only with `HistoricalUnproven` provenance; it cannot produce a V2
`Created`/`Armed` attestation or satisfy cold acceptance. Changing or
restamping V1 in place is therefore forbidden; V2 has a new magic and a strict
reader.

## Constants and byte order

All multibyte integers are unsigned big-endian. Byte strings have no implicit
terminator or padding. Every reserved field and undefined flag bit must be zero.
A strict reader rejects, rather than skips, unknown versions, event types,
flags, or nonzero reserved bytes.

| Name | Exact value |
| --- | ---: |
| File magic | ASCII `FRDMVS02` (8 bytes) |
| Header bytes | 32 |
| Format major/minor | 2 / 0 |
| Endian identifier | 1 = big-endian |
| Checksum identifier | 0 = no embedded checksum |
| Maximum MVS payload | `0x01000000` = 16 MiB |
| Maximum event body | `0x0100001c` = 16 MiB + 28 bytes |
| Maximum event bytes | `0x0100003c` = 32-byte prefix + maximum body |
| Maximum capture records | 4096 |
| Maximum events | 4102 |
| Maximum cumulative MVS payload | `0x20000000` = 512 MiB |
| Maximum capture duration | 30000 ms |
| Incomplete-record lifetime | 2000 ms |
| Socket read timeout | 100 ms |

V2 deliberately has no per-event or file checksum. Exact lengths, ordinals,
state transitions, counts, and terminal EOF detect truncation, insertion,
deletion, and reordering under the writer contract; they do not prove
detection of an arbitrary same-length bit flip. A post-close SHA-256 may be
recorded in sanitized task metadata as an artifact identifier, but it is not
part of the V2 validity contract. Adding an embedded checksum requires a new
format magic, not reinterpretation of checksum identifier zero.

## Exact `FRDMVS02` file header

The header is exactly 32 bytes.

| Offset | Bytes | Field | Required value |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | `FRDMVS02` |
| 8 | 2 | header length | 32 |
| 10 | 1 | major version | 2 |
| 11 | 1 | minor version | 0 |
| 12 | 1 | endian identifier | 1 |
| 13 | 1 | checksum identifier | 0 |
| 14 | 2 | format flags | `0x0001`; bit 0 means strict-cold, all other bits zero |
| 16 | 4 | maximum MVS payload | `0x01000000` |
| 20 | 4 | maximum event body | `0x0100001c` |
| 24 | 4 | maximum capture records | 4096 |
| 28 | 4 | maximum capture duration in ms | 30000 |

The writer emits the header once to a file opened with `create_new`. No valid
V2 file may contain another header or concatenate a second capture. The event
and cumulative-payload limits are fixed format rules derived outside the
header: at most 4096 records plus four lifecycle events, one optional gap, and
one terminal event gives 4102 events. A structural diagnostic `Surface` event
uses one of those 4102 slots rather than increasing the limit.

## Common event prefix

Every event starts with this exact 32-byte prefix.

| Offset | Bytes | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 4 | event length | total prefix plus body; 32 through `0x0100003c` |
| 4 | 2 | event type | one of the exact types below |
| 6 | 2 | event flags | zero |
| 8 | 8 | event ordinal | starts at zero and increases by exactly one |
| 16 | 8 | generation | current generation under the rules below |
| 24 | 8 | monotonic microseconds | elapsed from `Created`; nondecreasing |

The reader validates the event length before allocating or slicing the body.
`Created` has timestamp zero. Wall-clock time is intentionally absent. The
writer captures one `Instant` immediately before writing the header and uses
only checked elapsed-microsecond conversion from that origin. Conversion
overflow is terminal output failure. Timestamps must not be synthesized from
wall-clock time or reset after `Created`.

## Event types and exact bodies

### `0x0001 Created` — 16-byte body

| Offset | Bytes | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 4 | deadline ms | one of 5000, 10000, 15000, 20000, 30000 |
| 4 | 4 | record limit | 1 through 4096 |
| 8 | 2 | credential-provider version | 1 |
| 10 | 2 | capture policy | 1 = cold TCP MVS |
| 12 | 4 | reserved | zero |

`Created` is ordinal zero, generation zero, timestamp zero. The file header and
this event are written and flushed before attempting TCP connect.

### `0x0002 Armed` — 24-byte body

| Offset | Bytes | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 2 | committed width | exact authenticated initial surface; nonzero |
| 2 | 2 | committed height | exact authenticated initial surface; nonzero |
| 4 | 2 | requested width | planned initial `0x09`/`0x03` width; nonzero |
| 6 | 2 | requested height | planned initial `0x09`/`0x03` height; nonzero |
| 8 | 2 | transport profile | 1 = Apple TCP MVS |
| 10 | 2 | authentication profile | 1 = local username/password |
| 12 | 4 | arm flags | exactly `0x00000003` |
| 16 | 8 | reserved | zero |

Arm flag bit 0 means a new TCP connection was created after `Created`. Bit 1
means the capture writer is installed before HPSS triggers. Both committed and
requested surfaces must satisfy the existing framebuffer pixel budget.

The writer may enter `Armed` only after TCP authentication succeeds, the exact
initial committed geometry is available from the authenticated RFB session,
and the planned request geometry is known. Missing, zero, or resource-invalid
committed geometry terminates `Aborted` reason 266. Requested geometry may
differ from committed geometry, but the request is not a commit and does not
change the active geometry used for record validation. Both geometry pairs
must independently satisfy the existing framebuffer pixel budget.

The writer writes `Armed`, calls `flush`, then calls `sync_data` on the file.
No `0x1d`, `0x09`, or `0x03` write is permitted until both operations succeed.

### `0x0003 Triggered` — 16-byte body

| Offset | Bytes | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 2 | requested width | equals `Armed` requested width |
| 2 | 2 | requested height | equals `Armed` requested height |
| 4 | 4 | successful trigger mask | exactly `0x00000007` |
| 8 | 8 | first MVS assembler-input-frame ordinal | zero |

Trigger-mask bits 0, 1, and 2 respectively attest successful writes of the
initial `0x1d`, `0x09`, and nonincremental `0x03`. The event is written only
after all three writes succeed and after the required post-write deadline
samples remain before the deadline. A failure before this event is written is
terminal `Aborted` 260 and produces no `Triggered`; a later failure may leave a
structurally visible `Triggered` followed by `Aborted` 260.

### `0x0004 Recording` — 8-byte body

The sole body field is `next MVS assembler-input-frame ordinal`, an unsigned
64-bit value that must be zero. The `Recording` event is written and flushed
after `Triggered` and before the first network application-frame read; later
non-MVS frames do not advance this counter. Serializing this event does not by
itself enter the in-memory `Recording` state. The transition occurs atomically
only after that flush succeeds and the immediately following deadline sample is
strictly before the deadline.

### `0x0010 Surface` — 8-byte body

| Offset | Bytes | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 2 | new surface width | nonzero |
| 2 | 2 | new surface height | nonzero |
| 4 | 2 | transition reason | 1 = exact server geometry commit |
| 6 | 2 | reserved | zero |

The prefix generation must be the previous generation plus exactly one. A
`Surface` event is structurally legal only while the assembler is idle and
after the exact geometry commit has atomically reset the decoder/surface
generation. Its dimensions must satisfy the framebuffer pixel budget. A
generation change while an MVS record is pending emits a `Gap` and terminates
`Aborted` instead.

The general bounded structural-diagnostic reader may retain `Surface` for
future diagnostics, but it never returns cold provenance. The strict-cold
policy rejects every `Surface` event. The dedicated launcher has dynamic
resolution disabled, so an accepted cold artifact stays at generation zero and
uses the authenticated committed geometry from `Armed` for every record. A
request/committed-size mismatch is allowed as an observation; it remains
uncommitted and cannot enlarge or replace the active geometry.

### `0x0020 Record` — 28-byte header plus payload

| Offset | Bytes | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 8 | first source-frame ordinal | first MVS assembler-input frame used by this record |
| 8 | 8 | last source-frame ordinal | final MVS assembler-input frame; at least first |
| 16 | 2 | rectangle x | MVS rectangle |
| 18 | 2 | rectangle y | MVS rectangle |
| 20 | 2 | rectangle width | MVS rectangle |
| 22 | 2 | rectangle height | MVS rectangle |
| 24 | 4 | payload length | 1 through 16 MiB |
| 28 | N | payload | exact complete MVS payload, unchanged |

The common event length equals `60 + payload length`. The payload length must
equal the remaining event body exactly. Payload byte zero must be type 0, 1,
or 2; other tags are rejected by the strict cold reader.

The source ordinal counts only frames submitted as input to the MVS assembler.
Heartbeat, server state, media-answer, cursor, audio, and other non-MVS frames
do not consume an ordinal. A one-frame MVS record has equal first and last
values. For a fragmented record, the assembler stores the first ordinal and
the final continuation ordinal.

The receiver assigns the next ordinal immediately after classifying a frame as
an MVS begin or assembler continuation and before calling the assembler. Thus
an offending continuation-without-pending or new-begin-while-pending frame has
an ordinal and is included in `Gap.last`. A frame rejected before it can be
classified as an MVS assembler input consumes no MVS ordinal and cannot produce
a format-2.0 `Gap`; it is handled as a non-MVS/read error outside this event.

In a clean file, record ranges must continuously and exactly cover source
ordinals `0..footer.source_mvs_frame_count`: the first record starts at zero,
each following record starts at the previous last plus one using checked
arithmetic, and the final last plus one equals the footer count. Overlap,
reversal, or a hole is invalid.

Type-2 records require an all-zero rectangle. Type-0 and type-1 records require
a nonzero rectangle whose checked right/bottom bounds fit the active committed
surface, never merely the requested geometry; their record-local pixel area
must satisfy the existing MVS decode budget. Every complete
type-2/type-0/type-1 record is written before codec classification, including a
payload that later fails codec parsing.

### `0x0021 Gap` — 40-byte body

| Offset | Bytes | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 2 | reason | exact enum below |
| 2 | 2 | stage | exact enum below |
| 4 | 4 | frame-range flags | exactly 1: bit 0 = range present |
| 8 | 8 | first source-frame ordinal | first pending/offending MVS input |
| 16 | 8 | last source-frame ordinal | offending/latest MVS input |
| 24 | 4 | declared total | exact wire value for reason 2; current total for pending reasons; zero if unavailable |
| 28 | 4 | accumulated bytes | bytes accepted before the offending frame/event |
| 32 | 8 | x/y/width/height | four big-endian u16 values; zero if unavailable |

Format 2.0 requires frame-range-present and rejects every other flags value.
Neither ordinal may equal `u64::MAX`, first must not exceed last, and the range
includes the offending MVS input frame when one exists. For a pending record
timeout/deadline/read/generation/cancellation failure, it covers every MVS input
already assigned to that pending record. There is no no-frame Gap encoding in
format 2.0. The accumulated-byte count never includes bytes from the offending
frame. For reason 2, declared total may be zero or exceed 16 MiB because
preserving the invalid u32 is the diagnostic; for pending reasons it must equal
the already validated total. `Accumulated` is not the session
cumulative-payload counter.

The complete reason mapping is fixed below. `Pfirst`/`Plast` are the first and
latest already assigned ordinals of the pending record; `O` is the newly
assigned offending MVS input ordinal. `Ptotal`, `Pbytes`, and `Prect` are the
pending record's validated declared total, accepted byte count, and rectangle.
`Ctotal`/`Crect` are the offending begin candidate's declared total/rectangle.

| Reason | Condition | Stage | First / last | Declared | Accumulated | Rectangle | Terminal |
| ---: | --- | ---: | --- | --- | --- | --- | ---: |
| 1 | fixed media header identifies MVS but the envelope is invalid before a total is available | 1 | `O / O` | 0 | 0 | parsed candidate rectangle, otherwise all zero | 262 |
| 2 | declared total is zero or exceeds 16 MiB | 1 | `O / O` | exact invalid candidate u32 | 0 | `Crect` | 262 |
| 3 | first fragment length exceeds a valid candidate total | 1 | `O / O` | `Ctotal` | 0 | `Crect` | 262 |
| 4 | continuation length exceeds remaining pending bytes | 2 | `Pfirst / O` | `Ptotal` | `Pbytes` before offending continuation | `Prect` | 262 |
| 5 | pending age reaches 2000 ms before the capture deadline | 3 | `Pfirst / Plast` | `Ptotal` | `Pbytes` | `Prect` | 262 |
| 6 | absolute capture deadline is reached while pending | 4 | `Pfirst / Plast` | `Ptotal` | `Pbytes` | `Prect` | 263 |
| 7 | socket read fails while pending before the deadline | 5 | `Pfirst / Plast` | `Ptotal` | `Pbytes` | `Prect` | 261 |
| 8 | exact generation/geometry commit is attempted while pending | 6 | `Pfirst / Plast` | `Ptotal` | `Pbytes` | `Prect` | 266 |
| 9 | continuation is supplied with no pending record | 2 | `O / O` | 0 | 0 | all zero | 262 |
| 10 | an unambiguous new MVS begin is supplied while another record is pending | 1 | `Pfirst / O` | `Ptotal` | `Pbytes` before offending begin | `Prect` | 262 |
| 11 | at begin, checked cumulative payload plus candidate total would exceed 512 MiB | 8 | `O / O` | `Ctotal` | 0 | `Crect` | 262 |
| 12 | operator cancellation while a record is pending | 7 | `Pfirst / Plast` | `Ptotal` | `Pbytes` | `Prect` | 265 |

For reason 11 the writer performs the budget check after reading and validating
`Ctotal` and `Crect`, but before accepting or copying any candidate fragment
byte into the assembler; its range therefore contains only the offending begin
and accumulated is exactly zero. An unambiguous caller attempt to begin a new
media-envelope record while pending uses reason 10; opaque continuation bytes
are never speculatively reclassified as a begin merely because they resemble a
header. Operator cancellation while pending emits reason 12 and then
`Aborted` 265. Operator cancellation while the assembler is idle emits
`Aborted` 265 directly, with no `Gap`. Adding reason 12 does not increase the
4102-event cap: a capture still reserves one existing slot for its terminal,
and a gap consumes one of the remaining slots.

The first gap ends the evidentiary capture: the writer emits exactly one
`Gap`, then `Aborted`, then stops reading. A strict cold reader rejects every
file containing a `Gap`, even if a syntactically valid terminal follows. The
bounded structural reader requires the mapped terminal in the table to be the
immediately following event; a different reason, intervening event, missing
terminal, or second gap is structurally invalid.

### `0x00fe Clean` and `0x00ff Aborted` — shared 48-byte footer body

| Offset | Bytes | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 2 | terminal reason | enum scoped to terminal type |
| 2 | 2 | reserved | zero |
| 4 | 4 | gap count | zero for `Clean`; zero or one for `Aborted` |
| 8 | 8 | source MVS assembler-input-frame count | next source ordinal |
| 16 | 8 | record count | number of `Record` events |
| 24 | 8 | type-0 count | derived from payload byte zero |
| 32 | 8 | type-1 count | derived from payload byte zero |
| 40 | 8 | type-2 count | derived from payload byte zero |

The three type counts must sum exactly to record count and match the events.
The footer gap count, record count, tag counts, and MVS input-frame count are
recomputed from parsed event ranges; serialized counters are never trusted.
For `Clean`, record ranges alone cover exactly `0..source count`. For
`Aborted` with a `Gap`, the nonoverlapping record-prefix ranges followed by the
mandatory frame-bearing gap range cover the same interval exactly. Format 2.0
has no no-frame `Gap`. `Aborted` without a gap uses its record-prefix ranges
alone and may have zero records.
`Clean` reasons are 1 deadline reached while the assembler was idle and 2
record limit reached immediately after a complete record. `Aborted` reasons
are: 257 connect failure, 258 credential-input/authentication failure, 259 arm
write/flush failure, 260 trigger-sequence failure or deadline, 261 read
failure, 262 assembler/gap failure, 263 pending at deadline, 264 output
failure, 265 operator cancellation, 266 invalid geometry/generation
transition, and 267 pre-trigger deadline reached.

The terminal event is the last event. EOF must follow it immediately. A
partially written event, missing terminal, duplicate terminal, trailing byte,
or concatenated capture is invalid.

### Derived event sizes

These totals include the 32-byte common prefix and are part of the literal
schema tests.

| Event | Body bytes | Total event bytes |
| --- | ---: | ---: |
| `Created` | 16 | 48 |
| `Armed` | 24 | 56 |
| `Triggered` | 16 | 48 |
| `Recording` | 8 | 40 |
| `Surface` | 8 | 40 |
| `Record` | `28 + N` | `60 + N`; 61 through `0x0100003c` |
| `Gap` | 40 | 72 |
| `Clean` / `Aborted` | 48 | 80 |

The largest body remains a maximum-size `Record`: 16 MiB plus 28 bytes equals
`0x0100001c`; adding the prefix yields `0x0100003c`. Expanding `Armed` does not
change either maximum.

## Capture state machine

```text
create_new + Header + Created + flush
                  |
                  v
              Created
                  |
           new TCP + authentication
                  |
        Armed + flush + sync_data
                  v
               Armed
                  |
         attempt first 0x1d write
                  |
              Triggering
                  |
       send 0x09 and full 0x03,
       write Triggered, write + flush Recording,
       sample deadline, atomically enter state
                  v
             Recording
                  |
   success: select Clean / failure: optional Gap,
                 then select Aborted
                  v
              Finalizing
                  |
       terminal write -> into_inner flush
       -> File sync_data -> relinquish handle
                  |
            Clean or Aborted

`Triggering` is an in-memory phase, not an event type. Any deadline or I/O
failure from the first `0x1d` attempt through the successful post-`Recording`
flush sample selects `Aborted` 260 and joins `Finalizing`. Other failures or
cancellation in Created, Armed, or Triggering select their mapped `Aborted`
reason and also join `Finalizing`; no path jumps directly to an already-final
terminal state.
```

`Clean` is selectable only from `Recording`. `Aborted` is selectable from any
nonterminal state. Selecting either enters `Finalizing`; the in-memory state is
not terminal until finalization finishes. Once terminal, every API operation
fails without writing.

Finalization has one exact order: write the chosen terminal event, consume the
`BufWriter` with `into_inner` and handle its implicit flush result, call
`File::sync_data`, then relinquish/drop the file handle. A write, flush, or
`sync_data` failure makes the child exit nonzero. If output I/O fails so the
writer cannot append or durably flush `Aborted`, the missing/uncertain terminal
cannot be reported as successful.

Rust's standard `File` drop does not expose a later operating-system close
error, so the design does not claim to detect such an error. Overall capture
success requires all observable finalization calls to succeed, the launcher to
wait for child exit status zero, and the launcher to reopen the relinquished
file and pass both bounded structural validation and strict-cold validation.
An artifact that happens to contain `Clean` after a child finalization failure
is not accepted by that run.

### Deadline and assembler rules

- The deadline is created with checked `Instant::checked_add` from the same
  origin used for event timestamps. Failure to represent it aborts before
  connect. No new read starts at or after the deadline.
- Pre-trigger checks occur, in order, after the stdin provider returns,
  immediately before connect, immediately after connect, immediately after
  authentication, immediately before writing `Armed`, immediately after
  `Armed` flush/`sync_data`, and immediately before the first trigger write.
  If any check observes `now >= deadline`, no trigger begins and the writer
  selects `Aborted` reason 267 through `Finalizing`.
- Connect and every authentication read/write are bounded by the remaining
  absolute deadline, not merely an independent fixed timeout. Before each
  blocking operation the implementation computes checked remaining duration;
  zero remaining selects reason 267. A non-deadline connect failure selects
  257; invalid credential input or a non-deadline authentication failure
  selects 258.
- After a connect/auth operation returns, deadline classification precedes
  ordinary error classification: a remaining-deadline timeout or a return at
  `now >= deadline` selects 267; only a return before the deadline may select
  257/258. The implementation may not start an unbounded DNS, connect,
  handshake, authentication read, or authentication write and then rely on a
  later check.
- Reason 267 is exclusively a pre-trigger deadline: it is available only until
  immediately before the first attempted `0x1d` write. The first `0x1d`
  attempt atomically enters `Triggering` and permanently makes reason 267
  unavailable for that run.
- From the first attempted `0x1d` write until atomic entry into `Recording`,
  every observed deadline and every network or capture-event write/flush I/O
  failure selects `Aborted` reason 260. If the output failure also prevents the
  selected terminal from being persisted, finalization still exits nonzero as
  specified above; it is not reclassified as a successful capture.
- Deadline samples in `Triggering` are mandatory: immediately after each
  successful `0x1d`, `0x09`, and nonincremental `0x03` write; immediately
  before writing `Triggered`; immediately after successfully writing
  `Triggered`; and immediately after writing and flushing `Recording`. Every
  sample requires `now < deadline`; equality or later selects 260.
- A failure or deadline before the `Triggered` write produces no `Triggered`
  event. A failure at the post-`Triggered` sample or during/after the
  `Recording` write may leave the already completed event or events in the
  stream before `Aborted` 260. In particular, even a successfully flushed
  `Recording` event does not enter the in-memory state when its following
  sample is at or after the deadline.
- Atomic entry into `Recording` occurs only when every trigger write, the
  `Triggered` event write, the `Recording` event write/flush, and the
  post-flush sample have succeeded with that sample strictly before the
  deadline. `Clean` reason 1 is legal only after this transition; a pre-entry
  timeout can never be encoded as `Clean`.
- In `Recording`, loop-top priority is absolute deadline first: pending emits
  reason-6 `Gap` + `Aborted` 263, while idle selects `Clean` 1. Only when the
  deadline has not arrived does the loop test the 2000 ms incomplete lifetime
  and emit reason-5 `Gap` + `Aborted` 262. Only after both checks may a new
  read start.
- A requested 100 ms socket timeout bounds idle polling under the socket API so
  the loop does not rely on more server traffic. It is not a real-time or
  operating-system scheduling guarantee; correctness comes from checking the
  absolute deadline whenever control returns.
- Incomplete age starts at the first MVS fragment and is never refreshed by
  continuations.
- Operator cancellation observed in `Recording` is classified from the
  assembler state at that observation: pending emits reason-12 `Gap` followed
  immediately by `Aborted` 265; idle selects `Aborted` 265 directly with no
  gap. Cancellation never reuses the deadline or incomplete-lifetime reasons.
- Post-frame processing has one mandatory order. First finish classifying the
  received frame, apply it to the assembler, and, if it completes a record,
  write the entire `Record` event. Then sample one `now`/timestamp. If the
  assembler is pending and `now >= deadline`, emit reason-6 `Gap` + `Aborted`
  263. Otherwise, if it is idle and `now >= deadline`, select `Clean` 1. Only
  when `now < deadline` may the writer test `record_count == record_limit` and
  select `Clean` 2. No other ordering is valid.
- Record count may never exceed the limit. `Clean` 1 permits a recomputed count
  from zero through the limit and requires terminal timestamp greater than or
  equal to the deadline. `Clean` 2 requires terminal timestamp strictly less
  than the deadline and recomputed count exactly equal to the limit.
- Invalid totals, overflow, continuation without a pending record, and a
  generation transition while pending emit their exact gap and terminate. No
  partial record is serialized as `Record`.
- The writer uses checked addition before retaining an event or payload. A
  4103rd event or cumulative MVS payload above 512 MiB cannot be stored. A
  complete incoming record that would cross the payload budget emits reason-11
  `Gap` covering its MVS input frames and terminates; event-count exhaustion
  before a terminal is an output failure and therefore cannot be accepted.
- The writer reserves one of the 4102 slots for a terminal at all times. Before
  writing `Gap` it reserves two slots, one for `Gap` and one for `Aborted`; if
  both are unavailable it performs no partial diagnostic write and fails
  output. Thus no valid writer path consumes the final slot with a
  nonterminal event.
- A clean capture never asks the decoder to recover from a gap and continue;
  diagnostics after a gap require a separate new create-new capture.

A record whose read began before the deadline may finish after it; the
mandatory post-frame sequence writes that exact complete record before
selecting `Clean` 1, and no subsequent read is allowed. All event timestamps
must be no greater than the terminal timestamp.

## Strict cold reader

V2 has two separate readers. `read_mvs_capture_v2_structural` is a bounded
diagnostic parser: it may return a syntactically valid `Surface`, `Gap`, or
`Aborted` capture, always labelled `DiagnosticOnly`. It enforces all framing,
counter, resource, generation, geometry, and terminal-consistency rules but
does not grant cold provenance. `read_mvs_capture_v2_strict_cold` first requires
that structural result and then applies the stricter policy below. Neither API
falls back to V1.

Both readers validate before storing: event count is checked against 4102,
event/body lengths before allocation, cumulative payload by checked addition
against 512 MiB, generation transition before state replacement, and geometry
plus record bounds before retaining the record payload. No partially validated
event or record escapes on a later error.

The strict-cold reader performs these checks before returning records:

1. Validate every header constant, reserved bit, limit, and exact event length
   before allocation.
2. Require the first four events to be `Created`, `Armed`, `Triggered`, and
   `Recording`, with ordinals 0 through 3 and all cross-field equalities above.
3. Require contiguous event ordinals, no more than 4102 total events,
   nondecreasing checked monotonic timestamps, generation zero throughout, and
   no `Surface` event.
4. Set active geometry only from `Armed` committed width/height. Validate every
   record rectangle against that committed geometry with checked arithmetic
   and the existing framebuffer/MVS resource budgets. `Armed` requested and
   `Triggered` geometry may differ but never becomes active.
5. Require record MVS-input-frame ranges to continuously cover ordinal zero
   through the recomputed footer count with no overlap, reversal, or hole.
6. Reject unknown payload tags, any `Gap`, any `Aborted`, missing/duplicate
   lifecycle events, missing `Clean`, counter disagreement, cumulative payload
   above 512 MiB, or bytes after `Clean`.
7. For `Clean` 1 require terminal timestamp at least the declared deadline and
   recomputed record count no greater than the declared limit. For `Clean` 2
   require terminal timestamp strictly before the deadline and recomputed
   count exactly equal to the limit. Reject `Clean` before `Recording`, and
   return records only after the complete file and terminal counters validate.

The strict result includes the authenticated committed initial surface,
requested geometry as noncommitting metadata, ordered generation-zero records,
deadline, record limit, and terminal summary. The diagnostic result may also
include bounded `Surface`/`Gap` information but can never be passed to a
cold-only API without revalidation. Offline replay consumes strict records in
their validated order through the existing production MVS state machine.

Structural validity and decoder validity remain separate: a cold V2 file can
be structurally valid while a captured MVS payload exposes a protocol/decoder
blocker. The reader must not relabel such a blocker as a capture gap.

## V1 historical boundary and migration

- The existing function remains byte-for-byte API compatible:
  `read_mvs_capture(data: &[u8]) -> Result<Vec<MvsRecord>>`. Its name,
  arguments, return type, V1 parsing, and legacy-error behavior do not change.
- `HistoricalUnproven` belongs only to a new diagnostic wrapper around that
  unchanged `Vec<MvsRecord>` result. It is not added to `MvsRecord`, the old
  return type, or old errors. Wrapped V1 may be replayed for diagnostics but
  cannot satisfy cold-start acceptance or establish that a missing
  cache/coefficient seed was absent on the wire.
- The strict V2 reader rejects V1 with a distinct version/provenance error.
- There is no V1-to-V2 converter, V2-magic wrapper, header prepend, or
  restamping path. A separate generic diagnostic package may contain V1 only
  if it preserves `HistoricalUnproven`; packaging never upgrades provenance.
- Existing local V1 fixtures remain read-only and are never overwritten.
- The existing general `hpss --out` behavior remains V1 during migration. The
  new dedicated subcommand is the only V2 producer until V2 implementation and
  live acceptance are reviewed. A later default change requires a separate
  decision.

## `stdin-v1` secret provider

The new child subcommand is invoked without a host, username, password, or
secret-environment variable:

```text
freeremotedesk hpss-capture-v2 --credentials-stdin-v1 --out <fresh-path> --seconds 30 --max-records 4096
```

The output path and numeric limits are non-secret. The subcommand has no
`--udp-media`, username-environment, password-environment, Apple ID, remote
command, or GUI option.

### Exact stdin frame

The child reads exactly one frame and then requires EOF. All integers are
big-endian.

| Offset | Bytes | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | ASCII `FRDSTD01` |
| 8 | 4 | payload length | `8 + host_len + username_len + password_len` |
| 12 | 2 | host length | 1 through 255 bytes |
| 14 | 2 | username length | 1 through 255 bytes |
| 16 | 2 | password length | 1 through 1024 bytes |
| 18 | 2 | TCP port | 1 through 65535 |
| 20 | host length | host | UTF-8 bytes |
| next | username length | username | UTF-8 bytes |
| next | password length | password | UTF-8 bytes |

The payload length is 11 through 1542 and the total frame is 23 through 1554
bytes. The child reads no more than 1554 bytes plus one probe byte used to
require EOF. Truncation, extra bytes, invalid UTF-8, an invalid length/port,
NUL, or ASCII control characters in host/username are errors. Host and
username may not have leading or trailing Unicode whitespace. Password bytes
are not trimmed or normalized; they must be nonempty valid UTF-8 and contain no
NUL. Errors name only the field category, never its value or length-derived
fingerprint.

### Source, transport, and zeroization

For this strict mode the launcher reads the ignored local credential file
directly. `FRD_CREDENTIALS_FILE` may select that file, but direct target,
username, and password environment variables are rejected so immutable
environment strings are not used as secret transport. Mode dispatch must
branch to this byte provider before calling the legacy string-returning
credential accessors; importing their module without calling them does not
load a secret and is permitted.

The credential-file byte parser is frozen as follows:

- Input is a regular file of 1 through 65536 bytes. It may begin with exactly
  one UTF-8 BOM; a BOM elsewhere is invalid.
- Line endings are LF or CRLF. A bare CR is invalid. Lines are processed as
  bytes so the complete file is never decoded to an immutable string.
- Empty/ASCII-space-or-tab-only lines, lines whose first non-space byte is `#`,
  and single-line comments whose trimmed bytes start with `<!--` and end with
  `-->` are ignored. Multiline HTML comments are unsupported and rejected.
- Other non-table prose lines are ignored and terminate any active table. A
  table-looking line is one whose first non-space byte is `|`; it is never
  silently treated as prose.
- Each table header is exactly the two cells `项目` and `值`, allowing only
  ASCII spaces/tabs around cell content. It must be followed immediately by a
  separator of exactly two cells containing three or more ASCII hyphens;
  alignment colons are not accepted. Multiple such tables are allowed.
- A data row has this exact cell grammar, with ASCII spaces/tabs allowed only
  around cell content: `| <field> | ` followed by one backtick, the value,
  one backtick, ASCII spaces/tabs, and the final `|`. There is no escape
  syntax: backslash is a literal byte and backtick cannot occur in a value. A
  line beginning with `|` that matches neither header, separator, nor this row
  grammar is invalid. A data row is valid only inside an active table after its
  separator; a blank, comment, heading, or prose line ends that table.
- Unrecognized well-formed fields are ignored. The complete recognized label
  set, copied from the current provider source and its tests, is frozen below;
  no fuzzy, case-folded, normalized, translated, or substring match is allowed.

  | Logical field | Exact accepted labels |
  | --- | --- |
  | host | `当前 Mac 主机`; `目标主机（Mac mini）` |
  | username | `当前 Mac 用户名`; `Mac 用户名` |
  | password | `Mac 登录密码`; `VNC 密码` |

  Exactly one alias row per logical field is required across all tables; a
  second row from either alias is a duplicate and fails even when its bytes are
  identical.
- The value bytes are taken verbatim between backticks and then checked against
  the stdin-frame UTF-8, length, whitespace, control, and NUL rules. No entity,
  backslash, Unicode-normalization, or Markdown escape processing occurs.

The launcher reads into a mutable raw-file byte array, copies only the three
recognized values into mutable field byte arrays, constructs the `stdin-v1`
frame in one mutable frame byte array, writes it through the child's anonymous
stdin pipe without cloning or formatting secret values, closes the pipe, and
explicitly overwrites the raw-file, field, and frame arrays in `finally`
blocks. It never prints or serializes the frame. The child environment is built
from a system allowlist and contains none of the direct or legacy credential
variables.

The child stores the frame in one guarded byte vector and borrows UTF-8 slices
from it for address parsing and authentication. Application-owned capture and
provider code in the new mode prohibits `clone`, `to_owned`, `to_string`,
formatting, or diagnostic interpolation of those slices. On every observable
success/error path it explicitly overwrites
every initialized byte with volatile zero writes followed by a sequentially
consistent compiler fence, immediately after authentication returns and again
idempotently in `Drop`. Tests use an injected buffer observer to prove clearing
of the explicitly owned guarded buffer.

The zeroization claim is limited to the launcher's mutable raw-file, field, and
frame arrays and the child's explicitly guarded input vector. Python/runtime
temporaries, standard-library address parsing, DNS resolver state,
authentication-library internals, anonymous-pipe/kernel buffers, allocator
copies, compiler/runtime copies outside those owned buffers, and remote
protocol handling cannot be proven zeroized by this design. They must not be
deliberately cloned or logged, but they are explicitly outside the overwrite
guarantee. The separately testable guarantee is that secret values occur zero
times in argv, child environment, application logs, metadata, reports, and
capture files.

## Launcher and output behavior

`ard_re/run_live_hpss.py` gains a dedicated `cold-mvs-v2` mode. It performs
only these actions:

1. Resolve and hash the already reviewed local executable using the existing
   approval gate.
2. Resolve an approved artifact root below `.superpowers/sdd`.
3. Create a new directory named
   `cold-mvs-v2/<UTC-compact>-<16-random-hex>` with exclusive directory
   creation, and choose `capture.mvs` inside it.
4. Read the local credential file into the bounded mutable provider, spawn the
   child command shown above with a sanitized environment and stdin pipe, write
   the frame, zero it, and wait for the bounded result.
5. Never pass `--udp-media`, never run a remote command or delayed worker, and
   never start `hpssview`.
6. Let the child open `capture.mvs` with `OpenOptions::create_new(true)` before
   reading stdin or connecting. A collision is terminal; no retry may select or
   overwrite an existing filename in the same directory.
7. Sanitize stdout/stderr before console or artifact writes. Persist only
   non-secret status, executable hash, capture hash after close, file size,
   structural counters, duration, and terminal category. Do not persist target,
   display name, username, authentication error detail, command representation,
   or stdin data.

The child writes Header + `Created` and flushes before parsing connection data
or connecting. If provider parsing, connect, or authentication fails, it
attempts the corresponding `Aborted` footer and returns a generic nonzero
status. After the child enters `Finalizing`, it exits zero only when terminal
write, `BufWriter::into_inner` flush, and `File::sync_data` have succeeded and
the file handle has been relinquished. The launcher waits for process exit,
then opens the path afresh. It reports overall success only when exit status is
zero, the bounded structural reader succeeds, and the strict-cold reader
accepts `Clean`. The design cannot observe a standard-library/OS close error
that is not reported by those operations.

## TDD and verification

Implementation must proceed in independent RED/GREEN slices.

### Container and reader tests

- Freeze literal header and every event body byte-for-byte, including maximum
  lengths and big-endian values.
- Accept payload length exactly 16 MiB without eagerly allocating it in the
  boundary test; reject zero and 16 MiB + 1 before allocation.
- Reject wrong magic/version/endian/checksum, nonzero reserved bits, unknown
  events/flags, invalid lengths, ordinal skip/duplicate/reorder, timestamp
  reversal, missing/duplicate terminal, truncation at every field boundary,
  and trailing bytes.
- Accept exactly 4102 structurally valid diagnostic events when their other
  limits permit; reject event 4103 before retaining it. Accept cumulative MVS
  payload exactly 512 MiB by streaming/sliced fixtures and reject the next byte
  through checked addition before storage.
- Prove V1 remains readable by the V1 API and is rejected by the strict cold
  API; prove diagnostic packaging remains `HistoricalUnproven` and no wrapper
  or conversion path can return cold provenance.
- Cover structural generation increment, skipped/repeated generation,
  surface-budget overflow, checked rectangle overflow, zero/nonzero table
  rectangles, and records crossing the active committed surface. Separately
  prove strict-cold rejects every `Surface`, including an otherwise exact one.
- Freeze the 24-byte `Armed` body, require authenticated committed geometry,
  allow requested mismatch without changing active geometry, and reject a
  record that fits only the requested dimensions.
- Prove non-MVS frames do not consume source ordinals; require record ranges to
  cover every MVS assembler-input ordinal continuously. Mutate first/last and
  footer count independently.
- Require the Gap range-present bit, reject zero/unknown flags and either
  ordinal equal to `u64::MAX`, and cover offending-frame inclusion,
  accepted-before-offending byte count, continuation-without-pending,
  new-begin-while-pending, and cumulative-budget gap reasons. No format-2.0
  fixture may encode a no-frame Gap.
- Cover terminal counter mutations independently for records, each tag, source
  frames, and gaps.
- Freeze all twelve gap-table rows independently, mutating stage, range bit,
  first/last, declared total, pre-failure accumulated bytes, rectangle, and
  mapped `Aborted` reason one field at a time. Reason 11 must prove the budget
  check occurs after candidate total/rectangle parse but before accepting its
  first byte. Reason 12 must freeze stage 7, the complete pending
  `Pfirst..Plast` range, `Ptotal`, `Pbytes`, `Prect`, and terminal 265.

### Writer/state tests

- Use injected file/socket operations to prove `Created` flush precedes
  connect, and `Armed` flush plus `sync_data` precede every trigger write.
- Mutate the order so a trigger occurs before successful arm flush and record a
  deterministic RED.
- Cover every legal state transition and every illegal transition.
- Cover checked monotonic conversion, checked deadline creation, no read at or
  after deadline, post-frame deadline finalization, and terminal timestamp
  rules for both clean reasons.
- Inject deadline expiry after provider return, before/after connect, after
  authentication, before `Armed`, after arm sync, and before first trigger; each
  must produce pre-trigger reason 267 without a trigger. Bound connect/auth
  operations to remaining deadline. Once the first trigger is attempted,
  inject expiry and I/O failure after each trigger write and immediately before
  `Triggered`; each must produce 260 with no `Triggered`. Inject them after the
  `Triggered` write and require 260 while permitting that completed event.
  Inject them during the `Recording` write/flush and after its successful flush;
  require 260, never enter the in-memory `Recording` state, and never emit
  `Clean` 1 even when a `Recording` event was serialized.
- Mutation tests must fail if reason 267 remains available after the first
  `0x1d` attempt, if any mandatory trigger-phase deadline sample is omitted, if
  the state enters `Recording` before the post-flush sample, or if a deadline at
  that sample is misclassified as `Clean` 1 instead of `Aborted` 260.
- Cover a one-frame record and a fragmented record with exact source-frame
  ranges, interleaved non-MVS frames that do not affect those ranges, all three
  payload tags, and write-before-classify ordering.
- Drive 50 ms continuations for two seconds and prove the loop-top lifetime
  aborts once rather than refreshing the age.
- Cover deadline idle -> `Clean`, deadline pending -> `Gap` + `Aborted`, record
  limit immediately after a complete record -> `Clean`, generation while
  pending -> `Gap` + `Aborted`, and output failure -> missing/aborted terminal
  that strict reading rejects.
- Cover operator cancellation pending -> exact reason-12 `Gap` + `Aborted` 265
  and operator cancellation idle -> direct `Aborted` 265 with no `Gap`. Mutate
  the pending path to omit the gap or use the idle path and require RED. Confirm
  both paths remain within the unchanged 4102-event cap.
- Freeze the post-frame priority with a record that completes exactly as both
  deadline and record limit are reached: write the complete `Record`, then
  choose `Clean` 1. A timestamp just before deadline with count equal to limit
  chooses `Clean` 2. Mutations that check the limit first must fail.
- Inject terminal write, `into_inner` flush, and `sync_data` failures. Prove the
  child cannot exit zero, the handle is relinquished on the success path, and
  launcher success additionally requires a fresh structural plus strict reopen
  after child exit.

### Provider/launcher tests

- Freeze a fake-value literal `FRDSTD01` frame and cover all three exact maxima,
  each maximum + 1, empty fields, payload mismatch, truncation, extra EOF byte,
  invalid UTF-8, NUL/control, whitespace rules, and port bounds.
- Freeze credential byte-parser fixtures for no-BOM/BOM, LF/CRLF, bare CR,
  headings/comments/prose table termination, multiple valid tables, row
  outside a table, exact separator syntax, unknown well-formed rows, malformed
  table rows, duplicate aliases, and literal/no-escape value bytes.
- Freeze each of the six exact recognized labels and reject normalized,
  case-folded, translated, substring, or duplicate-alias variants.
- Prove the explicitly owned launcher raw-file/field/frame arrays and child
  guarded vector are overwritten after success and every injected error. A
  mutation that omits any of those explicit overwrites must fail; tests and
  docs make no assertion about standard-library, DNS, auth, or kernel copies.
- Prove cold-mode dispatch occurs before legacy string credential parsing and
  that secret slices are never cloned, formatted, or interpolated.
- Use canary secrets to assert they occur zero times in argv, child environment,
  stdout, stderr, metadata, exception text, and capture bytes.
- Prove direct secret environment variables are rejected, while the local file
  path alone is accepted.
- Prove exclusive directory/file creation refuses collisions and never touches
  the historical fixture.
- Prove the cold command omits UDP, GUI, remote workers, and server-side actions.

### Acceptance sequence

1. Run format, focused V2/provider/launcher tests, full default and headless
   tests, and both build configurations.
2. Run a mock TCP session that emits type-2/type-0/type-1 and fragmented MVS
   records plus non-MVS control frames. Require strict cold reading, continuous
   MVS-only ordinals, exact recomputed counts, and clean EOF.
3. Build the reviewed release in an isolated target and copy it to the existing
   approved executable location; record only its SHA-256.
4. Run one bounded `cold-mvs-v2` client-only capture for 30 seconds or 4096
   complete records, whichever occurs first. Do not start GUI/UDP/remote code.
5. Require the strict reader to accept one `Clean` file with no gaps, generation
   zero, no `Surface`, authenticated committed-geometry consistency, and at
   least one type-2, one type-0, and one type-1 record. Acceptance requires the
   recorded child exit status plus fresh structural and strict reopen results,
   not a terminal byte sequence alone.
6. Replay the accepted records through the existing production decoder in
   order. Diagnose the first relevant type-1 cache/coefficient use using only
   safe record/tile/bit metadata and verify whether its seed-producing chain
   exists earlier in the same generation. A missing seed remains protocol
   evidence from a valid cold capture; it is not repaired by inventing state.
7. Generate a PNG only if the independent decoder acceptance gates pass. A
   structurally valid capture may still end with decoder acceptance blocked.
8. Run a content-suppressed scan whose output is only match counts and redacted
   paths. Required secret matches are zero.

## Migration and operational rollback

V2 is additive. If any V2 test or live gate fails, the new subcommand remains
unavailable for acceptance and the existing V1 writer/reader behavior is left
unchanged. Removing an incomplete V2 artifact is optional; preserving it as a
local diagnostic is safe because strict reading will report its terminal
failure, but it must never be renamed or reported as `Clean`.

No capture task may overwrite, edit, or restamp an existing artifact. Every
retry creates a new exclusive directory and a new TCP session. Decoder or
protocol changes discovered from a valid capture remain separate evidence-led
tasks.

## Security and non-goals

- Client-only stock Screen Sharing authentication and media requests.
- No Mac installation, modification, helper, command, log query, injection, or
  privileged action.
- No Apple ID/IDS flow.
- No UDP media, audio, viewer window, dynamic-resolution experiment, or
  server-side capture.
- No credential, target, display identifier, stdin frame, or raw MVS payload in
  source, logs, reports, metadata, argv, or child environment.
- No claim that V1 is cold, that V2 checksum-free bytes resist arbitrary
  corruption, or that a structurally valid V2 file proves decoder correctness.
