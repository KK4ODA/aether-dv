# Phase C — Minimal interoperable prototype: status and first hardware test

Branch: `phase-c` (built on `phase-b`). Validation level of everything here: **Simulated** unless a row says otherwise.

## What runs today

| Program | What it does |
|---|---|
| `aetherd --config FILE` | Engine. Serial radio ports (half-duplex scheduler) and ratflector ports (legacy handshake, back-off, TCP keepalive). Legacy D-RATS behaviour on chat (session 1) and RPC (session 2): chat, ping/echo replies (broadcast replies rate-limited), status, `RPCGetVersion` with the Aether capability token. Durable SQLite log. Legacy file and form transfer (control NEW/ACK/END, offer and `OK`, ARQ-1) in both directions; received files go to `files_dir` and forms to `files_dir/forms`. Local JSON-RPC/WebSocket API with a token. |
| `aether-cli` | Terminal client: `ports`, `stations`, `send PORT DST TEXT`, `history`, `ping`, `version`, `send-file PORT CALL PATH`, `transfers [all]`, `cancel ID`, `tail`. |
| `aether-rfd` | Ratflector server, legacy-compatible, with loop safeguards and per-client limits. |
| `serial-spike` | Receive-only port lister and hex monitor, for hardware test HW-0/HW-1. |

**Not in this slice yet:**
- composing ICS-213 messages (incoming forms are saved as D-RATS form XML; sending a form works at the engine level but there is no `messages.*` API or form editor yet)
- resuming an interrupted transfer (we always answer `OK`, never `RESUME:`; see below)
- position parsing (GPS sentences are logged as `raw` chat)
- CI-V Fast Data control
- the desktop UI

## Phase C acceptance criteria: current status

| ID | Criterion | Status |
|---|---|---|
| C1 | Interop with unmodified D-RATS 0.4.2 (chat, ping, status, ICS-213, 10 KB file; PTY and ratflector) | **Ready to test; not yet run against D-RATS** (this environment can't run it). Chat, ping, version and file/form transfer are implemented to the D-RATS 0.4.x wire behaviour. Simulated evidence: a 10 KB file and a form between two engines through `aether-rfd`, and a scripted D-RATS-shaped sender (compressed frames, 512-byte blocks, END) received byte-exact. ICS-213 *composition* is still missing. |
| C2 | Ratflector reconnect with back-off; half-open detection; no loss of queued items | **Partial (Simulated)**: back-off with jitter, keepalive (60 s / 15 s × 4), queued chat sent after reconnect, honest failure states. Not yet tested against real NAT timeouts or public ratflectors. |
| C3 | HW-0…HW-7 on the IC-9700 and ID-52 PLUS in slow mode | **Needs you**: procedure below |
| C4 | Over the air with D-RATS | **Needs you** |
| C5 | Wire capture and replay; support bundle | Not started |
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
