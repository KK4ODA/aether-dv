# 05 — Proposed Architecture

Status: Phase B **proposal** for review. Nothing here is implemented. Decisions are recorded as ADRs in `adr/`.

## 1. Guiding constraints (from the brief and from Phase A findings)

1. The RF engine must keep working when the UI, network, or optional services fail. So the engine lives in **its own process**.
2. D-RATS talks to the radio as an opaque byte stream and gets **no RF feedback**. Aether must not pretend otherwise. Every delivery claim must come from an end-to-end acknowledgement.
3. Slow and Fast Data are, on current Icom radios, **the same computer-facing byte stream with a radio-side setting** (to be confirmed; see `03`). The shared machinery is large; the mode-specific part is small but must be explicit: rate, burst sizing, timers, and which byte values are safe.
4. Legacy interop is defined by DDT2 bytes and positional session ids. That codec must be **isolated, pure, and test-vector driven**.
5. Protocol logic must be deterministic and testable without hardware. Hence **sans-I/O cores with an injected clock**.

## 2. Technology recommendation

| Criterion | Rust core + Tauri 2 UI (web frontend) | Qt 6 / PySide6 (Python) | Rust core + native Rust GUI (Slint / egui / iced) | C++/Qt 6 |
|---|---|---|---|---|
| Serial I/O reliability | `serialport`/`tokio-serial`, mature on all 3 OSes | QtSerialPort good; Python GIL plus threads makes timing jittery | same as col. 1 | QtSerialPort good |
| Explicit concurrency | tokio tasks/actors, `Send` checking | threads and signals; GIL | same | manual |
| Headless engine and server sharing code | **yes** (same crates) | possible (Python daemon) | yes | possible |
| Memory safety | yes | yes (interpreter) | yes | no |
| UI quality and distinctiveness | high (full CSS control); MapLibre GL for offline vector maps | high, native-feel; maps via QtLocation (weaker offline story) | improving; **no mature offline map widget**; custom styling costs more | high |
| Accessibility | WebView2/WKWebView expose ARIA well; WebKitGTK on Linux weaker | good (Qt a11y bridges) | AccessKit maturing | good |
| Packaging | MSI/NSIS, DMG/app, AppImage/deb/rpm, signed updater built-in | PyInstaller/briefcase: large, AV false positives, notarisation pain | cargo-dist, good | good but heavy CI |
| Long-term dependency risk | Tauri 2 stable since 2024, active; WebKitGTK version drift on old distros | Qt LGPL obligations; PySide churn | Slint GPL/royalty-free licence terms; egui/iced pre-1.0 APIs | Qt licensing |
| Contributor pool | Rust + TS | Python (largest ham-dev pool) | Rust only | C++ |

**Recommendation (ADR-0001): a Rust workspace for every non-UI component, plus a Tauri 2 desktop shell with a TypeScript UI (Svelte 5 proposed; see ADR-0006).** The engine runs as a separate process, `aetherd`. The same daemon runs headless on a Raspberry Pi as a bridge/station. The ratflector server `aether-rfd` reuses the codec and link crates.

Cost of this choice: Rust raises the bar for the many Python-literate hams who maintained D-RATS. Mitigations: a sharply specified, test-vector-driven protocol spec (portable to any language), a small, well-documented crate set, and a UI layer in mainstream TypeScript.

## 3. Process model

```
┌──────────────────────── operator workstation ─────────────────────────┐
│                                                                        │
│  ┌──────────────────────────┐   local IPC (WebSocket on 127.0.0.1 or  │
│  │  Aether DV desktop       │   Unix socket / named pipe; JSON-RPC 2.0│
│  │  (Tauri shell + TS UI)   │◀─ + event stream; token auth; versioned)│
│  └──────────────────────────┘                    │                     │
│                                                  ▼                     │
│  ┌──────────────────────────── aetherd (engine) ───────────────────┐  │
│  │ api ─ app-state ─ messaging ─ store(SQLite WAL)                  │  │
│  │                    │                                             │  │
│  │      ┌─────────────┴─────────────┐                               │  │
│  │      │  Router / delivery tracker │◀── forms, geo                 │  │
│  │      └──────┬──────────────┬──────┘                               │  │
│  │   link: per-port scheduler + sessions (legacy DDT2 | ALP)         │  │
│  │      │                            │                               │  │
│  │  radio adapter (serial)      ratflector client(s)                 │  │
│  └──────┼────────────────────────────┼───────────────────────────────┘  │
│         ▼ USB/serial/BT-SPP           ▼ TCP :9000                        │
└─────────┼─────────────────────────────┼─────────────────────────────────┘
          ▼                             ▼
      Icom / Kenwood radio         aether-rfd or legacy ratflector
```

- The UI may be closed, crash, or be replaced (a future TUI or web UI) without touching RF state.
- `aetherd` runs as a user-level background process started by the desktop app or the OS (systemd user unit, launchd agent, Windows Task Scheduler/Service), with an option to keep running in the tray.
- Single-instance lock per profile. **Exclusive serial-port ownership** lives in the engine only.

## 4. Crate/module map and contracts

| # (brief) | Crate | Responsibility | Depends on | Key contract |
|---|---|---|---|---|
| 1 | `ui/` (TS) + `aether-api` | Views, view models; API schema, generated TS types | api only | Versioned JSON-RPC methods plus an event stream. The UI never touches ports, files, or the DB directly. |
| 2 | `aether-store` | SQLite schema, migrations, message/queue/attachment store, journal | — | `Store::enqueue(msg) -> MsgId` is durable (fsync'd) before return. Queue state transitions are transactional. |
| 2 | `aether-msg` | Message model, delivery-state machine, store-and-forward routing (legacy-compatible rules) | store, link | Emits `DeliveryEvent{msg, hop, state, evidence}` |
| 3 | `aether-ddt2` | **Pure** legacy codec: framing, yEnc, CRC-16/XMODEM, zlib, session/control/chat/RPC/file encodings, legacy ARQ state machine | none (no I/O, no clock) | `decode(&[u8]) -> Result<Frame>`, `encode(&Frame) -> Vec<u8>`; `LegacyArq::on_event(Event, Instant) -> Vec<Action>` |
| 4 | `aether-link` | Per-port **channel scheduler** (half-duplex turns, priorities, burst budget), session multiplexer, ALP (Aether Link Protocol) state machines, capability cache | ddt2 | Sans-I/O: `poll_transmit(now) -> Option<Burst>`, `on_rx_bytes(now, &[u8])`, `next_timeout() -> Instant` |
| 5 | `aether-radio` | Device discovery, stable identity, hardware profiles, adapter lifecycle, optional CI-V control | — | `trait ByteLink { open, write_all_paced, read, close, health }`; `trait RadioControl` (optional: read/set settings, only where documented) |
| 6 | `aether-rfl` | Ratflector client: handshake, reconnect state machine, multi-server | ddt2 | Exposes each server as a `ByteLink` with `PathKind::Internet{server}` |
| 7 | `aether-rfd` (bin) | Ratflector server | ddt2, rfl | Config, admin socket, metrics |
| 8 | `aether-forms`, `aether-geo` | Form schema (legacy XML import/export), ICS-213 etc.; positions: NMEA/DPRS codecs, station tracking, offline tiles | — | Pure codecs plus models |
| 9 | `aether-diag` | `tracing` setup, wire capture (`.aecap` = timestamped RX/TX byte records per port), support-bundle export | all | Capture format is documented and replayable into the simulator |
| — | `aether-sim` | Virtual radios, channel models (loss, burst loss, latency, half-duplex, turnaround), virtual clock, scenario runner | link, ddt2, radio(traits) | Used by every test category below hardware |

### 4.1 Shared and mode-specific code

Shared by Slow and Fast: everything above `aether-link`'s **`ModeProfile`**. A `ModeProfile` is data, not code paths:

```
ModeProfile {
  name: DvSlow | DvFast | Internet,
  nominal_payload_rate_bps,        // from measurement, not from brochure
  tx_keyup_overhead, tx_tail,      // measured per radio profile
  max_burst_bytes, max_burst_time, // channel etiquette and loss tradeoff
  safe_byte_policy,                // banned set for yEnc (per radio, per mode)
  ack_policy, rto_min/rto_max,
}
```

The scheduler and ARQ consume a profile. They never branch on "is fast?". Behaviour that is truly different, such as radio configuration steps and the fact that voice PTT forces slow data, lives in `aether-radio` profiles and the UI.

## 5. State machines (to be specified formally and model-checked in Phase B)

### 5.1 Port / radio adapter lifecycle
```
Absent ──device seen──▶ Identified ──open ok──▶ Open ──profile validated──▶ Ready
  ▲                         │ open fail (busy) ─▶ Conflict(owner?) ──retry──┘
  └─────unplug / IO error──────────────────── Lost ──backoff──▶ Identified
Ready ⇄ Transmitting (scheduler-owned) ; Ready ─operator─▶ Suspended
```
No commands are ever written to a port in `Identified`. Probing is **opt-in per profile** and only uses documented, side-effect-free queries (e.g. CI-V "read transceiver ID" on a port the user has marked as CI-V).

### 5.2 Channel scheduler (per RF port)
```
Idle ─queue non-empty & channel quiet≥holdoff─▶ Assemble(burst ≤ budget) ─▶ Writing
Writing ─all bytes accepted by port─▶ AwaitAirClear (estimated air time + tail)
AwaitAirClear ─timer─▶ Listen (response window) ─rx activity─▶ Receiving ─quiet─▶ Idle
Any ─rx activity while Assemble/Idle─▶ Defer (random back-off within etiquette bounds)
```
"Channel quiet" means no bytes from the radio for `holdoff`. It is a heuristic: the data port shows only *decoded data*, not voice or noise. Where a CI-V busy/squelch query is available and documented, it is used as an additional input. Never as the only one.

### 5.3 Reliable transfer (both legacy and ALP)
Sender: `Opening → Sending(window) → Polling(awaiting ACK, RTO) → {Sending | Done | Suspended(link down) | Failed(retries exhausted)}`. Receiver: `Open → Receiving → Complete | Abandoned(idle timeout)`. Suspension is a first-class state. A transfer survives a radio unplug or a reboot, and resumes by content hash (ALP) or `RESUME:` offset (legacy, with an added size/hash sanity check on our side).

### 5.4 Message delivery state (operator-visible)
```
Draft → Queued(durable) → Sending(hop k, path) → HandedOff(next hop accepted)
      → Delivered(end-to-end receipt)        [Aether peers only]
      → Unconfirmed(legacy peer: hop-level only, stated explicitly)
      → Failed(reason) / Expired(TTL)  ; operator: Retry / Reroute / Cancel
```

### 5.5 Ratflector client — see `04-ratflector-analysis.md` §6.

## 6. Protocol strategy

**Layer 0: legacy DDT2, byte-exact (ADR-0002).** Always on. Chat = session 1, RPC = session 2, file/form via control-negotiated stateful sessions. Aether speaks it with every station by default.

**Layer 1: sender-local optimisations within the legacy wire format (ADR-0003).** No negotiation needed, because they rely only on receiver behaviour that the source shows to be permissive:
- Choose block size and window from the active `ModeProfile` and the observed loss, within limits: window ≤ 64 so the 8-bit seq is never ambiguous; block 64–1024 B.
- Send control, ACK and short frames uncompressed (magic 0x22) when that is smaller.
- File blocks sent uncompressed, since the file stream is already zlib.
- RTO from measured *air-time-corrected* RTT, not serial write time.
- Replace the blind 0.5–2 s random delay with receive-activity hold-off plus a bounded random back-off.
- Priority interleaving: chat and ACKs pre-empt bulk between bursts.
- Avoid `[SOB]/[EOB]` poison blocks: detect before sending and re-chunk.

**Every Layer-1 item must pass an interop test against unmodified D-RATS 0.4.x before it ships.**

**Layer 2: Aether Link Protocol (ALP v1), Aether↔Aether only, negotiated (ADR-0004).**
- Capability discovery reuses the deployed **`RPCGetVersion`** job (`rpc.py:504-524,989-1018`). Legacy stations answer with `version/os/pyver/gtkver`; Aether adds keys such as `aether=1;alp=1;rcpt=1;fd=declared`. Extra dict keys are harmless to legacy requesters. Results are cached per (callsign, path) with an age. Aether status/ping replies also carry a short token so capabilities are learned passively.
- ALP still rides **inside DDT2 frames** (new stateful session class id, offered only to peers with confirmed capability), so legacy ratflectors and sniffers forward and display it.
- ALP features, each justified by a measured problem before it is built:
  - 16-bit sequence with SACK bitmap ACKs
  - content-addressed transfers (SHA-256) with resume by hash and offset
  - end-to-end message receipts
  - per-transfer priority
  - compression only when it helps
- Explicitly **deferred** until hardware data justifies it: FEC/erasure coding and any change below DDT2 framing.

**Mixed networks:** a message addressed to several stations is sent per peer using the best common layer. Broadcast chat always uses Layer 0.

## 7. Fast Data handling in the architecture

Distinguish three separate facts and never conflate them (detail in `03`):

| Fact | Source of truth | How Aether learns it |
|---|---|---|
| Local radio *model and firmware* support Fast Data | Hardware profile database | Operator selects the model; optional documented CI-V ID read |
| Local radio is currently *configured* for Fast Data | Radio menu state | CI-V read (documented: `22 03`, `1A 05 0093/0077/0098`; see `03` §2) when a concurrent CI-V channel exists; otherwise operator confirmation |
| Remote station *can receive* Fast Data on this path | Evidence | (a) operator declaration, (b) capability record says `fd=declared`, (c) **proven**: an end-to-end ACK of data sent while local FD was on |

Mode policy (operator-visible, per port):
- **Slow**: profile `DvSlow`. If the radio is believed to be in FD mode, Aether warns, because the radio setting, not the app, decides what goes on air.
- **Fast**: profile `DvFast`. Operator asserts the radio is set. Peers not proven FD-capable get a warning per send.
- **Auto**: Aether selects the `ModeProfile` (timers and burst sizing) from evidence. **It changes the radio setting only on models with a documented, tested CI-V control path, and never mid-transfer.** Otherwise "Auto" means "recommend and ask". Falling back to slow costs an operator action plus a radio menu change, and the UI must say so plainly.

## 8. Storage and durability

- SQLite in WAL mode. `synchronous=FULL` for queue and journal tables. One writer task in the engine.
- Tables: `messages`, `message_events` (append-only delivery log = exportable traffic record), `transfers` (+ chunk bitmap), `attachments` (content-addressed files on disk), `stations`, `positions`, `forms`, `capabilities`, `ports`, `profiles`, `settings`, `schema_migrations`.
- Crash recovery: on start, anything in `Sending` returns to `Queued` with an attempt count kept. Transfers resume from the chunk bitmap.
- Legacy import: read the D-RATS form store (XML folders) and config. Export legacy XML for interop.

## 9. Security and regulatory notes

- No encryption on RF (amateur rules). Authentication (signatures) is possible later, subject to a per-jurisdiction check.
- Station identification: the D-STAR RF header carries MYCALL, set in the radio, which satisfies ID on air. Aether should still warn if the configured callsign differs from what the radio reports, where readable.
- Remote-control features (RPC file pull/delete) are **off by default**, path-sanitised, and logged.
- Forms are parsed with a hardened XML parser: no DTD, no external entities (legacy forms came from lxml).

## 10. ADR index

| ADR | Title | Status |
|---|---|---|
| 0001 | Rust engine + Tauri 2 UI, engine as separate process | **Accepted** |
| 0002 | Legacy DDT2 as mandatory baseline; positional session ids preserved | **Accepted** |
| 0003 | Sender-local legacy-wire optimisations, gated by interop tests | Proposed |
| 0004 | ALP inside DDT2, capability discovery via RPCGetVersion | Proposed |
| 0005 | Fast Data = radio setting + ModeProfile; no automatic radio switching without a documented control path | **Accepted** |
| 0006 | UI framework (Svelte 5) and design-token system | **Accepted** |
| 0007 | SQLite WAL durable store, append-only delivery journal | Proposed |
| 0008 | Licence: GPL-3.0-or-later | **Accepted** |
