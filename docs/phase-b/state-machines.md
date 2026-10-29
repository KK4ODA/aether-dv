# Phase B — State-Machine Specifications (v0)

Status: **normative draft** for implementation in Phase C. Every machine is specified as:
- states, events, guards and actions
- timers, with defaults and the evidence behind them
- invariants
- **negative cases**: the legacy defects (P1–P20 in `../02-legacy-protocol-analysis.md` §8) and known failure modes that the implementation's test suite must reproduce and show handled

All machines are **sans-I/O**. They consume events stamped with a monotonic `now` and emit actions. They never read clocks, ports or sockets themselves. This makes every test deterministic in `aether-sim`.

Notation: `S --event [guard] / action--> S'`. Timer names are `T_*`. Defaults marked † are placeholders to be replaced by Phase C hardware measurements (HW-4).

---

## SM-1 Port / radio adapter lifecycle (`aether-radio`)

States: `Absent`, `Identified`, `Opening`, `Open`, `Ready`, `Suspended`, `Conflict`, `Lost`, `Closed`.

| From | Event | Guard | Action | To |
|---|---|---|---|---|
| Absent | `DeviceAppeared(id)` | id matches a configured profile's stable identity | record path | Identified |
| Identified | `OpenRequested` | — | open exclusive (Unix: `TIOCEXCL`; Windows: exclusive by default); set 8N1, profile baud and flow control; **assert DTR** if the profile says so (TH-D75: required per macrats; TM-D750: untested) | Opening |
| Opening | `OpenOk` | — | start the reader; **write nothing** | Open |
| Opening | `OpenFailed(Busy)` | — | report the owning process if the OS can tell | Conflict |
| Open | `ProfileValidated` | operator confirmed, or documented identification succeeded (CI-V `19 00` on a CI-V-designated port only) | — | Ready |
| Ready | `SuspendRequested` | — | stop scheduling; keep the port open | Suspended |
| Suspended | `ResumeRequested` | — | — | Ready |
| Open/Ready/Suspended | `IoError` / `Eof` / `Hangup` / `DsrDropped` | — | close; notify the scheduler (`LinkDown`); transfers → Suspended | Lost |
| Lost | `T_REOPEN` fires | device present | — | Opening |
| Lost | `DeviceAppeared(id)` | same stable identity | — | Opening |
| Conflict | `T_REOPEN` / operator retry | — | — | Opening |
| any | `CloseRequested` | — | drop DTR/RTS; close | Closed |

Timers: `T_REOPEN` uses exponential back-off of 1, 2, 4 … 30 s.

Invariants:
- I1.1 At most one adapter owns a device path.
- I1.2 No bytes are written outside `Ready`.
- I1.3 Unknown ports are never written to.

Negative cases:
- **N1.1** The Windows port wedges after a write timeout (DR#148). Rule: no blocking writes with timeouts on the hot path. On a write error, go to `Lost` and reopen; never retry the write in place.
- **N1.2** USB replug gives a new device node. Re-identify by stable identity (USB serial / by-id path), not by node name.
- **N1.3** The ID-52 PLUS USB function is set to CI-V when the profile expects DV Data. Detect CI-V frames (`FE FE … FD`) arriving on a DV-Data port and report "port function mismatch" with the menu path.

## SM-2 Channel scheduler (`aether-link`, one per RF port)

The scheduler owns the half-duplex turn-taking. It is the only component that decides *when* bytes go to the radio.

States: `Idle`, `HoldOff`, `Assemble`, `Writing`, `AirDrain`, `Listen`, `Receiving`, `Down`.

| From | Event | Guard | Action | To |
|---|---|---|---|---|
| Idle | `QueueNonEmpty` | channel quiet for ≥ `T_HOLDOFF` | — | Assemble |
| Idle | `QueueNonEmpty` | recent RX activity | arm `T_HOLDOFF` + random back-off in `[0, T_JITTER]` | HoldOff |
| HoldOff | timer | no RX since armed | — | Assemble |
| HoldOff | `RxBytes` | — | re-arm | HoldOff |
| Assemble | — | — | build a burst ≤ `max_burst_bytes` and ≤ `max_burst_time` of the ModeProfile, by priority: control/ACK > chat > form > file; never split a DDT2 frame | Writing |
| Writing | `PortAccepted(all)` | — | stamp `tx_end_est = now + air_time(burst) + T_TAIL` | AirDrain |
| Writing | `PortBackpressure` (XOFF) | — | pause; keep pending bytes | Writing |
| AirDrain | `T_AIR` (to `tx_end_est`) | — | — | Listen |
| Listen | `RxBytes` | — | — | Receiving |
| Listen | `T_LISTEN` | — | — | Idle |
| Receiving | quiet ≥ `T_RX_QUIET` | — | deliver frames upward | Idle |
| any | `LinkDown` | — | fail pending writes upward as *not transmitted* | Down |
| Down | `LinkUp` | — | — | Idle |

Timers (defaults):

| Timer | Default | Basis |
|---|---|---|
| `T_HOLDOFF` | 1.0 s† | Icom documents ~0.5 s carrier sense; add margin |
| `T_JITTER` | 1.0 s | Collision avoidance; the legacy fixed random delay 0.5–2 s applies on every burst, we apply it only after RX activity |
| `T_TAIL` | per profile† | Radio TX tail (e.g. TH-D75 Menu 614 0.5 s) |
| `T_RX_QUIET` | 0.3 s | Gap that ends an inbound burst |
| `T_LISTEN` | response window from ARQ | — |

`air_time(n) = n_bytes / profile.payload_rate + profile.keyup`†. This value is **an estimate used only for scheduling**. It never implies delivery.

Invariants:
- I2.1 Exactly one burst is in the radio buffer at a time (no stacking of bursts in the radio).
- I2.2 A burst never exceeds the profile limits.
- I2.3 The scheduler never reports delivery.

Negative cases:
- **N2.1** (P7) The blind random delay on every burst must not exist; delays only follow RX activity.
- **N2.2** (P5) Treating serial write completion as transmission is forbidden; `PortAccepted` only starts `AirDrain`.
- **N2.3** Lost XON (radio never sends XON): after `T_XOFF_MAX` (15 s, legacy value) go to `Down`, `LinkDown(reason=flow)`. Never assume XON silently as legacy does (`comm.py:451-463`).
- **N2.4** (DR#253) Bytes other than XON/XOFF read while paused **must be kept** in the RX stream.

## SM-3 Legacy session control (`aether-ddt2` + `aether-link`), wire-compatible

Implements control session 0 (`control.py`) exactly on the wire, with these local fixes:

| Rule | Legacy behaviour | Aether behaviour |
|---|---|---|
| Session id allocation | wraps with a bug at 255 (P12) | lowest free id ≥ 4; never reuse an id within `T_ID_QUARANTINE` (120 s) of its close |
| Duplicate NEW from (station, rid) | re-ACK the existing session | re-ACK only if that session is younger than `T_ID_QUARANTINE`; otherwise treat it as new (stale-session fix, P12) |
| Unknown session type in NEW | silently no ACK; the opener retries 10× | send no ACK (must stay wire-compatible); log `RefusedSessionType` |
| Self-addressed traffic | dropped as looped, lock never released (P17) | refuse at enqueue time with a clear error |
| NEW retries | 10 × (5 s then 15 s) | same counts; waits derive from the RTO estimate (min 5 s) |
| END | 3 × 15 s | same |

## SM-4 Legacy stateful transfer — ARQ-1 (wire-compatible with D-RATS 0.4.x)

**Sender** states: `Opening`, `Offering` (file/form offer), `Sending`, `Polling`, `Suspended`, `Done`, `Failed`.
- `Sending`: transmit `window` blocks (ascending index) + one REQACK listing their seqs (byte list, 8-bit).
- `Polling`: on `T_RTO`, re-send REQACK only (legacy-compatible). Back-off `4 + 4·attempt` s, but **never earlier than** the RTO estimate (reduces P20).
- On ACK: remove the acked blocks. If any remain, go back to `Sending` with the remaining blocks first.
- Rollover rule: never add seq 0 while blocks are outstanding (legacy rule; required because legacy receivers depend on it).
- Max attempts 10, then `Failed(reason=no_ack)`. On `LinkDown`, go to `Suspended` with outstanding state persisted (legacy peers will time out after 90 s idle; on resume, re-offer with `RESUME:`).

**Parameters chosen by the sender** (all legal for legacy receivers per source analysis; each gated by interop test ADR-0003):
- block size 64–1024 B, chosen from ModeProfile and observed loss (start 256 B slow / 512 B fast)
- window 2–16, legacy hard cap 4 KiB in flight respected
- magic 0x22 for every frame where compression doesn't shrink it (P3, P4)

**Receiver**: legacy semantics (cycle-based `recv_list`), plus Aether diagnostics:
- remember the previous cycle's per-seq content hash
- if a post-cycle "duplicate" differs in content from the stored block, log `PossibleWrapAmbiguity` (P19 signature) and keep the block in a side buffer for forensics. **Do not change ACK behaviour yet**; a content-aware variant must be model-checked first (open item).

RTO estimator: `RTO = srtt + 4·rttvar` (RFC 6298 form). RTT samples are measured from the **end of the estimated air time** of the REQACK to ACK arrival. Clamp to `[T_RTO_MIN = 4 s†, T_RTO_MAX = 60 s]`. Karn's rule applies: no samples from re-polled REQACKs.

*Phase C amendment (stale-ACK guard):* an ACK arriving earlier than **half** the estimated burst air time after the burst started is applied as information only and does not end the wait (P20). The first implementation used the full air time plus one over, which stalled every window until the RTO on paths faster than the estimate (seen on ratflector links). Per-path parameters: ratflector links use zero air time.

Invariants:
- I4.1 The receiver delivers in order.
- I4.2 The sender never reports *delivered* for a legacy transfer; the best state is `HandedOff(hop)` (see SM-7).

Negative cases (each is a required regression test):
- **N4.1** (P19/L1) A >256-block transfer with the first post-wrap block dropped. Expectation: Aether-as-receiver logs `PossibleWrapAmbiguity`; Aether-as-sender to a legacy model shows "unconfirmed" and the UI warns for transfers >256 blocks.
- **N4.2** (P20/L2) Late ACK after a re-poll. Expectation: no duplicate burst before RTO; a duplicate old seq 0 at a cycle end is not delivered twice by the Aether receiver.
- **N4.3** (P1) Repeated packing of an uncompressed frame must yield identical bytes (serialisation is pure).
- **N4.4** (P2) A payload containing `[EOB]`: the sender detects it before transmission and re-chunks or compresses.
- **N4.5** (P8) RESUME for a `.part` whose stored size/hash doesn't match the offer is refused; the receiver restarts from 0.
- **N4.6** (P15) The offer size field is little-endian (byte vector test).

## SM-5 ALP reliable transfer — ARQ-2 (Aether↔Aether only)

Rules verified in the model (`arq-model-findings.md`):
- **R-ALP-1 Span limit.** `newest_seq − oldest_unacked_seq < 2^15` (16-bit seq space). The window limit alone is insufficient (L3).
- **R-ALP-2 Window-relative dedup at the receiver.** Accept seq `s` as new iff `0 < (s − in_order_point) mod 2^16 ≤ 2^15` and it is not already buffered. Otherwise it is a duplicate: discard it and include it in the SACK.
- **R-ALP-3 Poll id.** Each REQACK carries a 16-bit poll id; the ACK echoes it, and ACKs with a stale id are applied only as SACK information and never clear the wait state. This is an efficiency rule, not a correctness rule (see findings).
- **R-ALP-4 SACK.** The ACK carries the cumulative in-order point plus a bitmap of the next 64 seqs.
- **R-ALP-5 Transfer identity.** A transfer is identified by `(sender, transfer_uuid)` plus the SHA-256 of the whole object. Resume is by `(uuid, received bitmap)`, never by filename.
- **R-ALP-6 Completion.** An explicit `Complete(sha256)` from the receiver after verifying the hash. Only this produces `Delivered`.

Block size and window adapt per the ModeProfile and loss estimate. The adaptation algorithm is an open item for Phase E and must be tuned from Phase D measurements, not designed now.

## SM-6 Ratflector client connection (`aether-rfl`)

States: `Idle`, `Resolving`, `Connecting`, `Banner`, `Authenticating`, `Up`, `Draining`, `Backoff`.

| From | Event | Guard | Action | To |
|---|---|---|---|---|
| Idle | `Enable` | — | resolve DNS | Resolving |
| Resolving | `Resolved` | — | TCP connect (timeout 10 s) | Connecting |
| Connecting | `Connected` | — | enable TCP keepalive (idle 60 s, interval 15 s, count 4) | Banner |
| Banner | line `100 …` | — | — | Up |
| Banner | line `101 …` | credentials configured | send `USER call` | Authenticating |
| Banner | `T_BANNER` (5 s; legacy 30 s optional) | — | assume old-school | Up |
| Banner | line not matching `^\d{3} ` | — | assume old-school; **keep the bytes** as stream input | Up |
| Authenticating | `102` | — | send `PASS` | Authenticating |
| Authenticating | `200` | — | — | Up |
| Authenticating | `500`/`501`/`201`/other | — | report `AuthFailed`; no automatic retry | Idle |
| Up | `Eof` / `IoError` / keepalive failure / `T_APP_IDLE` | — | — | Backoff |
| Up | `Disable` | — | flush ≤ 2 s | Draining → Idle |
| Backoff | `T_BACKOFF` | — | — | Resolving |

Timers:
- `T_BACKOFF`: exponential 1 s → 300 s cap, ±20 % jitter, reset after 60 s stable `Up`.
- `T_APP_IDLE`: optional; when no bytes have been received for N minutes on a busy ratflector, send an Aether status frame and expect echo-free silence. Only TCP keepalive is relied upon by default.

Invariants:
- I6.1 Delivery state on a ratflector send is at most `HandedOff(ratflector)`.
- I6.2 Frames received over two connections within the dedup window are delivered once.

Negative cases:
- **N6.1** (P18, DR#75/#109) Silent half-open TCP is detected within keepalive time (≈2 min).
- **N6.2** (R5) 100 clients reconnecting after a server restart spread over ≥30 s (jitter).
- **N6.3** A banner split across TCP segments.
- **N6.4** The server sends 200 to USER: do not send PASS (fixes a legacy quirk; verify no legacy server requires it).
- **N6.5** Platform timing: on Windows a refused connection to a closed port takes ~2 s to fail, where Linux and macOS fail immediately (observed in CI). The driver must use an explicit connect timeout and must not assume connect failures are instant.

## SM-7 Message delivery state (`aether-msg`)

`Draft → Queued → Sending(hop, path, attempt) → HandedOff(hop, evidence) → {Delivered(evidence) | Unconfirmed(reason) | Failed(reason) | Expired}`; operator actions `Retry`, `Reroute`, `Cancel`.

Evidence types:
- `TcpAccepted(ratflector)`: hop only
- `ArqAck(legacy, hop)`: hop only
- `AlpComplete(sha256, peer)`: end-to-end if the peer is the final destination
- `Receipt(msg_id, signer)`: end-to-end

Rules:
- A legacy final hop ends in `Unconfirmed(legacy_peer)`, never `Delivered`.
- `Queued` is durable before the API call returns (ADR-0007).
- Crash recovery: `Sending` → `Queued`, keeping the attempt count.
- Store-and-forward routing follows legacy rules (`../02-legacy-protocol-analysis.md` §6) with path-loop prevention, plus: never route via a station whose path is `Lost` for > `station_msg_ttl`. Self-addressed messages are refused (P17).

## SM-8 Peer Fast Data capability (`aether-link`, per peer × path)

`Unknown → Declared(source) → Proven(at) → Stale(after T_PROOF_AGE = 24 h) → Unknown`; any state → `Disproven(at)` when ≥3 consecutive FD transfers to the peer fail while a slow transfer succeeds within 10 min. `Disproven` expires after `T_DISPROOF_AGE` (24 h).

Sources for `Declared`: operator, capability record (`fd=on` in the RPCGetVersion extension). Profile flag `fd_only` (e.g. Kenwood TH-D74/D75 reports) sets `Declared(fd_required)`.

Mode selection (ADR-0005, owner decision D5):
- `Slow` (default on shared nets): never toggle the radio.
- `Fast`: the operator asserts FD; warn on sends to peers that are not `Proven`/`Declared`.
- `Auto`: may toggle the radio via CI-V only if the profile has `fd_control=verified` (HW-10 passed), only between transfers, only toward `Proven` peers, and must restore the prior state. Every toggle is logged.

## Traceability

| Defect / risk | Covered by |
|---|---|
| P1 | N4.3 |
| P2 | N4.4 |
| P3, P4 | SM-4 parameters |
| P5 | N2.2, SM-4 RTO |
| P7 | N2.1 |
| P8 | N4.5 |
| P12 | SM-3 |
| P15 | N4.6 |
| P16 | SM-6 (full-duplex TCP) |
| P17 | SM-3, SM-7 |
| P18 | N6.1 |
| P19 | N4.1 |
| P20 | N4.2, R-ALP-3 |
| DR#148 | N1.1 |
| DR#253 | N2.4 |
| R7/R8 (loops, dedup) | SM-6 I6.2 + `../04-ratflector-analysis.md` §5.2 |
