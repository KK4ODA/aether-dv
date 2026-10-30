# 04 — Ratflector Protocol Analysis and Server Requirements

Evidence: `D-Rats@d0e7a39` (`d-rats_repeater.py`, `d_rats/comm.py`, `d_rats/transport.py`); `ham-radio-software/ratflectors@4552e3a` (`ratflectors.yml`); `w9fyi/macrats@db33eba` (`RatflectorHandshake.swift`, tests); `g4klx/ircDDBGateway@f5ab9f7` (`Common/DRATSServer.cpp`).

Validation gap: this research environment cannot open raw TCP connections. All seven public ratflectors in `ratflectors.yml` timed out from here, and two names did not resolve (`ae5he.ham-radio-op.net`, the legacy default `ref.d-rats.com` from `config_defaults.py:249`). These results say nothing about whether the servers are actually up. Live interop testing is a Phase C task, to run from an operator's network.

---

## 1. What a ratflector is

A ratflector is the D-RATS "repeater" program (`d-rats_repeater.py`) listening on TCP port 9000. It is a **frame-level flooding hub** for DDT2 frames. It is not a chat server, not a store-and-forward node, and not a bridge with delivery semantics. Clients connect to it the same way they connect to a radio: the TCP socket is just another `DataPath` carrying `[SOB]…[EOB]` frames (`comm.py:988-1261`).

A single repeater process can also own **serial paths** (radios) and **outgoing net paths** (other ratflectors). This is how RF↔Internet bridges and ratflector-to-ratflector links are built (`d-rats_repeater.py:540-620`).

## 2. Connection and authentication

```
client                                       server
  ── TCP connect :9000 ───────────────────────▶
  ◀─ "100 Authentication not required\r\n"      (require_auth False, or trust_local and 127.0.0.1)
       … or …
  ◀─ "101 Authorization required\r\n"
  ── "USER <call>\r\n" ───────────────────────▶
  ◀─ "102 USER okay\r\n"
  ── "PASS <password>\r\n" ───────────────────▶
  ◀─ "200 Authorized\r\n"  |  "500 Not authorized\r\n"
  (errors: "501 Invalid Syntax", "201 Protocol violation")
  ═══ DDT2 byte stream both directions ═══
```

Client side (`comm.py:1027-1107`): it waits up to **30 s** for the first line. If none arrives or it cannot be parsed, it assumes an "old-school" ratflector that sends no banner, and proceeds. In the worst case that costs 30 s on every connect. Credentials come from `users.txt` on the server, are sent in **cleartext**, and are compared in plain text (`d-rats_repeater.py:340-370`). macrats reproduces all branches and has 13 handshake tests (`RatflectorHandshakeTests.swift`), which makes it a useful second reference.

## 3. Forwarding algorithm (`d-rats_repeater.py:205-246`)

For each frame decoded from connection *P*:
1. Drop it if `d_station == "!"` (warm-up).
2. If it is chat session 1 from CQCQCQ→CQCQCQ and starts with `$`, copy the raw data to the GPS side-port sockets.
3. Learn `calls[s_station] = (P, now)`.
4. If `d_station` was heard on connection *Q* within **600 s** and *Q* is alive, **unicast** to *Q* and stop.
5. Otherwise **flood** to every connection except *P*.

Frames are re-serialised with `frame.get_copy()` / `get_packed()`. The ratflector therefore **decodes and re-encodes** every frame. Frames that fail CRC or have no envelope are dropped; nothing is forwarded transparently. A future frame format the server cannot parse (e.g. an Aether extension that isn't DDT2) is silently discarded by legacy ratflectors.

## 4. Behaviours that matter for robustness

| # | Observation | Consequence |
|---|---|---|
| R1 | No dedup, no hop count, no frame id | Any loop (two ratflectors linked both ways, or an RF bridge hearing another bridge's output) amplifies or recirculates frames until a link drops |
| R2 | Split horizon only ("not back to the source connection") | Prevents only 2-node echo. Triangles loop. |
| R3 | Learned routes are "last connection heard" | A station reachable via two bridges flaps between them. Unicast goes to whichever heard it last. |
| R4 | No application keepalive | Half-open TCP (NAT timeout, Wi-Fi roam) is only detected when a write fails. A client can sit "connected" while receiving nothing for hours. |
| R5 | Client reconnect: fixed 5 s retry, forever (`transport.py:464-477`) | No back-off, no jitter. After a server restart, all clients reconnect in lock-step. |
| R6 | Per-connection `Transporter` threads poll with 0.25 s timeouts | O(n) threads. CPU-bound polling is noted in the code (`transport.py:230-232`). |
| R7 | One global lock around `__repeat` | A slow socket write (full TCP buffer) blocks forwarding for everyone |
| R8 | Unbounded outbound queues per connection | A stalled client grows memory without limit |
| R9 | No identity binding | `s_station` in frames is not tied to the authenticated USER. Spoofing is trivial. |
| R10 | Stateful sessions run end-to-end *through* the ratflector | ACK timing assumes one path. TCP's reliability hides loss on the Internet leg but not the RF leg of a bridged path. |

### 4.1 RF↔Internet bridging: TCP and RF delivery guarantees differ

When a ratflector owns a radio serial path (`timeout = 3` warm-up, `d-rats_repeater.py:616`), frames from the Internet are queued to the radio and transmitted at slow-data speed. With multiple Internet clients the RF side can receive far more than ~119 B/s, and the queue grows without bound (R8). The stateful ARQ between an Internet client and an RF station then measures a mixed path. TCP delivers to the bridge instantly, the RF side drains slowly, and the sender's timeout (computed from the measured rate) is wrong in both directions.

ircDDBGateway's `DRATSServer` (`Common/DRATSServer.cpp`) is a second, independent RF↔TCP bridge inside the D-STAR gateway itself. It extracts slow-data blocks of type `0x3X` from repeater traffic, reassembles bytes until it sees `[EOB]`, and writes them to TCP. In the other direction it builds DV frames with silent AMBE and `0x3X` slow-data blocks. **It handles only slow-data type `0x3X`; Fast Data blocks (`0x8X/0x9X`) are not bridged.** Any repeater-gateway D-RATS service is therefore slow-data only.

## 5. Requirements for the Aether ratflector server (`aetherdv-rfd`)

### 5.1 Compatibility mode (must)
- Speak the exact §2 handshake, including the no-banner "old-school" client path, and a **configurable** banner: `100` (open), `101` (auth), or none.
- Forward legacy DDT2 frames with **identical forwarding semantics** to legacy clients: unicast-if-known, else flood. Re-encoding must be byte-faithful; ideally forward the original bytes rather than re-serialising, to avoid P1-class bugs.
- Accept a legacy ratflector as an upstream/peer link.

### 5.2 Safety additions that remain legacy-compatible (must)
- **Cross-link duplicate suppression**: key = hash of the exact DDT2 bytes, remembered together with the link it first arrived on, for a short window (~3 s, tunable). Drop a copy only if it arrives on a *different* link within that window, which is the signature of a loop or multipath bridge. Repeats on the *same* link are always forwarded. Legacy frames carry no unique id: a user typing "ok" twice produces identical bytes, and stateful REQACK re-polls are byte-identical by design (`stateful.py:280-285`). Plain content-hash dedup would therefore delete legitimate traffic. **This rule must be validated in the loop/bridge simulation suite before release.** Aether-framed links use the frame uuid instead (§5.3) and need no heuristic.
  **Phase B simulation result** (`crates/aether-rfd/tests/loops.rs`, Simulated):
  - A triangle of legacy ratflectors turns one chat line into 1,000 copies at one client within 60 s. With the rule, the client gets exactly one.
  - An asymmetric RF bridge loop (bridge B hears A on air, A doesn't hear B) gives 58 copies in 120 s on legacy servers. The rule suppresses it **only if the window is longer than the loop**: a 5 s RF hop with a 3 s window still gives 24 copies, while a 30 s window gives 1.
  - **Correction to the rule: the window is per link.** Internet-only links use ~3 s. **Links that include an RF hop use ≥30 s**, or the worst-case air time of a loop, whichever is larger. The cost is that a genuinely repeated identical frame arriving over a *different* link within 30 s is dropped; same-link repeats still pass.
- **Per-link token-bucket rate limits**, with special limits on RF-bridge links derived from the RF rate.
- **Bounded per-connection queues** with priority (control/ACK > chat > form > file) and drop-oldest-bulk policy.
  **Phase C finding** (`crates/aether-rfd/tests/server.rs`): a frame-count queue smaller than the allowed input burst drops frames for *healthy* clients during a burst. Implemented rules:
  - each client's input is rate-limited by a token bucket (default 64 KiB/s, 256 KiB burst), so TCP flow control pushes back on a flooding sender;
  - each client's output queue is bounded in **bytes** (default 1 MiB), larger than the burst.
  
  Result: with one client never reading, the healthy client received 6.2 MB of 6.2 MB, and only the stalled client lost frames.
- **Application keepalive** for Aether clients only (§5.3). TCP keepalive (SO_KEEPALIVE plus tuned intervals) for all.
- **Peer-link loop guard**: Aether-to-Aether peer links carry an envelope with origin-server id and hop count (§5.3). Frames re-entering their origin server are dropped. Hop limit default 4.
- Malformed input: maximum line length in the handshake, maximum frame size (e.g. 8 KB pre-yEnc), maximum buffer without `[EOB]` (drop and resync), per-IP connection limits, and a handshake timeout.

### 5.3 Aether extensions (optional, negotiated, never sent to legacy peers)
- After the auth exchange, an Aether client sends a DDT2 frame on a **reserved session id** (proposal: control-type 0 "PNG" on session 0 addressed to the server pseudo-callsign `RFLECT`). Legacy servers forward or ignore it harmlessly; legacy clients ignore it because the control session drops frames not addressed to them (`control.py`). An Aether server answers with a capability record (protocol version, features: keepalive, dedup, msg-receipts, peer-envelope). **The harmlessness of this probe must be proven with a legacy-client interop test before adoption.**
- If both ends agree, they may switch to an **Aether framing** on that TCP connection: length-prefixed frames with an extension header (origin id, hop count, frame uuid, priority) wrapping unmodified DDT2 bytes. Re-wrapping on egress to legacy links is lossless because the inner DDT2 frame is untouched.
- Server-authoritative "who is here" lists, per-station last-seen and path (RF via bridge X, or Internet).
- TLS (Internet leg only, where legal and desired). This is opt-in, and a plain port 9000 is always kept.

### 5.4 Operations (must)
- Headless daemon. A single static binary for Linux x86-64/arm64/armv7 (Raspberry Pi), plus a systemd unit and an optional OCI image.
- Config file (TOML) with hot reload of users and limits. `SIGTERM` triggers a graceful drain.
- Structured logs (JSON lines) and a counters endpoint (Prometheus text, local-only by default).
- Admin CLI over a local Unix socket (list clients, kick, ban, stats). No web admin in v1.
- Persistence: only the ban list and a counters snapshot. The server is not a mailbox in v1. Store-and-forward is a client/gateway function; see the architecture docs.

## 6. Client requirements (Aether app)

- Saved servers seeded from `ratflectors.yml` (fetched from GitHub when online, with a bundled snapshot offline), plus favourites.
- A per-connection state machine: `Idle → Resolving → Connecting → Handshake(banner|old-school) → Authenticating → Up → Draining → Backoff`. Exponential back-off with jitter (1 s → 5 min cap). The old-school probe timeout is shortened to ~5 s, with the legacy 30 s available as an option.
- Path labelling: every message and station in the UI is tagged with **where it was heard**: RF-slow, RF-fast, or Internet via ratflector name. A message bridged RF→Internet by someone else's bridge shows as Internet, since we cannot see upstream.
- Multiple simultaneous ratflectors, with a dedup cache identical to the server's, so the same frame arriving over two paths is not delivered twice.
- **The delivery state shown for an Internet send is "Handed to ratflector"** (TCP acked). It is never "Delivered" unless an end-to-end ACK (ARQ or message receipt) arrives.
