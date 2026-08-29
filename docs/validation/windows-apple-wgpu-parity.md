# Windows WGPU Session Chrome and Apple Parity Validation

Date: 2026-08-28

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
- the four 32-point slots are centered as one cluster and occupy 64 physical
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

## Remaining Platform Evidence

- macOS and Linux expose the same platform-adapter contract but are compile
  placeholders in this Windows-first phase; their native title bars are not
  runtime-validated here.
- Android and HarmonyOS NEXT remain later product-shell phases and do not
  consume the Windows native-chrome adapter.
