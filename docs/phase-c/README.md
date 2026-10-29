# Phase C — Minimal interoperable prototype: status and first hardware test

Branch: `phase-c` (built on `phase-b`). Validation level of everything here: **Simulated** unless a row says otherwise.

## What runs today

| Program | What it does |
|---|---|
| `aetherd --config FILE` | Engine. Serial radio ports (half-duplex scheduler) and ratflector ports (legacy handshake, back-off, TCP keepalive). Legacy D-RATS behaviour on chat (session 1) and RPC (session 2): chat, ping/echo replies (broadcast replies rate-limited), status, `RPCGetVersion` with the Aether capability token. Durable SQLite log. Local JSON-RPC/WebSocket API with a token. |
| `aether-cli` | Terminal client: `ports`, `stations`, `send PORT DST TEXT`, `history`, `ping`, `version`, `tail`. |
| `aether-rfd` | Ratflector server, legacy-compatible, with loop safeguards and per-client limits. |
| `serial-spike` | Receive-only port lister and hex monitor, for hardware test HW-0/HW-1. |

**Not in this slice yet:**
- file and form transfer (stateful sessions are refused like an unsupported type; ARQ-1 exists in `aether-link` but isn't wired in)
- position parsing (GPS sentences are logged as `raw` chat)
- CI-V Fast Data control
- the desktop UI

## Phase C acceptance criteria: current status

| ID | Criterion | Status |
|---|---|---|
| C1 | Interop with unmodified D-RATS 0.4.2 (chat, ping, status, ICS-213, 10 KB file; PTY and ratflector) | **Not started**: this environment can't run D-RATS. Chat, ping and version are ready to test; forms and files need the ARQ-1 wiring. |
| C2 | Ratflector reconnect with back-off; half-open detection; no loss of queued items | **Partial (Simulated)**: back-off with jitter, keepalive (60 s / 15 s × 4), queued chat sent after reconnect, honest failure states. Not yet tested against real NAT timeouts or public ratflectors. |
| C3 | HW-0…HW-7 on the IC-9700 and ID-52 PLUS in slow mode | **Needs you**: procedure below |
| C4 | Over the air with D-RATS | **Needs you** |
| C5 | Wire capture and replay; support bundle | Not started |
| C6 | Baseline measurements (D-RATS vs Aether) | Not started; needs C3 |

## First hardware test (HW-0, HW-1, then a first chat)

You need: the IC-9700 (or ID-52 PLUS) on USB, and a second D-STAR station within simplex range running **D-RATS 0.4.2**. The ID-52 PLUS with D-RATS on another computer works. Use a simplex DV frequency agreed for testing, at low power.

1. **Build** (once):
   ```bash
   cargo build --release -p aetherd -p serial-spike
   ```
2. **HW-0, find the port** (opens nothing):
   ```bash
   ./target/release/serial-spike list
   ```
   Note the IC-9700 entries. The DV Data port is the USB (B) interface; the other one is CI-V.
3. **HW-1, passive receive** (never transmits): have the D-RATS station send a few chat lines, then run
   ```bash
   ./target/release/serial-spike listen /dev/serial/by-id/<IC-9700 DV data port> 9600
   ```
   You should see `[SOB]…[EOB]` text in the right-hand column. Save the output; it is the first real capture. If you see nothing, check the radio menu settings in `examples/ic9700.toml`.
4. **First Aether chat over RF**: copy `examples/ic9700.toml`, set your port path (remove the ratflector section if you like), then run
   ```bash
   ./target/release/aetherd --config my.toml
   ```
   In a second terminal:
   ```bash
   ./target/release/aether-cli --token-file aetherd.token tail
   ```
   In a third terminal:
   ```bash
   ./target/release/aether-cli --token-file aetherd.token send IC-9700 CQCQCQ test from Aether DV
   ```
   Then ping the D-RATS station from it, and from D-RATS ping `KK4ODA`.
5. **Report back**:
   - what D-RATS showed
   - `aether-cli history 50`
   - the `listen` capture from step 3
   - the aetherd log
   
   These results move C3 and C4 forward, and they are the first numbers for ModeProfile timing (HW-4).

Safety notes:
- `aetherd` transmits only when you send something or answer a ping/version request addressed to you. Broadcast pings are answered at most once a minute per port.
- The radio's own MYCALL provides station identification on air.
