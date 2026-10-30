# 10 — D-STAR repeater operation: findings and proposed design

Date: 2026-10-30 · Status: **proposal, no code changed** · Decision record: [ADR-0009](adr/0009-repeater-path-scope-and-channel-sense.md) (Proposed)

Evidence tags are the ones used in `03`:
- **[SRC]** read in source (repo@commit:file:line)
- **[DOC]** manufacturer or JARL documentation
- **[COMM]** community report
- **[MODEL]** estimate
- **[UNK]** unknown, needs hardware or on-air validation

Sources read for this document:
- ircDDBGateway `f5ab9f7`
- MMDVMHost `9384d68` (2026-10-18)
- D-Rats `d0e7a39`
- drats2 and macrats (local clones)
- Icom *IC-9700 CI-V Reference Guide* (rigpix `ic9700_civ_v2.pdf`)

Nothing here has been tested through a real repeater.

## 0. Summary

Repeater use will be the common case, and Aether DV already works through a repeater in the same way D-RATS does: it writes bytes to the radio, and the radio's own UR/R1/R2 settings decide where they go. Neither program knows or checks those settings. Three gaps follow from that:

1. **Scope is invisible.** Whether data stays on the repeater or goes out to every repeater on a linked reflector is decided by one radio field: R2 (RPT2) set to the gateway. Nothing in Aether DV or D-RATS shows it, checks it or limits it.
2. **Voice is invisible.** We treat the channel as busy only when *data* bytes come out of the radio. A voice QSO produces no data bytes, so Aether DV can key up in the middle of one. D-RATS has the same flaw.
3. **Timing is simplex timing.** The scheduler has no idea of repeater acknowledgements, voice break-in gaps, time-out timers or how much of the repeater's time we are using.

The proposal adds one model with three parts, all inside the existing crates and state machines:
- **Path and scope.** Each radio port declares how it reaches others: direct, through a repeater, or through a repeater onto its linked reflector. Scope is a property of the port, not of each message.
- **Route guard.** Before transmitting, the engine checks that the radio's actual UR/R1/R2 match the declared scope. It reads them over CI-V where that is documented, and otherwise asks the operator to confirm once per session. A mismatch holds the queue; it does not transmit and warn.
- **Channel sense and courtesy.** The scheduler takes busy and clear events from several sources:
  - data bytes (all radios)
  - CI-V DV RX status, which includes "receiving a voice call" (documented for the IC-9700)
  - squelch status

  It yields to voice with a break-in gap, caps the length of each over, and keeps a rolling air-time budget. ARQ timers pause while the channel is held by someone else.

No change to the DDT2 wire format is needed. Every timing change is sender-local, as ADR-0003 already allows, so D-RATS stations on the same repeater interoperate unchanged.

## 1. How data reaches a D-STAR repeater

### 1.1 The radio decides the route

- The data port carries only payload. The radio wraps it in an RF header built from its current MY/UR/R1/R2 settings and keys up ("DV Data TX: Auto") [DOC: Icom manuals, `03` §2]. The header the radio builds in DR mode is **[UNK]** until HW-R1.
- D-RATS never sets or reads those fields. A grep of D-Rats `d0e7a39` and drats2 finds no RPT1/RPT2/URCALL handling. Their "repeater" is the TCP ratflector [SRC].
- macrats talks to an MMDVM modem directly. It builds its own header with UR `CQCQCQ` and RPT1/RPT2 blank (`MMDVMProtocol.swift:211-227`), so it works direct only [SRC].

### 1.2 What a repeater does with the header

**MMDVMHost** (`DStarControl.cpp`) [SRC]:

| Header field | Effect |
|---|---|
| Repeater flag not set (radio in simplex) | Rejected: "non repeater RF header" (`:243`) |
| R1 ≠ this repeater's call + module | Rejected: "wrong repeater" (`:251`) |
| `SelfOnly`, black/white lists | Can reject the caller (`:258-269`) |
| R2 = gateway call (`xxxxxx G`) | Repeated on RF **and** sent to the gateway (`m_net`, `:272`, `:308-315`) |
| R2 anything else (blank, or the repeater's own call) | Repeated on RF only; never sent to the network |

After each RF over, if `AckReply=1` (default), the repeater waits `AckTime` (default 750 ms), then transmits an acknowledgement:
- a header addressed to the caller
- 19 slow-data frames of text, then EOT, about 0.5 s in all (`sendAck`, `:1218-1300`; ini `[D-Star] AckTime=750`)

The ack text includes BER, and the **name of the linked reflector** when linked. A new RF header cancels a pending ack (`m_ackTimer.stop()`, `:278`). `Timeout=180` cuts an RF over at 180 s. Multi-mode repeaters stay in D-STAR mode for `RFModeHang=10` s after activity, and ignore D-STAR while busy in another mode.

**ircDDBGateway** (`RepeaterHandler.cpp`) [SRC]. Applies only to traffic the repeater hands it, which for MMDVMHost means R2 = gateway:

| Condition | Effect |
|---|---|
| Any header | Copied to **incoming links**, CCS, the APRS writer and the D-RATS bridge before any routing decision (`:592` "Incoming links get everything"). Incoming links are dongles and repeaters that linked *to* this one. |
| R2 blank or the repeater's own call | Local; stops (`:644` "Reject silly RPT2 values") |
| R2 another local module | Cross-band to that module (`:648`) |
| UR `CQCQ…` | Sent to the **outgoing link**, the reflector this repeater is linked to (`:672`) |
| UR ends in `L` / `U` | **Link / unlink command** (`reflectorCommandHandler`, `:2060-2078`) |
| UR starts with `/` | Route to a repeater (`:1942`) |
| UR is another call sign | Call-sign routing via ircDDB to wherever that station was last heard |
| UR `       E` / `I` / `V` | Echo, info and version services |

**Icom controllers** (ID-RP2C, RS-RP3C, ID-RP4000):
- Local and gateway behaviour should match the convention above ("Local CQ" versus "Gateway CQ" in DR mode) [COMM].
- Whether local traffic ever reaches the gateway's incoming links is **[UNK]**.
- Whether they send an ack Aether DV can read is **[UNK]**.

### 1.3 What this means for containment

| Scope | Radio setting | Who receives it | Enforceable by us? |
|---|---|---|---|
| **Direct** | Simplex, R1/R2 blank | Stations in RF range of us | Yes. A repeater rejects non-repeater headers [SRC: MMDVMHost]. |
| **This repeater** | R1 = repeater + module, R2 blank (not `G`), UR `CQCQCQ` | Everyone in range of the repeater | **Yes for MMDVMHost repeaters** [SRC]. Probably for Icom controllers [COMM, UNK]. The repeater's own config can still add listeners: incoming links if the controller forwards local traffic (Icom [UNK]), or a sysop's D-RATS bridge. |
| **Linked reflector** | R2 = `xxxxxx G`, UR `CQCQCQ` | Everyone on every repeater, hotspot and dongle connected to whatever the repeater is linked to *at that moment* | We can only enforce that we *chose* this scope. The reach is set by the repeater's current link, which the sysop or any user can change at any time by voice command. It may be unlinked (local only) or linked to a module with dozens of repeaters. |
| **Routed** (future) | R2 = `G`, UR = station call | That station's last-heard repeater, plus our gateway's incoming links | Not proposed for v1 |

Two hazards follow directly from the source:
- **Leftover UR commands.** If the operator last used the radio to link a reflector (UR `REF030CL`), every Aether DV transmission with R2 = `G` re-sends that link command. A leftover unlink (`       U`), echo (`       E`) or call sign does the same. The route guard must block any UR other than `CQCQCQ` when R2 = `G`.
- **Inbound traffic cannot be filtered.** In every scope we receive whatever the repeater transmits, including reflector traffic. Containment of *replies* is therefore ours to do. Aether DV currently answers broadcast pings and echo requests (rate-limited, 0–5 s and 0–10 s random delay; `station.rs:174-240`), so a broadcast ping from one station on a large reflector draws a reply from every Aether DV station on every linked repeater.

### 1.4 What the radio can tell us (Icom CI-V)

Read in the IC-9700 CI-V Reference Guide [DOC], pp. 19–20:

| Command | Meaning | Use |
|---|---|---|
| `1F 00` | MY call sign (8) + note (4), read/set | Check the radio's call sign matches the station call sign |
| `1F 01` | **DV TX call signs: UR, R1, R2** (8 chars each), read/set | Route guard: verify, and with permission set, the scope |
| `20 00 00/01/02` | Auto DV RX call sign output on/off; transceive output; read. Payload has header flag bytes (bit 3 "through repeater", bit 4 voice/data, plus the ack/control code in byte 2), caller, called, RXRPT1, RXRPT2. RXRPT1 is the **gateway call when the call came through the internet**. | Who is talking; detecting traffic that came from the network; seeing the repeater's ack to us |
| `20 01 …` | DV RX message (20 chars) + caller | Reading the MMDVMHost ack text: BER, linked reflector |
| `20 02 00/01/02` | **DV RX status**: bit 6 *receiving a digital voice call*, bit 4 *receiving a signal*, bit 1 *signal other than DV*, bit 0 *packet loss*. Can be pushed (transceive) or read. | Channel sense, including voice and other modes |
| `15 01` | Noise / S-meter squelch status | Simpler busy fallback |
| `00`, `01` (transceive) | Frequency / mode changed | Re-verify the route after a repeater change |

For the other models:
- The ID-52 family numbers its `22 xx` commands differently from the IC-9700 (`22 03` is Fast Data there), so none of the IC-9700 table carries over without the model's own guide. `1F`/`20` support on ID-52, ID-52 PLUS, ID-50, ID-5200, IC-705 and IC-905 is **[UNK]** until checked against each guide.
- On ID-52-family radios, USB is CI-V *or* DV Data, never both (`03` §2). Route checks and channel sense there need Bluetooth for one of the two.
- Kenwood TH-D74, TH-D75 and TM-D750 have no documented control path **[UNK]**.

## 2. D-RATS and its forks on repeaters

| Behaviour | D-RATS 0.3.x/0.4.x (`d_rats/transport.py`, `config_defaults.py`) | Assessment |
|---|---|---|
| Route / scope | None; whatever the radio is set to | Gap: accidental reflector-wide sends are easy, and invisible |
| Busy detection | None. `sniff_packets` off. | Gap: steps on voice and on other data |
| Access delay | `force_delay = -2`: one random 0.5–2 s wait before draining the queue (`send_frames`, `:335`) | Keep the idea: randomised access. Too short for repeater acks and voice turnarounds. |
| Warm-up frame | Optional type-254 frame of `\x01` × `warmup_length` when idle longer than `warmup_timeout`; off by default (`warmup_timeout = 0`) | Not needed on Icom (radio keys up and sends a header). Keep decoding it (we already drop src `!`). |
| Over length | `ddt_block_size 512` × `ddt_block_outlimit 4` with `pipelinexfers`, so about 2 KB, about 17–20 s per over on slow data [MODEL] | Too long for a shared repeater; no voice break-in for 20 s |
| Duty / courtesy | None. QSTs (scheduled beacons) **enabled by default** (`qsts_enabled True`). | Gap |
| Auto replies | Answers pings and RPC requests such as version | Gap on linked repeaters |
| Repeater-side bridge | ircDDBGateway `DRATSServer`. A sysop-enabled TCP port that receives the repeater's **local RF** `0x3X` slow data framed at `[EOB]`, and transmits TCP input as the repeater's call sign (MY `xxxxx`/`DATA`, UR `CQCQCQ`) [SRC]. Slow data only, no Fast Data. | Optional later port type ("repeater bridge") for a station at the repeater site |

Community experience is that D-RATS data "runs through the repeater with no further work on the repeater" [COMM: YRARC]. That matches the routing above: R2 local works anywhere, and R2 = `G` reaches the reflector.

**Conclusion.** There is nothing in D-RATS or its forks to match beyond the randomised access delay and the decision to leave framing to the radio. Everything in §0 is an improvement, and none of it changes what goes on the wire.

## 3. Audit of Aether DV today

| Area | Current state (file) | Gap |
|---|---|---|
| Port model | `[[port]] kind = "serial"` with `mode = slow/fast`, optional `[port.civ]` used only for Fast Data (`config.rs`) | No notion of path, repeater or scope |
| Scheduler | SM-2, sans-I/O (`aether-link/src/scheduler.rs`). Busy = data bytes from the radio (`on_rx`). Hold-off 1 s + 0–1 s jitter. Max over 25 s / 2400 B. All values placeholders. | Voice invisible. No break-in gap between our overs. No repeater ack tail. No duty budget. No operator hold. |
| Profiles | `ModeProfile::dv_slow_placeholder` / `dv_fast_placeholder` | One profile per mode; path not modelled |
| ARQ | `arq_config(path_kind)`: RTO 12 s on RF (`engine.rs:160`) | RTO keeps running while someone else holds the channel. A long voice QSO causes spurious retransmissions, then failure. |
| CI-V | `aether-civ`: frame codec + Fast Data commands; `aetherdv/src/civ.rs` one-shot read/set | No call-sign, RX status or squelch commands; no long-lived CI-V session |
| Auto replies | Broadcast ping/echo answered with rate limit and random delay (`station.rs`) | No scope awareness |
| UI | Port strip, Diagnostics, Radio Setup guides | No scope, route or channel state; no air-time estimate; no hold |

What already fits: the scheduler is sans-I/O with priority queues; ports never claim delivery; CI-V is opt-in with read-only default (`allow_set`); ADR-0005 already treats radio settings as operator-owned. The design below extends these rather than adding parallel paths.

## 4. Proposed design

### 4.1 Path and scope (configuration model)

Each radio port gets a `path` table. Without one, the port behaves as **direct**, which is today's behaviour.

```toml
[[port]]
kind = "serial"
name = "IC-9700"
path = "/dev/serial/by-id/…-if01"   # existing
mode = "slow"

[port.path]
via = "repeater"            # direct | repeater
repeater = "W4DOC  B"       # R1: call sign padded to 7 + module
scope = "repeater"          # repeater | reflector   (only when via = repeater)
# Advanced, all optional; defaults come from the preset for via + scope
# holdoff_ms, jitter_ms, voice_gap_ms, min_gap_ms, max_over_ms,
# duty_percent, duty_window_s, verify = "civ" | "operator"

[port.civ]                  # existing; now also used for route + channel sense
path = "/dev/serial/by-id/…-if00"
allow_set = false
```

- `scope` is a property of the port, chosen once and shown everywhere. A per-message scope would make the radio's route change under queued traffic, and the radio cannot change R2 between overs without CI-V writes.
- An operator who wants both scopes defines two ports on the same radio; only one may be active at a time. This is a later convenience, not v1.
- The gateway call sign is derived as `repeater[0..7] + "G"`.

### 4.2 Route guard (engine, pure decision function)

A pure function in `aetherdv/src/route.rs` maps (declared scope, observed UR/R1/R2 or none, how they were observed) to a verdict:

| Observed | Declared `repeater` | Declared `reflector` |
|---|---|---|
| R1 = repeater, R2 blank or ≠ `G`, UR `CQCQCQ` | **Allow** | **Hold**: "radio is set to this repeater only" |
| R1 = repeater, R2 = `G`, UR `CQCQCQ` | **Hold**: "radio would send to the linked reflector" | **Allow** |
| R2 = `G` and UR ≠ `CQCQCQ` | **Block**: link command, unlink, echo or call-sign route | **Block** (same) |
| R1 ≠ declared repeater | **Hold**: "radio is set for W4DOC C, port is for W4DOC B" | same |
| R1/R2 blank (simplex) | **Hold**: "radio is in simplex" | same |
| Not readable (no CI-V or unsupported) | **Allow after operator confirmation** for this session; shown as *not verified* | Same, plus the reflector confirmation in §4.6 |

Rules:
- *Hold* means the port queue stops and nothing is written to the radio. Queued items are not failed while held; they wait.
- The UI offers to fix the setting:
  - **Set radio for this scope** writes UR/R1/R2 via `1F 01`. Only offered with `allow_set = true` on a model where `1F 01` is documented and hardware-tested.
  - **Show me how** opens the Radio Setup guide for that model.
- **When to check:**
  - at port open
  - before the first over after any CI-V frequency/mode transceive event (a repeater change in DR mode changes frequency)
  - before any over if the last check is older than 60 s
- A CI-V read is about 30 bytes at 19200 baud, so it is cheap.
- Also checked: MY call sign (`1F 00`) against the station call sign. A mismatch is a warning, not a block; the operator may run a club call.

### 4.3 Channel sense (aether-link, sans-I/O)

A small aggregator in `aether-link/src/channel.rs` combines sources into one channel state, `Clear` or `Busy { kind: Data | Voice | Signal | OtherMode }`, with the time it started:

| Source | Radios | Gives |
|---|---|---|
| Data bytes from the DV Data port (existing `on_rx`) | All | Busy(Data) while bytes arrive, clear after `rx_quiet_ms` |
| CI-V `20 02` transceive (preferred) or polled every 250 ms | IC-9700 [DOC]; others [UNK] | Voice (bit 6), Signal (bit 4), OtherMode (bit 1) |
| CI-V `15 01` polled | IC-9700 [DOC]; others [UNK] | Signal |
| CI-V `20 00` RX call signs | IC-9700 [DOC] | Attribution: caller, UR, R1/R2. Flags network-originated traffic and our repeater ack. |

- The scheduler's `on_rx` becomes `on_channel(now, ChannelEvent)`; data bytes are one event source among several. This keeps the single-writer rule: the scheduler is still the only thing that decides when bytes go to the radio.
- The port reports which sources are live. The UI says "Busy detection: data only" when CI-V is absent, so the operator knows voice is not being sensed.
- Collisions: on a repeater every station hears the same output, so carrier sense works better than on simplex, where hidden terminals are common. Two stations can still key in the same instant. Randomised hold-off spreads them out and ARQ recovers.

### 4.4 Scheduler changes (SM-2 v2)

The state machine keeps its states and gains these rules. The preset values are starting points to be replaced by HW-R measurements, as the existing placeholders are.

| Rule | Direct (today) | Repeater preset | Reflector preset | Why |
|---|---|---|---|---|
| Hold-off after data ends | 1.0 s | 1.5 s | 2.0 s | Repeater latency and ack tail |
| Random jitter added | 0–1 s | 0–1.5 s | 0–2 s | Spread simultaneous replies |
| Assumed repeater tail without CI-V | 0 | 1.5 s | 1.5 s | MMDVMHost ack: 750 ms + about 0.5 s burst [SRC] |
| Gap after **voice** ends before non-control traffic | n/a | 10 s | 15 s | Voice turnarounds are 1–5 s; don't jump into a QSO |
| Minimum gap between our own overs | 0.3 s | 2.5 s | 4 s | Lets a voice user break in |
| Max over | 25 s | 12 s | 8 s | Courtesy; well under repeater and radio time-out timers (180 s MMDVMHost default) |
| Air-time budget (rolling window) | none | 25 % per 10 min | 10 % per 10 min | Not monopolising the repeater |
| File transfers | allowed | allowed | blocked unless confirmed (§4.6) | Reflector reach |
| Fast Data | per ADR-0005 | only toward peers proven Fast on this path | off; advanced override only | FD disrupts voice users (beeps, no audio) and is unproven through repeater and reflector paths [`03` §1, R20] |

- **Priorities:** Control and Chat bypass the air-time budget up to a small reserve (e.g. 2 % of the window), so ACKs and short chat never starve. Forms and Files are throttled first.
- **Voice yield:** while the channel is Busy(Voice), only ACKs for an in-progress transfer may go, and only after the voice gap. Everything else waits.
- **Operator hold:** a per-port *Pause sending* flag. Nothing is written while it is set, and the queue is preserved. This is the main tool when voice can't be sensed.
- **Budget exhausted:** state *Courtesy pause*, with the time it ends, shown in the strip and the Transfers view.

### 4.5 ARQ while the channel is held

`LegacySender` gets `pause(now)` / `resume(now)`. While the scheduler reports the channel held by others (busy, voice gap, courtesy pause, operator hold), RTO and window timers stop. This is local, so no protocol change is needed. Both ends hear the same repeater output, so both pause at the same times without coordinating.

Legacy D-RATS peers do not pause. A long voice QSO can still time out a transfer to a D-RATS station. The engine reports this honestly ("peer timed out while the channel was busy"), and the transfer can be resumed.

### 4.6 Containment safeguards (beyond the route guard)

- **Reflector scope is opt-in twice.** It is chosen in port setup with an explanation. It is then confirmed once per session on the first send, with the linked reflector named when known: "Data on this port goes to every repeater linked to REF001 C."
- **Link change detection.** Traffic arriving with RXRPT1 = gateway (`20 00`), or an MMDVMHost ack naming a reflector (`20 01`), records the current link. If the link changes while scope = reflector, the queue holds and asks again.
- **Files in reflector scope** need a confirmation showing size and estimated air time, for example: "about 3 min of air time on every linked repeater". A limit (default 8 KiB) applies above which it is refused.
- **Auto replies:**
  - Broadcast ping/echo replies are off in reflector scope.
  - In repeater scope they are off for requests that arrived via the gateway, when CI-V attribution shows that.
  - Directed requests (to our call) are still answered.
- **No automatic beacons** in any scope. Aether DV has none today; keep it that way unless a future beacon feature is limited to direct and repeater scopes.

### 4.7 Voice and data together

D-STAR lets the *same* transmitter carry voice and slow data together; two stations cannot transmit on one repeater at once. "Simultaneous voice and data" therefore means two things:

1. **Coexistence**: interleaving data between voice overs without stepping on them. §4.3–4.5 cover this.
2. **Data inside voice**: with the radio's DV Data TX set to **PTT**, bytes written to the port go out inside the operator's next voice transmission [DOC: Icom manuals].
   - Useful for chat lines and position reports.
   - Unsuitable for ARQ, because timing is in the operator's hands and the radio's buffer size is [UNK].
   - Proposed as a later per-port option, "Send with my voice", that only allows Chat and Position and shows "waiting for your next PTT".
   - Fast Data cannot be used with voice; voice PTT reverts to slow data [DOC: `03` §1].

Receiving data inside other stations' voice (D-PRS positions and similar) already works and is unaffected.

### 4.8 Repeater diagnostics

- **Repeater access check**: Diagnostics sends one short directed frame to our own call (no one else replies), then reports:
  - whether the repeater acked (MMDVMHost, via `20 00`/`20 01`)
  - BER from the ack text
  - the linked reflector, if any
  - the time from end of over to ack

  This separates "repeater didn't hear me" (wrong R1, offset, tone, range) from "no station answered". Without CI-V it is shown as unavailable rather than guessed.
- **Path record per port:** last verified route, last known link, busy sources available, and measured repeater latency. This feeds the support bundle.

### 4.9 Where the code goes

| Crate / module | Change |
|---|---|
| `aether-civ` | `dstar` module: encode/decode `1F 00`, `1F 01`, `20 00`, `20 01`, `20 02`, `15 01`; per-model capability flags (`Documented` / `Unverified` / `None`) |
| `aether-link::channel` | New: channel-state aggregator (sans-I/O) |
| `aether-link::scheduler` | `on_channel`, voice gap, min gap, air-time budget, hold; profile = mode × path preset |
| `aether-link::legacy_arq` | `pause` / `resume` |
| `aetherdv::route` | New: route-guard decision function + tests |
| `aetherdv::civ` | From one-shot reads to a long-lived CI-V session task per radio (poll + transceive), emitting `RadioState` events to its port |
| `aetherdv::ports` / `engine` | Consume `RadioState`; enforce hold/block; scope rules for auto-replies and files; ARQ pause |
| `aetherdv::config` | `[port.path]` |
| API | `ports` gains `path { via, repeater, scope, route { state, ur, r1, r2, checkedAt, source }, channel { state, kind, since, sources }, courtesy { used, budget, pausedUntil }, link { reflector, seenAt } }`. New commands: `ports.setPath`, `ports.verifyRoute`, `ports.applyRoute` (needs `allow_set`), `ports.hold` / `ports.resume`, `diag.repeaterCheck`. Events: `port.route`, `port.channel`, `port.courtesy`. |
| Store | Path record per port (migration 0002) |

### 4.10 Protocol changes

None to DDT2 or the legacy sessions. The ALP (ADR-0004) needs nothing for this either, because pausing is local and symmetric. One optional Aether-to-Aether addition is left for later: a capability flag saying "I pause timers on busy". It would let an Aether DV sender use longer windows toward Aether DV peers.

## 5. User experience

### 5.1 Port setup: "How do you reach other stations?"

1. **Direct (simplex)**, or **Through a repeater**.
2. Through a repeater: enter the repeater call sign and module. With CI-V, *Use the radio's current repeater* fills it from R1.
3. **Who should receive your data?**
   - **This repeater only** (recommended, the default). Text: "Stations in range of W4DOC B. Stays local even if the repeater is linked."
   - **Everyone on the linked reflector**. Text: "Every repeater and hotspot connected to the reflector W4DOC is linked to right now, which can change at any time. File transfers are limited."
4. The route check runs at once and shows the radio's UR/R1/R2 next to what this scope needs, with *Set radio* (CI-V, permitted) or *Show me how* (Radio Setup guide).

The wording follows Icom's own DR-mode terms where possible ("Local CQ" versus "Gateway CQ") so the radio and the app use the same language. The exact menu wording is to be checked per model in the guides.

### 5.2 Always visible (port strip)

Repeater ports show, in plain words:
- the repeater and scope
- route: *verified*, *not verified* or *held*
- channel: *clear*, *voice*, *data*, *other mode*, *courtesy pause 0:42* or *paused by you*

A held route turns the plate amber with the reason. A *Pause sending* control is on the plate and on a keyboard shortcut.

### 5.3 At send time

- Compose shows the estimated air time on RF ports ("about 14 s on air").
- Reflector scope adds the once-per-session confirmation and the file confirmation (§4.6).
- Transfers show *waiting for a clear channel* or *paused for courtesy* instead of a stalled progress bar.

### 5.4 Advanced (collapsed)

- Hold-off, jitter, voice gap, minimum gap, max over, air-time budget
- Route check: CI-V or operator; recheck interval
- Answer broadcast pings
- Fast Data on this path (ADR-0005 rules still apply)

Out-of-range values are refused with the reason. The presets are restored with one action.

### 5.5 Radio Setup tab

Each radio guide gets a "Using a repeater" section:
- how to set Local CQ versus Gateway CQ
- where R1/R2 are on that model
- whether CI-V route checks are available on it

Field reports keep improving these, as they do for the other guide sections.

## 6. Testing strategy

| Level | What | Pass criteria |
|---|---|---|
| Unit (sans-I/O) | Scheduler under scripted channel events. Property tests over random event sequences. | Never starts an over while Busy. Never starts non-control traffic within the voice gap. Air time within budget over every window. Chat not delayed by files beyond one over. Hold transmits nothing and loses nothing. |
| Unit | Route-guard decision table, every row in §4.2 plus padding/case variants | Exact verdicts; UR link/unlink/echo/route always blocked with R2 = `G` |
| Unit | CI-V `dstar` codec | Golden frames built from the IC-9700 guide layouts; round-trip; truncated/garbage input rejected |
| Unit | ARQ pause/resume | No retransmission while paused; RTO resumes with remaining time |
| Simulation (new harness in `aetherdv/tests`) | Repeater model following the MMDVMHost rules in §1.2 (R1/R2 checks, 750 ms ack, time-out), a reflector hub with N repeaters, a voice traffic generator (QSOs with 1–5 s turnarounds), D-RATS-like peers (0.5–2 s random delay, no busy sense) and Aether DV peers with and without CI-V | Repeater scope: zero frames on other repeaters. Voice overs stepped on: 0 with CI-V sense, measured and reported without. Message delivery ≥ 99 % at the given load. Air time ≤ budget. |
| Bench, no RF path to others | MMDVM duplex hotspot as a mini-repeater with MMDVMHost + ircDDBGateway, linked to a **private** XLX/loopback reflector. IC-9700 and ID-52 PLUS as stations. | R2 local stays off the reflector; R2 `G` reaches it; ack timing and text match the source; route guard and channel sense react to real radios; Fast Data pass-through by MMDVMHost build date |
| On air | A cooperating repeater (e.g. W4DOC or WX4GPB) with trustee permission, off-peak, first **unlinked**, then linked to a quiet or private module agreed with the trustee | §7 measurements. No reflector-scope test on a public, busy reflector. |

The simulator runs in CI. Bench and on-air results are recorded at level A in `07`, with captures from both stations.

## 7. Validation register (needs hardware or on air)

| ID | Question | How |
|---|---|---|
| HW-R1 | Does the data TX header use the current UR/R1/R2, including in DR mode? Does `1F 01` read and write them in VFO, memory and DR mode? | IC-9700 + hotspot header log |
| HW-R2 | `1F`/`20`/`15` support and payloads on ID-52 PLUS, ID-52, IC-705, IC-905, ID-50, ID-5200 | Each model's CI-V guide, then the radio |
| HW-R3 | Does an Icom radio with DV Data TX Auto key up while receiving, or wait? How long from first byte to RF header (carrier sense about 500 ms [DOC])? | Bench, scope on the hotspot RX |
| HW-R4 | `20 02` latency: time from repeater carrier to status change; behaviour during our own TX and the repeater ack | Bench |
| HW-R5 | Repeater latency (input to output) and ack timing on MMDVMHost and an Icom controller | Bench + on air |
| HW-R6 | Icom controller: does local (R2 not `G`) traffic reach ircDDBGateway or incoming links? Does it send an ack, and what? | On air with a trustee, or the trustee's gateway log |
| HW-R7 | Radio buffer size and behaviour in DV Data TX = PTT ("send with my voice") | Bench |
| HW-R8 | Fast Data through the target repeaters (MMDVMHost build date; Icom unknown), and its effect on voice users | Bench, then on air with permission (R20) |
| HW-R9 | Kenwood TH-D74/75 and TM-D750: any way to read UR/R1/R2 or busy status | Manuals + bench |
| HW-R10 | Final preset values (§4.4) from measured delivery and voice-interference figures | Simulation, tuned to bench and on-air data |

## 8. Implementation plan

Each step is a reviewable PR with tests. Behaviour for direct ports does not change until step 3, and then only through the shared scheduler code paths.

1. **R-1 Path, scope and route guard.**
   - `[port.path]`, the `route` module, CI-V `1F 00`/`1F 01` read, operator confirmation without CI-V
   - auto-reply scope rules
   - port strip plate and the setup flow
   - API fields and events
2. **R-2 Channel sense.**
   - CI-V session task
   - `20 02` / `15 01` / `20 00` / `20 01`
   - `aether-link::channel`
   - scheduler `on_channel` and voice gap; ARQ pause
3. **R-3 Courtesy.**
   - presets per path, minimum gap, max over, air-time budget, operator hold
   - air-time estimates and send-time confirmations in the UI
4. **R-4 Diagnostics and guides.**
   - repeater access check, link detection, path record
   - "Using a repeater" in the Radio Setup guides
5. **R-5 Validation.**
   - simulator scenarios in CI
   - bench with a hotspot and private reflector, then on air (§7)
   - replace the presets with measured values

Later, each needing its own decision: "send with my voice" mode, call-sign-routed scope, a repeater-bridge port for ircDDBGateway's D-RATS server, Fast Data on proven repeater paths.

## 9. Decisions for the owner

1. Default scope for new repeater ports: **this repeater only** (recommended).
2. Reflector scope: allow with the double confirmation and file limits above (recommended), or don't offer it at all in v1.
3. Writing UR/R1/R2 via CI-V: offer only with `allow_set` and after HW-R1 on that model (recommended), or never write and only guide.
4. Default air-time budgets: 25 % repeater, 10 % reflector, per 10 min (proposed). Any local repeater norms to respect, for example W4DOC/WX4GPB trustee preferences?
