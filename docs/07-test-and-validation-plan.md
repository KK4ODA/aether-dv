# 07 — Test, Validation and Performance-Evaluation Plan

## 1. Validation levels (every feature carries exactly one current level)

| Level | Meaning | Evidence required |
|---|---|---|
| **S — Simulated** | Passes in `aether-sim` with the virtual clock and channel models | CI run id |
| **I — Interoperability** | Verified against unmodified D-RATS (named version, OS) or a legacy ratflector | Capture file + test report |
| **H — Hardware** | Verified with an identified radio, firmware, interface and cable, using the radio's data port, possibly into a dummy load or with a co-located receiver | Profile id + capture + report |
| **A — Over the air** | Verified between two stations over a real RF path, with distance/repeater/simplex recorded | Both stations' captures + log |

The feature matrix in the README is generated from a `validation.yaml` that each test report updates. **Nothing may be labelled RF-tested on the basis of a virtual serial port.**

## 2. Test architecture

```
             ┌────────────── aether-sim (deterministic, virtual clock) ───────────────┐
 scenario ──▶│ StationA(engine) ⇄ VirtualRadio ⇄ ChannelModel ⇄ VirtualRadio ⇄ StationB│
  (YAML)     │                       ▲  half-duplex, key-up, tail, rate(ModeProfile)   │
             │                       └─ loss: iid BER, Gilbert-Elliott bursts,         │
             │                          superframe-sync loss (drop to next sync),      │
             │                          byte-filter (banned bytes), radio buffer limit │
             └─────────────────────────────────────────────────────────────────────────┘
 Legacy interop rig:  Aether ⇄ PTY pair (socat) ⇄ D-RATS 0.4.x (container, Xvfb)
                      Aether ⇄ legacy d-rats_repeater (container) ⇄ D-RATS 0.4.x
 Hardware rig:        Aether host ⇄ radio A ⇄ dummy load/attenuator ⇄ radio B ⇄ host (Aether or D-RATS)
```

Components:
- **Golden vectors**: the two upstream vectors already matched (`02` §2.4), extended from captures of real D-RATS sessions (chat, ping, status, RPC, file with resume, form). Stored as `.aecap` and replayed in CI.
- **Property/fuzz tests**: DDT2 decoder, yEnc, NMEA/DPRS parsers, form XML, ratflector handshake (cargo-fuzz).
- **State-machine tests**: model-based tests (e.g. `proptest-state-machine`) for ARQ, the scheduler, the ratflector client and message delivery. Every legacy defect in `02` §8 gets a reproducing test.
- **Fault injection**: kill -9 the engine mid-transfer, pull the virtual USB, full disk, clock jump, TCP half-open (drop packets without FIN), slow-loris clients against `aether-rfd`.
- **Loop/amplification suite**: ratflector triangles, two RF bridges hearing each other, and a mixed legacy + Aether servers topology. The pass criterion is bounded total frames per injected frame.

## 3. First hardware validation tests (Phase C, in this order)

Each test has a written procedure, required equipment, a capture, and a pass/fail result. No test transmits until the station's identification and frequency plan are recorded.

| # | Test | Purpose | Method | Pass criterion |
|---|---|---|---|---|
| HW-0 | **Port identity, no-transmit** | Find the right port without probing | Plug and unplug; record VID/PID/serial/interface number and the stable path (`/dev/serial/by-id`, Windows instance id, macOS `cu.*`) | Profile recognises the radio, with no bytes written |
| HW-1 | **Passive receive** | Verify the RX byte path and the radio's own outputs | Another station (or D-RATS) sends known frames; Aether only listens and captures | 100 % of frames decoded; radio-injected NMEA/DPRS identified; unexpected bytes catalogued |
| HW-2 | **Byte transparency** (slow, then fast) | Validate the banned-byte set per radio and mode | Send frames whose payloads cover all 256 byte values, unescaped, in a test-only mode with a co-located receiver | Table of bytes altered or dropped per (radio, firmware, mode) |
| HW-3 | **Flow control and buffer** | Learn the radio buffer size and XON/XOFF behaviour | Write increasing bursts at max serial rate; log any 0x11/0x13 from the radio and the time to air | Buffer size, XOFF thresholds and the safe write-pacing parameters per profile |
| HW-4 | **Key-up / tail timing** | Measure the ModeProfile timing constants | Timestamp serial writes against an RF detector, SDR, or second radio's data output | Distributions (p50/p95) of key-up latency, tail, and min gap before the next TX |
| HW-5 | **Raw rate** | Actual slow/fast payload throughput | Long single burst, measure the receiver's byte arrival rate | B/s per mode with confidence intervals |
| HW-6 | **Interop chat/ping/status** | Aether ⇄ D-RATS 0.4.2 (and 0.3.x if available) over RF | Scripted exchange both directions | All messages delivered and correctly displayed on both sides |
| HW-7 | **Interop file and form** | Legacy ARQ over RF | 1 KB, 10 KB, 50 KB files and an ICS-213, both directions, ×10 each | ≥95 % success at good signal; captures for analysis |
| HW-8 | **Fast Data matrix** | Who can receive what | FD-enabled sender → FD-capable receiver, → slow-only receiver, → receiver with FD off; plus voice PTT during a data transfer | Documented behaviour per combination (decoded / garbled / silent / audio squawk) |
| HW-10 | **CI-V Fast Data control** (IC-705 / IC-9700 / ID-52 first) | Validate the documented `22 03` / `1A 05 0093/0077` path | Read and set FD between transfers; measure settling time; toggle during RX and TX (bench only); check whether DV-data buffers survive; check persistence across power cycles; check whether CI-V `22 00` data goes out as fast or slow | Safe toggle procedure defined, or CI-V control marked unsafe for that profile |
| HW-11 | **Received-mode visibility** | Can the app learn the mode of received data? | Compare data-port output and any CI-V RX data/status (`22 01`) for slow vs fast reception | Documented yes/no per profile |
| HW-9 | **Interruption** | Recovery | Unplug the USB mid-transfer, power-cycle the radio, block the signal for 30 s | Transfer resumes or fails cleanly with a correct operator-visible state |

## 4. Performance evaluation protocol (Phase C baseline → Phase D/E comparison)

Systems compared:
1. D-RATS 0.4.2 defaults (512 B × 4, force_delay −2), slow
2. D-RATS on a fast-configured radio
3. Aether Layer-1 (legacy-wire, tuned), slow and fast
4. Aether ALP, slow and fast (Phase E)

Workloads:
- 10 chat messages
- ICS-213 form (~1.2 KB XML)
- files of 1 KB, 10 KB, 50 KB, 200 KB (200 KB on fast only)
- mixed: one file transfer with chat injected every 30 s

Conditions:
- Simulator: BER 0, 1e-5, 1e-4, 3e-4; Gilbert-Elliott bursts; 5 % / 20 % superframe-sync loss.
- Hardware: strong signal (attenuated bench), marginal signal (attenuator stepped to the decode threshold), plus at least one real simplex path and one repeater path.

Metrics, each logged per run:
- goodput (application bytes delivered ÷ wall time)
- protocol overhead (on-air bytes ÷ payload)
- time to first byte / message latency (p50/p95)
- completion time
- success probability (N ≥ 10 runs per cell)
- channel occupancy (key-down time ÷ wall time)
- retransmission ratio
- recovery time after a 30 s outage
- **chat latency during bulk transfer** (responsiveness)

Reporting: each cell carries its validation level. Simulator results are plotted together with the analytic model (`tools/arq_model.py`) so model error is visible. The model is **a hypothesis**: its key-up/tail values are guesses until HW-4 replaces them.

### 4.1 Prior expectation (model, unvalidated)

From `tools/arq_model.py` (assumes 0.6 s key-up, 0.5 s tail, 119 B/s slow ceiling, 435 B/s fast ceiling, iid bit errors, no FEC):

| BER | D-RATS 512×4 slow | D-RATS 512×4 fast | fast ÷ slow | Tuned slow | Tuned fast | tuned fast ÷ tuned slow |
|---|---|---|---|---|---|---|
| 0 | 81 B/s | 199 B/s | 2.45× | 108 B/s | 387 B/s | 3.6× |
| 1e-4 | 47 B/s | 114 B/s | 2.45× | 66 B/s | 227 B/s | 3.4× |
| 3e-4 | 15 B/s | 38 B/s | 2.45× | 39 B/s | 128 B/s | 3.3× |

Hypotheses for hardware tests to confirm or refute:
- **H1**: with legacy parameters, Fast Data yields well under its 3.66× nominal speed-up because per-over overhead dominates.
- **H2**: block and window sizing chosen per mode and per loss recovers most of the nominal ratio.
- **H3**: under marginal conditions, smaller blocks beat 512 B blocks by a large factor, on both modes.
