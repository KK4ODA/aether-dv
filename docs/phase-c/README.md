# Phase C — Minimal interoperable prototype: status and first hardware test

Branch: `phase-c` (built on `phase-b`). Validation level of everything here: **Simulated** unless a row says otherwise.

## What runs today

| Program | What it does |
|---|---|
| `aetherd --config FILE` | Engine. Serial radio ports (half-duplex scheduler) and ratflector ports (legacy handshake, back-off, TCP keepalive). Legacy D-RATS behaviour on chat (session 1) and RPC (session 2): chat, ping/echo replies (broadcast replies rate-limited), status, `RPCGetVersion` with the Aether capability token. Durable SQLite log. Legacy file and form transfer (control NEW/ACK/END, offer and `OK`, ARQ-1) in both directions; received files go to `files_dir`. ICS-213 messages: compose, send directly to a station, receive into the inbox (D-RATS form XML, stored in the database), with the delivery states of SM-7. Position reports (NMEA style and DPRS, from chat lines and from the radio's own GPS-TX output) are parsed, checksum-checked and stored; our own position is sent only on request. Local JSON-RPC/WebSocket API with a token. |
| `aether-cli` | Terminal client: `ports`, `stations`, `send PORT DST TEXT`, `history`, `ping`, `version`, `send-file PORT CALL PATH`, `transfers [all]`, `cancel ID`, `ics213 CALL SUBJECT MESSAGE [field=value…]`, `msg-send ID [PORT]`, `messages [FOLDER]`, `msg ID`, `positions [CALL]`, `send-position PORT [dprs|nmea]`, `capture-start PORT`, `capture-stop PORT`, `log [N]`, `bundle [captures]`, `tail`. |
| `aether-rfd` | Ratflector server, legacy-compatible, with loop safeguards and per-client limits. |
| `aecap` | Reads a wire capture: `aecap show FILE [--hex]` decodes every frame and stray byte run in time order; `aecap stats FILE` counts them. |
| `serial-spike` | Receive-only port lister and hex monitor, for hardware test HW-0/HW-1. |

**Not in this slice yet:**
- store-and-forward routing (messages go directly to the destination station; a message addressed to someone else is kept in our inbox, not forwarded) and automatic retry (use `msg-send` again on a failed message)
- forms other than ICS-213, and attachments

Layering note: message delivery lives in `aetherd` (`engine/messages.rs`) for this slice. It moves into the `aether-msg` crate of the architecture (05 §3) when store-and-forward routing is built; `aether-forms` is already its own crate.
- resuming an interrupted transfer (we always answer `OK`, never `RESUME:`; see below)
- CI-V Fast Data control
- the desktop UI

## Phase C acceptance criteria: current status

| ID | Criterion | Status |
|---|---|---|
| C1 | Interop with unmodified D-RATS 0.4.2 (chat, ping, status, ICS-213, 10 KB file; PTY and ratflector) | **Ready to test; not yet run against D-RATS** (this environment can't run it). Chat, ping, version and file/form transfer are implemented to the D-RATS 0.4.x wire behaviour. Simulated evidence: a 10 KB file and a form between two engines through `aether-rfd`, and a scripted D-RATS-shaped sender (compressed frames, 512-byte blocks, END) received byte-exact. ICS-213 is composed, sent and received between two engines (and through the real binaries); the XML follows `formgui.py` (double escaping, path, message id) but has not yet been opened by D-RATS. |
| C2 | Ratflector reconnect with back-off; half-open detection; no loss of queued items | **Partial (Simulated)**: back-off with jitter, keepalive (60 s / 15 s × 4), queued chat sent after reconnect, honest failure states. Not yet tested against real NAT timeouts or public ratflectors. |
| C3 | HW-0…HW-7 on the IC-9700 and ID-52 PLUS in slow mode | **Needs you**: procedure below |
| C4 | Over the air with D-RATS | **Needs you** |
| C5 | Wire capture and replay; support bundle | **Done (Simulated)**: `.aecap` capture per port (config `capture = true`, or `capture-start`/`capture-stop`), decoded by `aecap show`/`stats` through the same deframer the engine uses; the e2e test replays a capture and gets the exact frames back. Support bundle `.tar` (about, log, transfers, message states, optional captures; no message content, passwords or token). |
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
./target/release/aether-cli --token-file aetherd.token send-file IC-9700 W4DRS ./sitrep.txt
./target/release/aether-cli --token-file aetherd.token transfers all
```

From D-RATS, send a file or a form to your callsign; it appears in `received/` (or `received/forms/`) next to the database, and in `aether-cli tail`.

A finding from building this: the stale-ACK guard in ARQ-1 (P20) originally ignored any ACK that arrived before the *estimated* end of our transmission. On a path faster than the estimate (a ratflector, or a radio faster than 119 B/s) that stalled every window until the RTO. The guard now uses half the estimated air time: stale and duplicated ACKs, which arrive right after a burst starts, are still caught (test `faster_than_estimated_path_is_not_stalled`). HW-4 timing measurements will tell us how good the estimate is.

## Positions

Received position reports appear in `aether-cli positions` (latest per station) and `positions CALL` (history), and as `station.position` events. Both D-RATS formats are understood:

- NMEA style (`$GPGGA…` plus a `CALL,comment` line, optionally with a DPRS symbol and checksum in the comment)
- DPRS (`$$CRC…,CALL>APRATS,DSTAR*:…`), including what an Icom radio emits on its data port for a received GPS-TX beacon

Every checksum is verified. A rejected report is logged with the reason (`aether-cli log`), never silently dropped.

To send your own position, add a `[position]` section (see `examples/ic9700.toml`) and run `aether-cli send-position IC-9700` (DPRS) or `send-position IC-9700 nmea`. Nothing is sent automatically. D-RATS's radio note still applies: keep the radio's own GPS-TX off while using data, because it competes for the same slow-data capacity.

## ICS-213 messages

```bash
aether-cli ics213 W4DRS "Need cots" "Shelter A needs 40 cots" incident="Flood 26" to_pos="Logistics"
aether-cli messages drafts            # note the id
aether-cli msg-send <id>              # or: msg-send <id> IC-9700
aether-cli msg <id>                   # fields and delivery events
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

To check during C1: open an Aether ICS-213 in D-RATS (form view and the printable HTML), and send one from D-RATS to Aether; then compare `aether-cli msg <id>` with what D-RATS shows. Our test fixture for "what D-RATS writes" is reconstructed from the source, not captured, so this is a real check.

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
   - a support bundle: `aether-cli --token-file aetherd.token bundle captures` (with `capture = true` in your config, so the RF traffic is in it)
   
   These results move C3 and C4 forward, and they are the first numbers for ModeProfile timing (HW-4).

Safety notes:
- `aetherd` transmits only when you send something or answer a ping/version request addressed to you. Broadcast pings are answered at most once a minute per port.
- The radio's own MYCALL provides station identification on air.
