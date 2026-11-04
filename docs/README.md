# Aether DV — Phase A Technical Investigation & Architecture Proposal

Date: 2026-10-29 · Status: **research deliverable, no production code** · Licence of these docs: CC-BY-4.0 (proposed)

| Doc | Contents |
|---|---|
| [01-legacy-feature-inventory.md](01-legacy-feature-inventory.md) | Per-function inventory of D-RATS / drats2 / macrats / ratflectors: what, how, failure modes, retain / redesign / replace, compatibility requirements |
| [02-legacy-protocol-analysis.md](02-legacy-protocol-analysis.md) | DDT2 wire format, sessions, ARQ, file/form/RPC/position encodings, verified golden vectors, defect register P1–P18 |
| [03-dstar-rf-transport.md](03-dstar-rf-transport.md) | Slow Data and Fast Data on air, infrastructure path effects, radio compatibility matrix, mode-selection feasibility, scheduling |
| [04-ratflector-analysis.md](04-ratflector-analysis.md) | Ratflector protocol, forwarding behaviour, loop/robustness issues, server and client requirements |
| [05-architecture.md](05-architecture.md) | Stack recommendation, process model, module contracts, state machines, protocol layering, Fast Data policy |
| [06-ux-information-architecture.md](06-ux-information-architecture.md) | IA, wireframes, keyboard map, design tokens |
| [07-test-and-validation-plan.md](07-test-and-validation-plan.md) | Validation levels, simulator, first hardware tests, performance protocol, model predictions |
| [08-risks-and-roadmap.md](08-risks-and-roadmap.md) | Risk register, phase plan with acceptance criteria |
| [09-licensing.md](09-licensing.md) | Licence analysis and recommendation |
| [10-repeater-operation.md](10-repeater-operation.md) | Repeater operation: routing and reflector containment, D-RATS comparison, channel sense, courtesy, UX, test plan (accepted design) |
| [11-analog-fm-mode.md](11-analog-fm-mode.md) | Analog FM mode (design note): packet TNC port (KISS/AGWPE over TCP, serial, Bluetooth, BLE), D-RATS TNC framing and defects, FM repeater containment, plan |
| [12-fast-ip-paths.md](12-fast-ip-paths.md) | Fast IP paths (design note): tiered internet and amateur-IP paths (tuned legacy, unicast relay, direct peer links, AREDN/HamWAN), streaming ALP, path choice with RF failover, plan |
| [13-resilient-servers-and-relays.md](13-resilient-servers-and-relays.md) | Resilient servers and store-and-forward relays (design note): ratflector cluster across sites, client failover, presence; message relays that move traffic to RF when a site loses internet, with receipts and RF protection |
| [bench-tests.md](bench-tests.md) | Step-by-step bench tests for the IC-9700 and ID-52 PLUS (setup, five recorded tests, troubleshooting) |
| [bench-results.md](bench-results.md) | Results of the bench tests: key-up, air time, flow control, voice sensing (IC-9700, ID-52 PLUS) |
| [adr/](adr/) | ADR-0001 … 0009 (0001, 0002, 0005, 0006, 0008, 0009 Accepted) |
| [decisions.md](decisions.md) | Owner decision log |
| [phase-b/](phase-b/README.md) | Phase B status report: state machines, model-checking findings, schemas, spikes, CI results |
| [tools/](tools/) | `model_ddt2.py` (independent DDT2 model, matches upstream vectors); `arq_model.py` (throughput model) |
| [research-notes/](research-notes/) | Raw research notes: issue-history catalogue (`issues_history.md`), Fast Data hardware research (`fastdata_research.md`) |

**Evidence discipline.** Every claim in these documents is tagged as one of:
- source-read (repo@commit:file:line)
- documented (JARL / Icom / Kenwood)
- community report
- model
- unknown

**Nothing has been tested on hardware or against a running D-RATS yet.** This environment could not run D-RATS or reach public ratflectors (raw TCP is blocked). The one thing verified by execution: an independent DDT2 model (`tools/model_ddt2.py`) reproduces both upstream D-RATS golden vectors byte-for-byte.

## 1. Answers to the ten questions

**1. How does D-RATS implement communication over D-STAR DV?**
It never touches D-STAR framing. It writes an opaque byte stream to the radio's serial "DV Data" port. The radio packs the bytes into slow-data blocks of type `0x3X` (5 bytes per 40 ms; ceiling 119 B/s ≈ 950 bps) and keys up automatically ("DV Data TX: Auto"). The byte stream consists of DDT2 frames:
- 25-byte header (magic, seq, session, type, CRC-16/XMODEM, length, 8-char source/destination)
- optional zlib payload
- yEnc-escaped to avoid bytes radios mangle
- wrapped in `[SOB]…[EOB]`

Above that:
- positional sessions: 0 control, 1 chat, 2 RPC
- a stateful selective-repeat ARQ (window, REQACK poll, ACK list, 8-bit seq) used for files and forms
- store-and-forward of XML forms

D-RATS treats serial write completion as "sent" and gets no RF feedback. Details: `02`, `03` §1.1.

**2. Does D-RATS work over radios configured for DV Fast Data? What changes are needed?**
Yes, in the sense that it needs no change: Fast Data is a radio setting, the DV Data port carries the same byte stream, and the D-RATS wiki documents an ID-51 PLUS recipe with Fast Data ON. Limits:
- Both ends (and every repeater/hotspot hop) must handle Fast Data. A slow-only receiver gets nothing.
- D-RATS's fixed timing (random 0.5–2 s pre-TX delay, 512 B × 4 window, 4 KB cap, 12 s minimum timeout) wastes much of the gain. The model predicts ≈2.4× slow throughput instead of ≈3.6× (`07` §4.1, unvalidated).
- No published measurement of D-RATS over Fast Data was found.
- What should change: per-mode timing and burst sizing, which is sender-local and compatible with legacy receivers (ADR-0003), plus clear operator warnings about the slow-only peers it excludes.

**3. Can Aether automatically identify whether a connected radio is Fast Data capable?**
- **Model:** partly. Where a CI-V channel exists, the documented "read transceiver ID" identifies the model, which maps to the profile DB. There is no blind probing. On DV-Data-only connections the operator selects the model.
- **Firmware:** generally not readable, so old ID-5100/ID-4100/ID-51 firmware gets a "verify" warning.
- **Current setting:** **yes, on current Icom models**, via documented CI-V (`22 03` on ID-52/ID-52 PLUS/ID-50/ID-5200; `1A 05 0093` IC-705, `0077` IC-9700, `0098` IC-905; verified in Icom's CI-V guides). This requires a CI-V channel concurrent with DV Data: easy on IC-705/9700/905 (two USB ports), USB+Bluetooth on ID-52-class, not possible on single-function setups. Kenwood: no documented path.

**4. Can it distinguish whether the remote station supports Fast Data?**
Not from the radio. The data port shows decoded bytes only (whether CI-V can report the received mode is to be tested, HW-11).
- At the protocol level, Aether peers *declare* capability (extended `RPCGetVersion` reply, status token).
- The path is *proven* only by an end-to-end ACK of data sent with local Fast Data on.
- Proofs must age, because repeaters and hotspots in the path vary: MMDVMHost dropped and restored Fast Data handling several times between 2025-02 and 2026-02.
- Legacy D-RATS peers are "unknown" unless the operator declares them.

**5. Can the application select Slow or Fast programmatically, or is manual configuration required?**
Both, depending on hardware. On CI-V-equipped current Icom radios with a concurrent control channel, the setting can be read and set programmatically (documented). Everywhere else (Kenwood, single-port setups, older DATA-jack radios with CI-V off, ID-5100 unknown) it is a manual menu change. Untested risks of programmatic switching: settling time, behaviour mid-transmission, flash wear. The bigger risk is operational: in Fast mode the station is **unreadable to every slow-only station on the channel**. Recommendation (ADR-0005): Slow by default on mixed nets. Auto switches only between transfers, toward proven-fast peers, on hardware-verified profiles, and is always logged and visible.

**6. Which RF limitations are inherent to D-STAR and which are introduced by D-RATS?**
- Inherent:
  - ~119 B/s slow / ~435 B/s fast ceilings
  - no FEC on data bytes
  - half-duplex
  - per-over overhead (≈0.5 s documented carrier sense + key-up, RF header, tail)
  - no carrier sense visible on the data port
  - radio-inserted text/GPS/header blocks
  - slow-only receivers can't decode Fast Data
  - path elements that may corrupt Fast Data
- Introduced by D-RATS:
  - random pre-TX delay
  - serial-write-based rate estimation
  - 12 s minimum timeout
  - fixed small window and 4 KB cap
  - compressing small frames and double compression
  - unescaped `[SOB]/[EOB]` poison blocks
  - half-duplex behaviour even on TCP
  - historic flow-control byte loss
  - no end-to-end message receipts

Table: `03` §6.

**7. What improvements could produce measurable gains without breaking interoperability?**
- **Layer 1, sender-local, legacy wire format** (each gated by an interop test):
  - block/window sizing per mode and loss
  - uncompressed control and file blocks
  - air-time-corrected RTO
  - receive-activity hold-off instead of the blind random delay
  - priority interleaving (chat ahead of bulk)
  - poison-block avoidance
  - full-duplex TCP
  - robust ratflector reconnect/keepalive
  - honest delivery states
  
  Model estimate: 1.3–2.5× on slow and restoring ≈3.5× fast/slow (to be measured).
- **Layer 2 (ALP)** between Aether peers only, inside DDT2 so legacy ratflectors carry it: receipts, hash-resumable transfers, SACK. Build each only when Phase C/D data shows the need.

**8. What is the best implementation architecture?**
- A Rust workspace with **sans-I/O protocol cores** and an injected clock: codec, link/scheduler, ARQ.
- A separate engine process (`aetherdv`) owning ports, store and routing.
- A Tauri 2 + TypeScript desktop UI over a versioned local API.
- SQLite WAL durable store.
- A standalone `aetherdv-rfd` ratflector server from the same crates.
- A deterministic simulator for every test below hardware level.

Slow and Fast share everything above a data-only `ModeProfile`. See `05`; ADR-0001…0008.

**9. How should the first hardware validation tests be structured?**
In order, with captures for each:
- HW-0 port identity (no transmit)
- HW-1 passive receive
- HW-2 byte transparency per mode
- HW-3 flow control and buffer
- HW-4 key-up/tail timing
- HW-5 raw rate
- HW-6/7 interop chat/file with D-RATS 0.4.2
- HW-8 Fast Data receive matrix
- HW-9 interruption recovery
- HW-10 CI-V Fast Data control
- HW-11 received-mode visibility

The baseline comes before any optimisation: D-RATS vs Aether with identical parameters on the same rig. See `07` §3–4.

**10. What is the smallest viable prototype that demonstrates a real advantage over D-RATS?**
A headless `aetherdv` plus a minimal UI or TUI that:
1. Interoperates byte-exactly with D-RATS 0.4.2 for chat, ping and an ICS-213 form and 10 KB file, over one Icom radio and over a ratflector.
2. Keeps a durable queue that survives kill -9 and USB unplug, with operator-visible delivery state that never claims more than the evidence.
3. Reconnects to ratflectors automatically with back-off and detects half-open sockets.
4. Shows measurably higher goodput on the same radio and settings, by sender-local tuning alone, **with no change for the D-RATS station at the other end**.

The measurable advantage is (4), backed by (2) and (3) as reliability advantages D-RATS lacks. Fast Data and ALP come after this baseline exists (Phases D/E).

## 2. Feature / compatibility matrix (target behaviour and current validation level)

| Feature | vs legacy D-RATS | vs legacy ratflector | Aether↔Aether extra | Validation now |
|---|---|---|---|---|
| Broadcast/direct chat, ping, status | Full (wire-identical) | Full | Receipts on direct chat (ALP) | Source-read; 2 golden vectors executed |
| Position (NMEA/DPRS) | Full | Full | — | Source-read |
| Forms/messages (XML) | Full, hop-level confirmation only | Full | End-to-end receipts | Source-read |
| File transfer | Full incl. RESUME | Full | Hash-resumable, SACK | Source-read |
| RPC (version/position/file list) | Answer + request; remote file ops opt-in | Full | Capability discovery | Source-read |
| DV Slow | Full | n/a | Tuned timing | Not hardware-tested |
| DV Fast | Works if both radios and the path support FD | n/a | Proven-path tracking, CI-V control | Documented only |
| Ratflector client | — | Full handshake incl. old-school | Keepalive, dedup, capability | Source-read (+ macrats tests) |
| Ratflector server | Serves legacy clients | Peers with legacy servers | Loop guard, envelopes | Design only |
| Winlink, email, APRS-IS, KISS, socket forwarding | Deferred | — | — | — |

## 3. Risk register and roadmap
See `08`. Top risks:
- radio behaviour varies per model and firmware (R1)
- remote/path FD capability is unknowable (R4/R20)
- Layer-1 optimisations could trip legacy receivers (R5)
- loops in mixed ratflector meshes (R7)
- access to real radios for validation (R9)

## 4. Corrections to assumptions in the brief
- DV Fast Data dates from JARL standard v5.0 (**Aug 2014**; ID-51 PLUS, ID-5100 firmware E2), not the ID-52 era.
- Icom's "TX Delay (PTT)" keeps sending queued data as Fast Data for 1–10 s after PTT release (PTT data mode only). It is not a batching delay.
- Fast Data inserts a beep block about every second. Slow-only radios hear beeps, and the RF header is not retransmitted during Fast Data.
- Fast Data on/off **is** programmatically controllable on current Icom models (documented CI-V). It is not menu-only.
- Kenwood TH-D74/D75 reportedly support Fast Data but **not slow-data transmit**. Icom↔Kenwood interop therefore needs Fast Data (community report; verify).

## 5. Owner decisions (2026-10-29)

All Phase A questions are answered; see [decisions.md](decisions.md). In short: Rust + Tauri; GPL-3.0-or-later; test radios IC-9700, ID-52 and Kenwood TM-D750; D-RATS 0.4.x only; Slow default with Fast opt-in; Chat as the launch screen; per-port chat with an optional merged view; ICS-213 only in the first release.

### Original questions (for the record)
1. **Stack**: accept Rust + Tauri (ADR-0001), or weight contributor familiarity higher (PySide6)?
2. **Licence**: accept GPL-3.0-or-later (ADR-0008)?
3. **Hardware**: which radios do you (and willing testers) own? Phase C needs at least one Icom DV radio plus a second D-STAR station. Phase D needs two Fast-Data radios. The radio list drives which profiles get verified first.
4. **Legacy targets**: are D-RATS 0.3.x (Python 2) stations still in use in your networks? That decides whether 0.3.10 joins the interop gate.
5. **Default mode policy**: Slow by default on shared nets (recommended), or Fast-first for your intended deployments?
6. **First screen and nets**: see `06` §6.

