# Phase B — Architecture: status report

Branch: `phase-b`. Started 2026-10-29 at the owner's request; executed unattended.

Phase B acceptance criteria come from `../08-risks-and-roadmap.md`. Status values:
- **Done**: criterion met as written
- **Partial**: deliverable exists but part of the criterion is unmet
- **Owner**: needs owner review or approval
- **Blocked**: cannot be done in this environment

| ID | Criterion | Deliverable | Status |
|---|---|---|---|
| B1 | ADRs 0001–0008 accepted or amended | `../adr/` | **Done**: 0001, 0002, 0005, 0006, 0008 accepted by the owner. 0003, 0004, 0007 amended with Phase B evidence; each stays Proposed until its named Phase C test runs (stated in the ADR). |
| B2 | Formal state machines with legacy defects as negative cases | [`state-machines.md`](state-machines.md), [`arq-model-findings.md`](arq-model-findings.md), `../../spec/models/` | **Done** (spec). Legacy ARQ model-checked; found defects P19/P20. TLA+ tooling unavailable here, so a project-written exhaustive checker was used. |
| B3 | API schema v0 and storage schema v0 | [`../../spec/`](../../spec/README.md) | **Done**: 49 methods / 9 event types; schema applies cleanly and passes 20 constraint checks |
| B4 | Hi-fi mockups plus accessibility review | [`../../design/`](../../design/README.md) | **Owner**: 4 screens built; automated accessibility audit passes (0 issues after a token fix); screen-reader passes pending; **needs your acceptance** |
| B5 | Tooling spikes: serial loopback and replug on 3 OSes; Tauri packaging in CI | `../../spikes/serial-spike/`, `../../app/`, `../../.github/workflows/` | **Done, with stated limits**: CI green on Linux, macOS and Windows; installers built for all three. The serial PTY tests run on Linux only (macOS PTYs reject the baud ioctl), so macOS/Windows serial behaviour and real radio replug still need hardware. |

Extra work, labelled as Phase C groundwork: the `aether-ddt2` codec crate, which turns the protocol spec into executable, golden-vector-tested code. Its validation level is **Simulated** only.

Results and evidence are appended below as each item completes.

## B5 results (2026-10-29)

**Serial spike** (`spikes/serial-spike`): five tests on Linux pseudo-terminals, all passing.

| SM-1 behaviour | Result |
|---|---|
| Opening the port writes nothing to the "radio" (I1.2) | pass |
| A second open of the same device is refused while held (I1.1; `serialport` sets `TIOCEXCL`) | pass |
| All 256 byte values both directions, including `0x11`/`0x13` (N2.4, DR#253) | pass |
| Unplug detected (I/O error on read) | pass, detected within microseconds |
| Replug: reopen through a stable by-id style symlink to a new device node | pass |
| Serial path throughput vs. Fast Data rate | ~69 MB/s on a PTY, roughly 10^5 × the ~435 B/s Fast Data ceiling, so the serial path is not a bottleneck. (Real USB-CDC and Bluetooth SPP latency still need hardware measurement.) |

Not covered by PTYs: DTR/RTS/DSR modem lines, USB re-enumeration and device-node renaming, Windows COM ports, Bluetooth SPP. The spike's `listen` command is **receive-only** and safe to point at the IC-9700 or ID-52 PLUS for HW-1:

```bash
cargo run -p serial-spike -- list
cargo run -p serial-spike -- listen /dev/serial/by-id/<radio> 9600
```

**Tauri shell** (`app/`):
- `cargo tauri build --bundles deb,rpm` on Ubuntu 24.04 produced `Aether DV_0.0.1_amd64.deb` and `Aether DV-0.0.1-1.x86_64.rpm` (2.84 MiB each); the `.deb` depends on `libwebkit2gtk-4.1-0`, `libgtk-3-0`.
- The release binary was launched under Xvfb and screenshotted: the window renders with the bundled IBM Plex and v1 tokens, and the JS↔Rust bridge works (it shows the version from the native side).
- UI capabilities are limited to `core:default`: the UI cannot touch files, shell or serial ports (ADR-0001).
- Not verified: AppImage (bundler downloads tools at build time), macOS `.app`/`.dmg`, Windows MSI/NSIS, code signing and notarisation.

**CI on GitHub** (results read through the API from this session):

| Run | Result |
|---|---|
| `ci` on push, Linux + spec | success ([run 40811275092](https://github.com/KK4ODA/aether-dv/actions/runs/40811275092)) |
| `ci` full matrix, first attempt | Linux success. **macOS failed**: PTYs reject `IOSSIOSPEED` (ENOTTY), so the PTY tests became Linux-only. **Windows failed**: a refused localhost connect takes ~2 s there, so the test was widened and the rule recorded as N6.5. |
| `ci` full matrix after fixes, at `5aeb8b2` | **all green**: Linux, macOS 14, Windows 2022, spec ([run 40813383486](https://github.com/KK4ODA/aether-dv/actions/runs/40813383486)) |
| `package`, three OSes | **all green** ([run 40811898416](https://github.com/KK4ODA/aether-dv/actions/runs/40811898416)). Artifacts: Linux deb/rpm/AppImage (82.9 MiB, mostly the AppImage's bundled WebKit), macOS app/dmg (3.0 MiB), Windows MSI/NSIS (4.7 MiB). Unsigned. Run from the throwaway branch `package/phase-b-spike`, because manual dispatch only works once the workflow is on `main`. |

Workflow design (`.github/workflows/`):
- `ci.yml`: fmt, clippy `-D warnings`, workspace tests, spec checks. Pushes run Linux only; pull requests and manual runs add macOS and Windows tests.
- `package.yml`: three-OS bundles, run manually or on `v*` tags.
- The split is deliberate: macOS minutes bill at 10× on private repositories.

## Phase C groundwork done in this session (Simulated level only)

Started because it needed no hardware and no further owner decisions. All of it is sans-I/O and covered by the CI above.

| Crate | What | Tests |
|---|---|---|
| `aether-ddt2` | Byte-exact DDT2 codec: CRC, yEnc, frames, bounded deframer with lost-marker recovery, session payloads | 27, including both upstream golden vectors byte-exact (compressed too) |
| `aether-link` | ARQ-1 (legacy stateful ARQ, wire-compatible) and the SM-2 channel scheduler | 13. The simulation through the full codec **reproduces P19 in real code** (sender "done", receiver 2,816 bytes short) and the receiver flags it. |
| `aether-rfl` | Ratflector client handshake and reconnect (SM-6) | 16, including real-TCP tests against a mock legacy server |
| `aetherdv-rfd` | Ratflector forwarding core with cross-link duplicate suppression | 9 loop simulations. The legacy triangle turns one line into 1,000 copies in 60 s; Aether delivers 1. RF bridge links need a ≥30 s window (spec 04 corrected). |
| `serial-spike` | Receive-only `list`/`listen` tool for HW-1; PTY behaviour tests | 5 (Linux) |

70 Rust tests in total, all passing on Linux; the cross-platform matrix passes as well.

## What needs you

1. **Review and accept (or change) the four mockups**: `design/mockups/*.html` or `design/mockups/screens/*.png`. This is the open Phase B exit criterion.
2. **Merge `phase-b` into `main`** when you're satisfied, or ask me to open a pull request. Merging also makes the `package` workflow dispatchable from the Actions tab.
3. **First hardware step (HW-0/HW-1):** plug in the IC-9700 or the ID-52 PLUS and run `cargo run -p serial-spike -- list`, then `listen` on the DV Data port while another station sends D-RATS traffic. It never transmits.
4. **Actions minutes:** this session triggered one three-OS packaging run and two matrix runs. macOS bills at 10× on private repositories; packaging and the matrix stay manual for that reason.
