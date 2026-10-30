# Phase C — Minimal interoperable prototype: status and first hardware test

Branch: `phase-c` (built on `phase-b`). Validation level of everything here: **Simulated** unless a row says otherwise.

## What runs today

| Program | What it does |
|---|---|
| `aetherdv --config FILE` | Engine. Serial radio ports (half-duplex scheduler) and ratflector ports (legacy handshake, back-off, TCP keepalive). Legacy D-RATS behaviour on chat (session 1) and RPC (session 2): chat, ping/echo replies (broadcast replies rate-limited), status, `RPCGetVersion` with the Aether capability token. Durable SQLite log. Legacy file and form transfer (control NEW/ACK/END, offer and `OK`, ARQ-1) in both directions; received files go to `files_dir`. ICS-213 messages: compose, send directly to a station, receive into the inbox (D-RATS form XML, stored in the database), with the delivery states of SM-7. Position reports (NMEA style and DPRS, from chat lines and from the radio's own GPS-TX output) are parsed, checksum-checked and stored; our own position is sent only on request. Local JSON-RPC/WebSocket API with a token. |
| `aetherdv-cli` | Terminal client: `ports`, `stations`, `send PORT DST TEXT`, `history`, `ping`, `version`, `send-file PORT CALL PATH`, `transfers [all]`, `cancel ID`, `ics213 CALL SUBJECT MESSAGE [field=value…]`, `msg-send ID [PORT]`, `messages [FOLDER]`, `msg ID`, `positions [CALL]`, `send-position PORT [dprs|nmea]`, `radio-mode PORT [fast|slow --confirm]`, `capture-start PORT`, `capture-stop PORT`, `log [N]`, `bundle [captures]`, `tail`. |
| Desktop app (`app/`) | Minimal UI on the engine API: Chat (launch screen), Messages (ICS-213 view, compose, delivery ladder), Transfers, Stations, Diagnostics. See `app/README.md`. |
| `aetherdv-rfd` | Ratflector server, legacy-compatible, with loop safeguards and per-client limits. |
| `aetherdv-bench` | Scripted interop/performance runs against one peer through a running `aetherdv`: ping, version, 1 KB / 10 KB / 50 KB file, ICS-213, N rounds; Markdown summary plus a JSON report. Every test is one an unattended D-RATS station answers by itself. |
| `aetherdv-cap` | Reads a wire capture: `aetherdv-cap show FILE [--hex]` decodes every frame and stray byte run in time order; `aetherdv-cap stats FILE` counts them. |
| `serial-spike` | Receive-only port lister and hex monitor, for hardware test HW-0/HW-1. |

**Not in this slice yet:**
- store-and-forward routing (messages go directly to the destination station; a message addressed to someone else is kept in our inbox, not forwarded) and automatic retry (use `msg-send` again on a failed message)
- forms other than ICS-213, and attachments

Layering note: message delivery lives in `aetherdv` (`engine/messages.rs`) for this slice. It moves into the `aether-msg` crate of the architecture (05 §3) when store-and-forward routing is built; `aether-forms` is already its own crate.
- resuming an interrupted transfer (we always answer `OK`, never `RESUME:`; see below)
- the full desktop UI (map, command palette, file picker, settings); the minimal UI covers chat, messages, transfers, stations and diagnostics

## Phase C acceptance criteria: current status

| ID | Criterion | Status |
|---|---|---|
| C1 | Interop with unmodified D-RATS 0.4.2 (chat, ping, status, ICS-213, 10 KB file; PTY and ratflector) | **Ready to test; not yet run against D-RATS** (this environment can't run it). Chat, ping, version and file/form transfer are implemented to the D-RATS 0.4.x wire behaviour. Simulated evidence: a 10 KB file and a form between two engines through `aetherdv-rfd`, and a scripted D-RATS-shaped sender (compressed frames, 512-byte blocks, END) received byte-exact. ICS-213 is composed, sent and received between two engines (and through the real binaries); the XML follows `formgui.py` (double escaping, path, message id) but has not yet been opened by D-RATS. |
| C2 | Ratflector reconnect with back-off; half-open detection; no loss of queued items | **Partial (Simulated)**: back-off with jitter, keepalive (60 s / 15 s × 4), queued chat sent after reconnect, honest failure states. Not yet tested against real NAT timeouts or public ratflectors. |
| C3 | HW-0…HW-7 on the IC-9700 and ID-52 PLUS in slow mode | **Needs you**: procedure below |
| C4 | Over the air with D-RATS | **Needs you** |
| C5 | Wire capture and replay; support bundle | **Done (Simulated)**: `.aecap` capture per port (config `capture = true`, or `capture-start`/`capture-stop`), decoded by `aetherdv-cap show`/`stats` through the same deframer the engine uses; the e2e test replays a capture and gets the exact frames back. Support bundle `.tar` (about, log, transfers, message states, optional captures; no message content, passwords or token). |
| C6 | Baseline measurements (D-RATS vs Aether) | Not started; needs C3 |

## Legacy file and form transfer (C1)

How it behaves, and where it deliberately differs from D-RATS (all differences are invisible to a D-RATS peer):

| | D-RATS 0.4.x | Aether |
|---|---|---|
| Accepting a transfer | Every offer is accepted into the download directory | Accepted up to `max_incoming_kib` (default 4096 KiB of compressed stream, 32 MiB inflated); 8 incoming at a time. A larger offer is refused with END, which D-RATS reports as an interrupted transfer. |
| File name | Joined onto the download directory | Reduced to a portable base name (no separators, control characters, Windows-reserved names or leading dots); an existing file is never overwritten (`report (1).txt`). |
| Resume | `RESUME:<n>` if a `.part` file of the same name exists (P8: no size or hash check) | Always `OK` (restart). When D-RATS asks *us* to resume, we honour its offset. |
| Completion check | Decompress; on failure keep a `.part` | The stream length must match the offer and it must inflate within the limit; otherwise the transfer fails and nothing is saved. |
| "Done" for a send | Every block written | Every block **acknowledged** by the peer, shown as "handed off", never "delivered" (I4.2). Transfers over 256 blocks carry a warning (P19). |
| Block size / window | 512 B (files) or 1024 B (forms), window up to 4 KiB in flight | 256 B on DV slow data, 512 B on Fast Data, 1024 B on ratflectors; window 4. All within the legacy limits. |
| Retries | RTO from serial write time, 12 s floor | RTO measured per transfer (RFC 6298, Karn); starts at 12 s on RF so a lost offer is re-polled before D-RATS gives up waiting for it (~20 s). |

To try it on a ratflector or over RF once chat works:

```bash
./target/release/aetherdv-cli --token-file aetherdv.token send-file IC-9700 W4DRS ./sitrep.txt
./target/release/aetherdv-cli --token-file aetherdv.token transfers all
```

From D-RATS, send a file or a form to your callsign; it appears in `received/` (or `received/forms/`) next to the database, and in `aetherdv-cli tail`.

A finding from building this: the stale-ACK guard in ARQ-1 (P20) originally ignored any ACK that arrived before the *estimated* end of our transmission. On a path faster than the estimate (a ratflector, or a radio faster than 119 B/s) that stalled every window until the RTO. The guard now uses half the estimated air time: stale and duplicated ACKs, which arrive right after a burst starts, are still caught (test `faster_than_estimated_path_is_not_stalled`). HW-4 timing measurements will tell us how good the estimate is.

## Scripted interop run (C1, C6)

D-RATS answers pings and version requests and accepts files and forms automatically, so most of C1 can run unattended against a D-RATS station:

```bash
./target/release/aetherdv-bench --token-file aetherdv.token --port net --peer W4DRS --rounds 50 --out c1-ratflector.json
./target/release/aetherdv-bench --token-file aetherdv.token --port IC-9700 --peer W4DRS --rounds 10 --gap 5 --out c1-rf-slow.json
```

It prints a table (success %, median and p90 time, goodput) and exits non-zero if anything failed. For C1, the criterion is 100% of 50 exchanges per test. The JSON report plus a support bundle (`aetherdv-cli bundle captures`) is the evidence. Exchanges D-RATS can't start on its own still need a person at the D-RATS station: D-RATS→Aether chat, form and file, and the status message.

For C6 (baseline), run the same suite with D-RATS on both ends using its defaults, timed by hand or from its log, then with Aether on one end. Use the same radios, path and settings.

## Radio Fast Data setting over CI-V (Phase D groundwork)

If a serial port has a `[port.civ]` section (see the examples), Aether:

- reads the radio's DV Fast Data setting at start-up and warns in the log if it doesn't match the port's `mode`. This is the classic misconfiguration where nobody can decode anybody.
- reads it on request: `aetherdv-cli radio-mode IC-9700`.
- changes it only on request, with `allow_set = true` in the config, `--confirm`, no transfer running on the port, and a read-back to verify: `aetherdv-cli radio-mode IC-9700 fast --confirm`. Nothing switches automatically (ADR-0005).

The commands are the documented ones (IC-9700 `1A 05 00 77`, ID-52/ID-52 PLUS `22 03`). They are tested against a simulated radio only, so their **validation level is Simulated** until HW-8 is run on your radios. The IC-9700's default CI-V address (A2h) is from Icom's guide. The ID-52 PLUS default (B4h) comes from a secondary source, and Aether says so in the log; set `addr` if it doesn't answer. After switching the radio, the port's `mode` (timers and burst sizing) still has to match: edit the config and restart for now.

## Through a repeater

Add a `[port.route]` section to a radio port (see the examples and `docs/10-repeater-operation.md`):

- **Choose the scope.** `scope = "repeater"` (the default) keeps traffic on the repeater: R2 must be blank. `"reflector"` sends to whatever the repeater is linked to: R2 is the gateway, and you confirm it once per session.
- **The route check.**
  - With `[port.civ]`, Aether reads the radio's UR/R1/R2 and holds sending when they don't match the scope. It always blocks a link, unlink or echo command left in UR with R2 on the gateway.
  - Without CI-V, it asks you to confirm the values (`aetherdv-cli route IC-9700 confirm`, or the port plate in the app).
- **Courtesy.**
  - It waits for voice to end, then a gap, before sending. This needs CI-V; without it, voice is inferred only from relayed D-PRS.
  - It leaves room between its own transmissions and keeps to an air-time budget.
  - `aetherdv-cli pause IC-9700 30` pauses sending for a net.
- **Access check.** `aetherdv-cli repeater-check IC-9700` reports the repeater's acknowledgement, BER and link (CI-V needed).

Validation level: **Simulated** (simulated IC-9700 and a simulated repeater). The bench and on-air tests in `docs/10` §7 come next.

## Positions

Received position reports appear in `aetherdv-cli positions` (latest per station) and `positions CALL` (history), and as `station.position` events. Both D-RATS formats are understood:

- NMEA style (`$GPGGA…` plus a `CALL,comment` line, optionally with a DPRS symbol and checksum in the comment)
- DPRS (`$$CRC…,CALL>APRATS,DSTAR*:…`), including what an Icom radio emits on its data port for a received GPS-TX beacon

Every checksum is verified. A rejected report is logged with the reason (`aetherdv-cli log`), never silently dropped.

To send your own position, add a `[position]` section (see `examples/ic9700.toml`) and run `aetherdv-cli send-position IC-9700` (DPRS) or `send-position IC-9700 nmea`. Nothing is sent automatically. D-RATS's radio note still applies: keep the radio's own GPS-TX off while using data, because it competes for the same slow-data capacity.

## ICS-213 messages

```bash
aetherdv-cli ics213 W4DRS "Need cots" "Shelter A needs 40 cots" incident="Flood 26" to_pos="Logistics"
aetherdv-cli messages drafts            # note the id
aetherdv-cli msg-send <id>              # or: msg-send <id> IC-9700
aetherdv-cli msg <id>                   # fields and delivery events
```

What the states mean:

| State | Meaning |
|---|---|
| `draft` | Being written; editable |
| `queued` → `sending` | A form transfer to the station is open |
| `handed_off` | Every block was acknowledged by the destination station. With D-RATS this is the best available evidence: it sends no delivery receipt, so Aether never shows `delivered` for a D-RATS peer (the database refuses it without end-to-end evidence). |
| `failed` | The station didn't accept or stopped acknowledging; the reason is shown. Retry with `msg-send`. |
| `received` | Arrived in our inbox. Our callsign is appended to its path, as D-RATS does; a second copy (same message id) is not stored twice. |

Field names are the D-RATS ICS-213 ids: `incident`, `date`, `time`, `recip`, `to_pos`, `sender`, `from_pos`, `subject`, `message`, `reply`, `sig`, `date_reply`, `time_reply`. Date and time are filled in on creation in UTC, in the D-RATS formats (`29-Oct-2026`, `4:31:23`). D-RATS fills local time by default, so a D-RATS user may see a different hour unless they also use UTC.

To check during C1: open an Aether ICS-213 in D-RATS (form view and the printable HTML), and send one from D-RATS to Aether; then compare `aetherdv-cli msg <id>` with what D-RATS shows. Our test fixture for "what D-RATS writes" is reconstructed from the source, not captured, so this is a real check.

## First hardware test (HW-0, HW-1, then a first chat)

You need: the IC-9700 (or ID-52 PLUS) on USB, and a second D-STAR station within simplex range running **D-RATS 0.4.2**. The ID-52 PLUS with D-RATS on another computer works. Use a simplex DV frequency agreed for testing, at low power.

1. **Build** (once):
   ```bash
   cargo build --release -p aetherdv -p serial-spike
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
   ./target/release/aetherdv --config my.toml
   ```
   In a second terminal:
   ```bash
   ./target/release/aetherdv-cli --token-file aetherdv.token tail
   ```
   In a third terminal:
   ```bash
   ./target/release/aetherdv-cli --token-file aetherdv.token send IC-9700 CQCQCQ test from Aether DV
   ```
   Then ping the D-RATS station from it, and from D-RATS ping `KK4ODA`.
5. **Report back**:
   - what D-RATS showed
   - `aetherdv-cli history 50`
   - the `listen` capture from step 3
   - a support bundle: `aetherdv-cli --token-file aetherdv.token bundle captures` (with `capture = true` in your config, so the RF traffic is in it)
   
   These results move C3 and C4 forward, and they are the first numbers for ModeProfile timing (HW-4).

Safety notes:
- `aetherdv` transmits only when you send something or answer a ping/version request addressed to you. Broadcast pings are answered at most once a minute per port.
- The radio's own MYCALL provides station identification on air.
