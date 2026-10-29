# Phase B — Architecture (work in progress)

Branch: `phase-b`. Started 2026-10-29 at the owner's request; executed unattended.

Phase B acceptance criteria come from `../08-risks-and-roadmap.md`. Status values:
- **Done**: criterion met as written
- **Partial**: deliverable exists but part of the criterion is unmet
- **Owner**: needs owner review or approval
- **Blocked**: cannot be done in this environment

| ID | Criterion | Deliverable | Status |
|---|---|---|---|
| B1 | ADRs 0001–0008 accepted or amended | `../adr/` | 5 of 8 accepted by the owner; 0003/0004/0007 to be reviewed with the Phase B results |
| B2 | Formal state machines with legacy defects as negative cases | [`state-machines.md`](state-machines.md), [`arq-model-findings.md`](arq-model-findings.md), `../../spec/models/` | **Done** (spec). Legacy ARQ model-checked; found defects P19/P20. TLA+ tooling unavailable here, so a project-written exhaustive checker was used. |
| B3 | API schema v0 and storage schema v0 | [`../../spec/`](../../spec/README.md) | **Done**: 49 methods / 9 event types; schema applies cleanly and passes 20 constraint checks |
| B4 | Hi-fi mockups plus accessibility review | [`../../design/`](../../design/README.md) | **Owner**: 4 screens built; automated accessibility audit passes (0 issues after a token fix); screen-reader passes pending; **needs your acceptance** |
| B5 | Tooling spikes: serial loopback and replug on 3 OSes; Tauri packaging in CI | `../../spikes/serial-spike/`, `../../app/`, `../../.github/workflows/` | **Partial**: Linux verified locally (below). macOS/Windows jobs are written but **have not run**: no CI results are visible from this session. Real-radio replug needs your hardware. |

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

**CI** (`.github/workflows/`):
- `ci.yml`: fmt, clippy `-D warnings`, workspace tests, spec checks. Pushes run Linux only; pull requests and manual runs add macOS and Windows tests.
- `package.yml`: three-OS bundles, run manually or on `v*` tags.
- The split is deliberate: macOS minutes bill at 10× on private repositories.
- The Linux job was dry-run locally, step for step: all green. **Status on GitHub is unknown** (this session can't read Actions results). Please check the Actions tab; the first push of this branch will run `ci`.
