# 02 — Legacy D-RATS Wire Protocol (DDT2) Analysis

Status: Phase A research. Evidence level: **source-reading**, plus byte vectors produced by an independent re-implementation (`tools/model_ddt2.py`). None of this has yet been checked against a live D-RATS instance (see §9).

Source baseline: `ham-radio-software/D-Rats` at commit `d0e7a39` (2026-08-15). All `file:line` references are to that tree unless another repository is named.

---

## 1. Layering as implemented

```
 ┌───────────────────────────────────────────────────────────────────────┐
 │ Applications: chat, ping, status, GPS, RPC jobs, file/form transfer, │
 │               message router (store & forward of XML forms)          │
 ├───────────────────────────────────────────────────────────────────────┤
 │ Sessions (sessions/*.py)                                             │
 │   id 0  ControlSession  – open/close stateful sessions               │
 │   id 1  ChatSession     – stateless, broadcast/unicast               │
 │   id 2  RPCSession      – stateless request/response                 │
 │   id 3  SniffSession    – local only                                 │
 │   id ≥4 Stateful / File / Form / Socket sessions (windowed ARQ)      │
 ├───────────────────────────────────────────────────────────────────────┤
 │ SessionManager (sessionmgr.py) – demux by DDT2.session, addr filter  │
 ├───────────────────────────────────────────────────────────────────────┤
 │ Transporter (transport.py) – [SOB]…[EOB] framing, warm-up, pre-TX     │
 │   random delay, GPS sentence sniffing, raw/"compat" text passthrough │
 ├───────────────────────────────────────────────────────────────────────┤
 │ DataPath (comm.py) – Serial (8-byte paced writes, soft XON/XOFF),     │
 │   Socket (ratflector, auth), TNC/KISS, AX.25, AGWPE, "dongle"         │
 └───────────────────────────────────────────────────────────────────────┘
                      ▼ byte stream into the radio's DV-data port ▼
           The radio packetizes bytes into D-STAR slow-data blocks
```

The key architectural fact: **D-RATS never sees D-STAR frames.** It writes an opaque byte stream to the radio's serial "DV data" port. The radio decides how those bytes are split into slow-data blocks, when to key up, and when to unkey. Everything below the serial port is invisible to D-RATS. That includes the RF header, superframe sync, slow-data mini-headers, and Fast Data framing. D-RATS has no feedback from that layer.

---

## 2. DDT2 frame

### 2.1 Header (`ddt2.py:64`, struct `"!BHBBHH8s8s"`, 25 bytes, network byte order)

| Off | Size | Field | Notes |
|---|---|---|---|
| 0 | 1 | magic | `0xDD` = payload zlib-compressed; `0x22` (= `~0xDD`) = uncompressed (`ddt2.py:135,203-207`) |
| 1 | 2 | seq | 16-bit field, but stateful sessions only use 0–255 (`stateful.py:353,609`) |
| 3 | 1 | session | Session id, 0–255 |
| 4 | 1 | type | Meaning depends on session (§4) |
| 5 | 2 | checksum | CRC over header (with checksum = 0) + payload (§2.3) |
| 7 | 2 | length | Length of the *on-wire* (possibly compressed) payload |
| 9 | 8 | s_station | ASCII callsign, right-padded with `~` |
| 17 | 8 | d_station | ASCII callsign or `CQCQCQ`, right-padded with `~` |
| 25 | n | payload | zlib level 9 if magic `0xDD` |

Station fields allow 8 characters. Callsigns with SSIDs, or any text over 8 bytes, are silently truncated by `struct.pack` (`8s`). The receiver strips **all** `~` bytes (`ddt2.py:232`), not only the padding.

### 2.2 Envelope and escaping (`ddt2.py:29-30,305-347`; `yencode.py:38-78`)

On the wire every frame is `"[SOB]" + yenc(header+payload) + "[EOB]"`.

yEnc variant: any byte in `{0x00,0x11,0x13,0x1A,0x84,0xE7,0xFD,0xFE,0xFF,0xC0,0xDB,'='}` is sent as `'=' , (b+64) mod 256`. The comment block (`yencode.py:23-37`) says these are XON/XOFF, Icom "packet loss notification" bytes (`0x84,0xE7,0xFE`), Icom CI-V terminator `0xFD`, and KISS `FEND/FESC`. This set is the de-facto statement of which bytes radios do not pass transparently through the DV-data port. `0xE7` and `0x84` were added in 2025 (D-Rats#305, PR #302). Per **JARL STD6 §6.2**, the run `E7 84 76` in slow data adjacent to the silence AMBE pattern is interpreted by receiving radios as a *packet-loss marker*, and the data is discarded. Adding bytes to the banned set is decode-compatible with all older versions, because decoders do not consult the list. JARL §6.2 lists the reserved bytes for simple data as `00 11 13 76 84 E7 FE`. **D-RATS does not escape `0x76`** (the comment at `yencode.py:26` mentions it, but `DEFAULT_BANNED` omits it). Escaping `E7`/`84` already breaks the documented `E7 84 76` loss sequence. Whether a lone `0x76` matters is a hardware-test item (HW-2). The set is **undocumented by Icom/Kenwood and must be validated per radio and per mode (slow vs fast)**; see `03-dstar-rf-transport.md`.

Flow-control history: D-RATS 0.3.x and early 0.4.x discarded any non-XON/XOFF byte read while polling for flow control (D-Rats#253, #252). A switch to driver XON/XOFF ran in Linux IXANY mode and broke reception between 0.3.10 and 0.4.1 stations (#262). PR #268 returned to software XON/XOFF but keeps the bytes (`comm.py:417-425` in this tree). The lesson: **flow-control behaviour is part of the interoperability surface**, and Aether needs a per-radio, explicit flow-control setting that is covered by hardware tests.

Measured expansion on incompressible data: **+4.6 %** (`tools/model_ddt2.py`).

Receiver framing (`transport.py:246-280`): scan the input buffer for `[SOB]`…`[EOB]`. Bytes outside a pair are either discarded or, in "compat/raw" mode, delivered as plain text chat after `compat_delay` seconds of silence (`transport.py:448-454`). NMEA (`$GP…`) and DPRS (`$$CRC…`) sentences emitted by the radio itself are regex-extracted from the same buffer (`transport.py:282-333`).

Failure modes inherent in this framing:
- A lost `[EOB]` merges two frames. The first CRC fails and both frames are lost.
- The literal 5-byte markers `[SOB]`/`[EOB]` are not escaped, because `[` (0x5B) is not in the banned set. A payload (or header field) containing `[EOB]` truncates the frame. The CRC check catches it, but the same bytes are lost **deterministically on every retransmission** (a "poison block"). Compression makes this rare but not impossible. Uncompressed chat text containing the marker always fails.
- No length-based framing, so resynchronisation relies on text markers.

### 2.3 Checksum (`crc_checksum.py:20-70`)

A bitwise CRC-16, poly `0x1021`, init 0, message augmented with two zero bytes. This is **CRC-16/XMODEM**. Check value for `"123456789"` = `0x31C3`, confirmed with `tools/model_ddt2.py`. A 16-bit CRC over frames up to ~1.1 KB is adequate against random errors. It gives no end-to-end file integrity, but zlib's Adler-32 provides some (§5.4).

### 2.4 Reference vectors

**Confirmed:** `tools/model_ddt2.py` reproduces byte-for-byte the upstream D-RATS self-test vectors recorded in macrats (`Tests/MacRatsCoreTests/DDT2FrameTests.swift:77-78`). That covers both the uncompressed and the zlib-9 compressed form of `seq=3, session=2, type=1, FOO→BAR, "This is a test"`:

```
22 0003 02 01 b557 000e 464f4f7e7e7e7e7e 4241527e7e7e7e7e 5468697320697320612074657374
dd 0003 02 01 c3ab 0014 464f4f7e7e7e7e7e 4241527e7e7e7e7e 78da0bc9c82c5600a2448592d4e21200247304f6
```

Compressed frames are byte-identical only when the sender uses the same deflate implementation and level. macrats found Apple's deflate differs from zlib's. Receivers must therefore be tested by decode equivalence; the Aether sender should use real zlib level 9 so that byte-exact golden tests remain possible.

Further vectors from the same model, not yet confirmed against a live D-RATS:

```
chat "Hello" N0CALL→CQCQCQ, session 1, type 0, uncompressed
 raw  22 00 00 01 00 45 54 00 05 4e 30 43 41 4c 4c 7e 7e 43 51 43 51 43 51 7e 7e 48 65 6c 6c 6f   (30 B)
 wire [SOB]"=@=@\x01=@ET=@\x05N0CALL~~CQCQCQ~~Hello[EOB]                                        (44 B)

warm-up: session 0, type 254, "!"→"!", 16 × 0x01, uncompressed      raw 41 B → wire 56 B
T_REQACK [0,1,2,3], session 4, compressed                           raw 37 B → wire 53 B
```

Note: the 4-byte REQACK list becomes 12 bytes after zlib. **DDT2 compresses control frames even when that enlarges them.**

---

## 3. Transporter behaviour (`transport.py`)

| Behaviour | Code | Default | Notes |
|---|---|---|---|
| Pre-TX random delay | `transport.py:344-357` | `force_delay=-2` → uniform 0.5–2.0 s before each burst (`config_defaults.py:172`) | Crude collision avoidance. There is **no carrier sense**: the radio's busy state is not read. |
| Warm-up frame | `transport.py:359-371` | `warmup_timeout=0` → disabled by default since 0.3.6 (`config_defaults.py:170-171`) | When enabled, a type-254 frame from `!` is sent if idle > timeout, to cover radio key-up so the real frame isn't clipped. |
| Write | `transport.py:184-205` | — | `sent_event` is set when `pipe.write()` returns, i.e. when the OS/radio accepted the bytes, **not** when RF transmission finished. |
| Read loop | `transport.py:412-477` | 0.25 s serial timeout | Polling loop. Reconnect retries with linear sleep. |
| Outbound queue | `BlockQueue` | unbounded | No priority. Chat queues behind file blocks. |

### 3.1 Serial path pacing (`comm.py:295-512`)

`SWFSerial._write` writes **8 bytes at a time**, calls `flush()`, then polls for XON/XOFF bytes from the radio, with 10 ms sleeps and a 15 s XOFF limit (`comm.py:329,430-463`). Any non-XON/XOFF byte read during polling is pushed into a side buffer (`comm.py:417-425`). Hardware flow control and the driver's XON/XOFF are disabled (`comm.py:710-714`).

Consequences:
- The application's "transmit rate" estimate (`DDT2Frame.get_xmit_bps`, `stateful.update_xmt`) measures serial pacing, not RF rate.
- 8-byte writes plus a flush every 10+ ms cap serial throughput near 800 B/s, before OS latency. That is above slow-data rate (~119 B/s) but **only about 2× the Fast Data ceiling (~435 B/s)**. Under USB scheduling jitter it may become a bottleneck for Fast Data. This needs measurement.
- The code comments admit the XON/XOFF handling is "not optimal" and buggy (`comm.py:388-399`).

---

## 4. Sessions

### 4.1 Session ids are positional, not negotiated

`SessionManager.__init__` registers Control as id 0 (`sessionmgr.py`). `mainapp.start_comms` then starts `chat` (id 1) and `rpc` (id 2), then `Sniffer` (`mainapp.py:718,726`). Stateless sessions are matched on the receiver **by numeric id**, so interoperability requires chat = 1 and RPC = 2 exactly. `SessionManager.incoming` accepts session-1 frames whatever the destination callsign (`sessionmgr.py:181`) and drops frames whose source equals the local callsign as "looped" (`:190`).

Session-id allocation wraps after 255 with a buggy search loop: when `_sid_counter >= 255`, `ident` is assigned only inside the `for` loop and may be stale (`sessionmgr.py:261-268`).

### 4.2 Control session (id 0, `sessions/control.py`)

| type | name | payload |
|---|---|---|
| 0 | PNG | unused |
| 1 | END | ASCII decimal session id |
| 2 | ACK | `struct "BB"` (requester's id, responder's id) |
| 3+t | NEW | `B` requester id + UTF-8 session name. `t` = session class: 1 general(→4), 4 socket(→7), 5 file(→8), 6 form(→9) (`sniff.py:36`) |

Open: requester sends NEW up to 10 times, waiting 5 s and then 15 s for the state change (`control.py:236-260`). Close: END up to 3 times, 15 s each (`control.py:295-305`). A duplicate NEW from the same `(station, remote id)` re-ACKs the existing session. After a crash and restart, a peer can therefore be re-attached to a *stale* session that reused the same id.

### 4.3 Chat session (id 1, `sessions/chat.py`, stateless, **uncompressed**)

| type | meaning | payload |
|---|---|---|
| 0 | chat text **or** GPS/DPRS sentence (parsed by content) | UTF-8 |
| 1/2 | ping request / response | text, e.g. `Running D-RATS <ver> (<os>)` |
| 3/4 | echo request / response | arbitrary |
| 5 | station status | ASCII digit status code + message |

There are no message ids, no ACKs and no dedup. Broadcast pings make every station reply after a random 0–5 s delay (`chat.py`), which causes a reply storm on a busy channel.

### 4.4 RPC session (id 2, `sessions/rpc.py`, stateless)

Request type 0 has payload `JobClassName \x1D key\x1Fvalue\x1Ekey\x1Fvalue…`, with the job id carried in the DDT2 `seq`. Response type 1 carries an encoded dict result. Jobs: file list, form list, pull file, delete file, pull form, position report, get version, check mail (`rpc.py:54-62`). Timeout 30 s, **no retry** (`rpc.py:605,711-722`).

Security findings for the redesign:
- `rpc_file_pull` joins an unvalidated remote filename onto the download directory (`rpc.py:900`). Path traversal is possible when "allow remote files" is on. Delete checks only for `/`.
- Authorisation is by the claimed source callsign plus a cleartext password, so it is spoofable. On amateur RF, encryption is not permitted, but **authentication** (e.g. HMAC signatures) generally is. This needs a regulatory check per jurisdiction.

### 4.5 Stateful session — the reliability layer (`sessions/stateful.py`)

DDT2 types within a stateful session: `T_DAT=4`, `T_REQACK=5`, `T_ACK=1`, `T_NAK=2` (never sent).

Algorithm, as implemented:
1. `write()` chops data into `blocksize` chunks. The app default is 512 (`config_defaults.py:129`); the class default is 1024. Each chunk gets an 8-bit seq.
2. The sender keeps a window, `out_limit` (app default 4, `config_defaults.py:130`), adjusted by ±1 for each full or partial ACK (`__full_acks`), clamped to `[2, 4096/blocksize]` (`stateful.py:144-160`). **Hard cap: 4 KB in flight.**
3. It transmits every outstanding block, then a `T_REQACK` listing their seqs (1 byte each).
4. The receiver answers each REQACK with a `T_ACK` listing the seqs it holds. Missing seqs are implicit NAKs. Out-of-order blocks are buffered and released in order (`stateful.py:337-447`).
5. Sender timeout = `max(12 s, 1.5 × pending_bytes / rate)`. `rate` is the measured round-trip byte rate, or **80 B/s** before a measurement exists (`stateful.py:203-227`). If no ACK arrives, only the REQACK is resent, with back-off `4 + 4·attempt` s. After **10 attempts** the session closes (`stateful.py:268-285`).
6. If the session idles for **90 s** without events it closes itself (`stateful.py:57,498-503`).

This is a form of **selective repeat with a poll** ("REQACK"). The design is sound. The weaknesses are in parameters, measurement and coupling:
- The rate estimate mixes serial write time, radio buffering and RF time.
- The 12 s minimum timeout dominates short transfers.
- The 8-bit seq and 1-byte ACK entries are noted as FIXMEs (`stateful.py:233,360`).
- Data and REQACK go in one burst, so a lost REQACK costs a full timeout, not just a re-poll.
- The 4 KB cap is fixed and not per path.
- Duplicate detection relies on `recv_list` being reset only when seq 0 follows 255 (`stateful.py:397-401`).

### 4.6 File and form transfer (`sessions/file.py`, `sessions/form.py`)

1. Sender opens a stateful session of class FILE (control type 8) or FORM (type 9).
2. Offer: `struct "<I"` **compressed** length + UTF-8 base filename (`file.py:144`).
3. Receiver replies `OK` or `RESUME:<offset>` if a `.part` file exists (`file.py:262-275`).
4. Sender streams `zlib.compress(file, 9)` from that offset (`file.py:326`). Each DDT2 block is **compressed again** (magic 0xDD), which wastes ~11 bytes per block and CPU for no gain.
5. Completion is signalled by **session close**. The receiver treats "session closed" as end of file and decompresses (`file.py:286-305`).

Defects relevant to reliability:
- Resume identifies a partial file by **name only**: no size, hash or date check. A different file of the same name is silently spliced.
- End of transfer is inferred from session close, so a lost END while the receiver lacks the tail still produces a decompress attempt. zlib/Adler-32 catches it and the data is saved as `.part`.
- Each wait loop has its own constants (40 × 0.5 s for start, `write(timeout=120)`), so the state space is large and implicit.

Forms use the same mechanism. The payload is the form's XML document (§6).

---

## 5. Position reporting

GPS fixes travel as **chat type 0** text (session 1) in one of two formats:
- NMEA style: `$GPGGA,…*CS\r\n` + `%-8.8s,%-20.20s\r\n` (callsign, comment) (`gps.py:629-656`).
- DPRS/APRS style: `$$CRC%04X,CALL>APRATS,DSTAR*:/hhmmssh…\r\n` (`gps.py:702-766`).

The receive path also extracts **radio-generated** GPS sentences (the radio's own GPS-TX feature) directly from the serial stream (`transport.py:282-333`). D-RATS position exchange therefore overlaps with native D-STAR DPRS. Radio GPS-TX should be **off** while D-RATS data is used, because it competes for the same slow-data capacity.

---

## 6. Messages, forms and routing (`msgrouting.py`, `formgui.py`)

- A message *is* a form: an XML file (`<form id=…><title/>…<field id><caption/><entry type/>…</field><path><src/><dst/><mid/><e/>…</path></form>`). Bundled forms include ICS-213 and HICS-260 (`forms/`).
- Message id: `CALL.<hex(time − 1114880400)>.<hex(rand 0..100000)>` (`ui/main_messages.py:56-66`).
- Store-and-forward: the Outbox is a directory of XML files with `.lock.*` lock files. The router wakes every `msg_flush` = 30 s, groups messages by destination, and chooses a route:
  1. gratuitous source route `A;B;C`
  2. email (`@`)
  3. directly heard station (heard within `station_msg_ttl` 600 s)
  4. `routes.txt` static route
  5. default `*`
  6. `WL2K:`
  
  Loop prevention uses the `<path><e>` list (`msgrouting.py:134-330`).
- **Delivery confirmation is per hop only.** A message moves to Sent when the form-transfer session to the *next hop* completes. There is no end-to-end receipt.

---

## 7. Ratflector (summarised; full analysis in `04-ratflector-analysis.md`)

The ratflector carries the same `[SOB]…[EOB]` byte stream over TCP port 9000, with an optional text auth preamble (`100/101/102/200/500` codes). The server (`d-rats_repeater.py`) learns `callsign → last connection`, unicasts if the station was heard within 600 s, and otherwise floods to every other connection. There is no dedup, no TTL and no keepalive.

---

## 8. Defect and limitation register (protocol layer)

| # | Defect / limitation | Evidence | Impact | Fix possible while staying wire-compatible? |
|---|---|---|---|---|
| P1 | `get_packed()` on an uncompressed frame flips `magic` on every call | `ddt2.py:135` | Any code that packs an uncompressed frame twice (retransmit, copy) sends `0xDD` with raw data, so the receiver's zlib fails | Yes: never mutate state in serialisation |
| P2 | `[SOB]/[EOB]` not escaped | `ddt2.py:29-30` | Deterministic poison blocks | Sender-side: detect and re-chunk or toggle compression for that block |
| P3 | Control and ACK frames compressed even when that enlarges them | §2.4 | ~8–20 B wasted per ACK over | Yes: magic 0x22 is legal for every frame |
| P4 | Double compression of file data | `file.py:326` plus DDT2 | ~2 % and CPU | Partly: Aether can send file blocks with magic 0x22. The receiver still decompresses the file-level stream. |
| P5 | Rate and timeout estimation uses serial write time | `transport.py:376-381`, `stateful.py:203-227` | Spurious retransmits, or long stalls | Yes: timing is sender-local |
| P6 | 8-bit seq, 4 KB in-flight cap, window 2–8 | `stateful.py:154,353` | Limits Fast Data utilisation | Window and block size are sender-local (receiver accepts any). The seq space is not. |
| P7 | Random 0.5–2 s pre-TX delay, no carrier sense | `transport.py:344-357` | ~1.25 s mean dead air per over; collisions still happen | Yes (local), but channel etiquette matters on shared repeaters |
| P8 | Resume by filename only | `file.py:240-260` | Silent corruption risk (caught by Adler-32 later) | Partly |
| P9 | No message-level end-to-end ACK | §6 | Delivery uncertainty is invisible to the operator | Needs an extension. Legacy peers just ignore unknown chat types / RPC jobs. |
| P10 | 8-char station fields truncate | `ddt2.py:149-150` | SSID or long-call collisions | No; carry the full id inside the payload for Aether peers |
| P11 | RPC path traversal / spoofable auth | `rpc.py:900,915-957` | Security | Yes (receiver-side validation) |
| P12 | Session id reuse after restart; sid wrap bug | `control.py`, `sessionmgr.py:261` | Mis-delivery after crash | Yes (receiver-side) |
| P13 | Broadcast ping reply storms | `chat.py` | Channel congestion | Yes (rate-limit replies) |
| P14 | Positional session ids | `mainapp.py:718,726` | Interop requires chat = 1, rpc = 2 | Must be preserved |
| P15 | File/form offer size is little-endian; every other field is big-endian | `file.py:144`; D-Rats#111 | Wire quirk | Must be preserved exactly |
| P16 | Transport is half-duplex even on TCP (single polled worker) | `transport.py:412-477`; D-Rats#134, #261 | Internet file transfers are unnecessarily slow | Yes: Aether can be full-duplex on TCP. ARQ stays compatible. |
| P17 | Self-addressed traffic is dropped as "looped", so its message lock is never released | `sessionmgr.py:190`; D-Rats#122 | Stuck outbox | Yes (local) |
| P18 | Closed socket is indistinguishable from idle in the polled read; no keepalive | D-Rats#75, #109 | Silent ratflector disconnects | Yes (client-side EOF detection + TCP keepalive) |

---

## 9. What must be verified against real D-RATS before Phase C code depends on it

1. Byte-exact vectors in §2.4 beyond the two upstream self-test vectors already matched. Capture with `sniff_packets` and serial logging (`SWFSerial.set_log_file`) from D-RATS 0.4.x and from 0.3.x if any stations still run it.
2. That a legacy receiver accepts windows > 8 and block sizes other than 512 from an Aether sender (theory: yes; §4.5).
3. That a legacy receiver accepts magic 0x22 (uncompressed) T_DAT/T_ACK/T_REQACK frames (theory: yes; `ddt2.py:203-207`).
4. Behaviour when the sender omits the warm-up frame and random delay.
5. Python 2 D-RATS 0.3.x compatibility. Whether any 0.3.x stations remain in service is an open question (see `01-legacy-feature-inventory.md`).
