# Windows WGPU Session Chrome and Apple Parity Validation

Date: 2026-08-29

## Scope

This record validates the Windows-first winit/wgpu product shell after the
single-row session title bar and application icon work. It covers the shared
chrome model, official Material Symbols rendering, native Windows chrome,
remote-content geometry, packaged icons, and one bounded Apple HPSS/MVS
session. It does not claim runtime validation of the future macOS or Linux
product shells.

## Automated Verification

All Cargo commands used Cargo's normal default build parallelism. No explicit
job-count environment, command-line, or configuration override was present.

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo test --workspace` | Passed; all non-fixture tests green, documented local-fixture tests remained ignored |
| `cargo build --workspace --no-default-features` | Passed; only the pre-existing top-level MVS dead-code warnings were emitted |
| `cargo build -p freeremotedesk-windows --release` | Passed |
| `cargo clippy -p freeremotedesk-windows -p frd-shell-desktop -p frd-ui-egui -p frd-ui-model -p frd-icon-assets --all-targets --no-deps -- -D warnings` | Passed |

The final verified Release executable was 32,563,200 bytes with SHA-256
`DC68505F6F07F2395683020270164B9C5B114D17B7F83CE48304F75943E05A45`.
`ExtractIconExW` returned both a non-null large icon and a non-null small icon;
both handles were destroyed after inspection. The decoded 64-by-64 ICO entry
also matched the runtime RGBA asset byte for byte.

Focused coverage proves:

- each connection/capability/action state maps to a distinct official
  Material Symbols Rounded glyph and Simplified-Chinese accessible label;
- the 24dp, weight-400 subset is registered as an isolated egui named family;
- unavailable capability slots stay disabled and the four-slot geometry stays
  fixed;
- asymmetric Windows native controls do not move the session cluster away from
  the true window center;
- 100%, 150%, and 200% scale geometry produces the expected title-bar inset,
  content rectangle, hit regions, and pointer mapping;
- a DPI transition suppresses redraw and viewport publication until the OS
  physical size and new scale can be committed as one geometry; Windows DWM
  frame margins and resize metrics are refreshed for the same transition;
- maximized Windows never publish resize-edge hit results;
- the title bar cannot produce remote pointer events, while the first pixel
  below it maps to remote row zero;
- the executable contains large and small Windows icon resources and the
  runtime window icon uses the exact matching 64-by-64 RGBA artwork;
- connection diagnostics are included in the accessible name as well as the
  hover/focus tooltip.

The validation machine has one 192-DPI (200%) monitor. Live visual acceptance
therefore used 200% scaling; 100% and 150% are covered by literal geometry and
hit-test cases rather than by changing the user's global Windows display
setting.

## Material Symbols Evidence

The previous hand-drawn `paint_glyph` implementation was removed. The title
bar now uses an offline 3,532-byte subset of Google's Material Symbols Rounded
font at optical size 24, weight 400, fill 0, and grade 0. The subset contains
only the approved connection, audio, clipboard, cancel, disconnect, and idle
symbols. Its source commit, upstream and subset SHA-256 values, generation
parameters, and Apache License 2.0 text are recorded under `assets/ui-icons/`.

Release visual acceptance confirmed:

- `check_circle`, `volume_off`, `content_paste_off`, and `link_off` render with
  consistent rounded geometry and optical weight;
- the four 44-point slots are centered as one cluster and occupy 88 physical
  pixels each at 200% scaling;
- DWM remains responsible for minimize, maximize/restore, and close hit
  semantics, system tooltips, and Snap Layout on the Windows trailing side;
  because the full-frame WGPU surface covers the standard caption pixels, the
  shell draws only their conventional visual mirrors;
- the remote texture begins immediately below the one title-bar row and no
  second toolbar exists;
- hovering the audio symbol displays `远程音频不可用`;
- UI Automation exposes `已连接`, `远程音频不可用`, `剪贴板不可用`, and
  the actionable `断开连接`; every slot is keyboard-focusable and unavailable
  capabilities are disabled.

The final Release binary also completed the bounded offline texture/resize
driver and exited with code 0.

## Platform UI font fallback

The desktop shell now places the current platform UI font and the CJK font
matching the UI locale before egui defaults. The current `zh-Hans` Windows
chain is Segoe UI followed by Microsoft YaHei UI. A bundled Noto Sans SC
variable font at weight 400 is the final proportional and monospace
missing-glyph fallback; if no platform font can be read, it becomes the
proportional primary. Material Symbols remains an isolated named family.

The bundled font is 17,773,248 bytes with SHA-256
`E80613A35583F59B46DBF6CC2EB640F3DB0BB0F53FA7F6FBAA7B09FAF20E5172`.
Its SIL OFL 1.1 text was obtained from the official Google Fonts Noto Sans SC
directory and matches that source after normalizing trailing whitespace.
The Release test-texture run rendered the Simplified-Chinese UI, exercised a
resize, and exited with code 0, proving the complete font stack can be parsed
by the production egui path.

## Bounded Apple Session

Credentials were parsed only from the ignored local credential file into the
child process environment. They were not written to command arguments, logs,
captures, source, or this record. Exactly one Release client was launched.

Observed result:

1. The Apple HPSS/MVS adapter authenticated and reached `已连接`.
2. A real Mac desktop frame was presented inside the content rectangle with no
   title-bar overlap; black side letterboxing matched the remote aspect ratio.
3. The Material Symbols cluster and native Windows controls remained visible.
4. No remote keyboard or pointer test event was sent during this bounded run.
5. Invoking the accessible `断开连接` action returned to the single connection
   form, exposing only 地址、端口、目标系统、协议、用户名、密码和连接 controls.
6. A normal window-close request then terminated the process within the
   bounded wait.

## Mac-baseline/RDP integration refresh (2026-08-29)

The integration candidate was `35e5962`. Cargo used the normal default
parallelism. A fresh final offline round passed `fmt`, both complete workspace
test configurations (868 passed, 11 explicitly ignored local-fixture tests),
the no-default-feature workspace build, the Windows Release build, the full
planned `-D warnings` Clippy set, and the RDP forbidden-dependency/import
checks. The final rebuilt executable used for the bounded live comparison was
42,106,880 bytes with SHA-256
`F0A80A17150BD9E457DFBBDABD8B4070C294A98DCA0A0A215B44F646EB5B1A4B`.

Exactly one client was active at a time. The saved profile selected macOS and
the product resolved automatic selection to `apple-hpss-mvs`; no RDP session,
thread, or socket was created. The client authenticated, presented a correctly
colored complete 1440-by-2560 portrait surface below the 44-point title bar,
and continued to apply repeated type-0 and type-1 MVS updates without
reconnecting. The user confirmed both pointer and keyboard input. A normal
disconnect returned to the centered connection form, and normal close left no
client process. The focus-loss/cursor-leave release behavior was not separately
re-exercised in this refresh; its already verified shell path is unchanged by
the RDP integration.

One initial bounded session returned the generic `apple_runtime_failed` after
successful first-frame and pointer activity. A temporary local diagnostic
build did not reproduce an Apple runtime error: subsequent sessions sustained
continuous MVS updates and closed normally. The temporary diagnostic source
change was removed and was not committed. This remains a non-reproduced bounded
observation, not evidence of a resolved root cause.

The user also reported visibly higher input-to-refresh latency. A same-machine,
same-target A/B resource sample produced these observations:

| Variant | CPU seconds in 5 seconds | Working set | Private bytes | stderr growth |
| --- | ---: | ---: | ---: | ---: |
| Frozen Mac-only baseline `cc71206` | 5.750 | 336.5 MiB | 458.3 MiB | 32,422 bytes |
| Integrated candidate | 5.547 | 328.3 MiB | 449.5 MiB | 30,937 bytes |

This sample only shows that it found no obvious CPU, working-set, private-byte,
or stderr-volume increase in the integrated binary. It did not measure the
input-send-to-MVS-commit-to-GPU-present path and did not prove identical remote
desktop workloads, so it cannot exclude an RDP-integration scheduling or
input-to-refresh regression. The latency root cause remains unconfirmed.

The strongest current candidates are the native 3.69-megapixel portrait MVS
decode while the fitted client image is much smaller, frequent large dirty
rectangles, and synchronous Release hot-path diagnostics. These are candidates,
not established causes. Dynamic resolution remains default-off because resized
`0x09` interoperability is still an Apple wire experiment; performance work
must not enable it without the required ARD evidence and live gate. That work is
tracked explicitly in the top-level README pending list.

## Strict Apple High Performance pre-live evidence (2026-08-30)

The reviewed commit was
`b2d89d16f598a355268d781a4a9505a6e7340c10`. Cargo used its normal default
parallelism. The following implementation and build gates all exited with code
0:

- `cargo +stable fmt --all -- --check`;
- `cargo +stable test -p frd-protocol-apple` (345 passed, 9 ignored), including
  the separate authentication (4 passed) and session (1 passed) integration
  targets;
- `cargo +stable test -p frd-app` (72 passed);
- `cargo +stable test -p frd-shell-desktop` (53 passed);
- both `cargo +stable test --workspace --no-default-features` and
  `cargo +stable test --workspace`;
- both `cargo +stable build --workspace --no-default-features` and
  `cargo +stable build --workspace`;
- `cargo +stable run -- --help` and
  `cargo +stable run -- hpssview --help`;
- `cargo +stable build --release -p frd-shell-desktop` and
  `cargo +stable build --release -p freeremotedesk-windows`.

The resulting standalone executable was
`D:\FreeRemoteDesk\.worktrees\mac-baseline-rdp-integration\target\release\freeremotedesk-windows.exe`,
42,138,112 bytes, with SHA-256
`5CD74FE5396EE7E1C2D3B74917697D2022579F4E3EE703FAAE4A6B56CA3C328B`.
It is not Authenticode-signed. This repository state provides no installer or
packaging pipeline, so the artifact is an unsigned standalone EXE, not an MSI,
ZIP package, or installable release.

The strict live gate was not started. The Windows host was attached to a
different local /24 from the previously authorized target; ICMP, ARP, and TCP
5900 checks could not reach that target. A bounded scan found seven responsive
hosts on the current /24 and no listening TCP 5900 service. Target addresses
are intentionally omitted from this tracked record.

Consequently, none of the strict product observations were made: stock macOS
acceptance of the High Performance virtual display, physical-display blanking
and restoration, complete continuously updating virtual desktop, strict
geometry agreement, focused input, or normal disconnect. Historical
shared-console authentication, MVS, input, and media evidence cannot substitute
for this gate. Windows-client to macOS-server strict High Performance therefore
remains **开发中** and must not be promoted to **受限验证**.

## Remaining Platform Evidence

- macOS and Linux expose the same platform-adapter contract but are compile
  placeholders in this Windows-first phase; their native title bars are not
  runtime-validated here.
- Android and HarmonyOS NEXT remain later product-shell phases and do not
  consume the Windows native-chrome adapter.
