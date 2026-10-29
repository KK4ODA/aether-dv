# 03 — D-STAR RF Transport: DV Slow Data and DV Fast Data

Evidence tags used throughout:
- **[SRC]** read in open-source code (repository@commit:file:line)
- **[DOC]** manufacturer or JARL documentation
- **[COMM]** community report
- **[MODEL]** analytic estimate
- **[UNK]** unknown or unverified

Nothing in this document is hardware-validated yet. That is Phase C/D work (`07` §3).

## 1. D-STAR DV air interface: the parts that matter for data

| Item | Value | Evidence |
|---|---|---|
| Modulation / gross rate | GMSK, 4800 bit/s | [DOC] JARL D-STAR spec; [SRC] MMDVMHost `DStarDefines.h` |
| Frame | 20 ms = 72 bits AMBE voice (9 B) + 24 bits slow data (3 B) | [SRC] `ircDDBGateway@f5ab9f7:Common/DStarDefines.h:61-68`; `MMDVMHost@9384d68:DStarDefines.h:41-42,78` |
| Superframe | 21 frames = 420 ms. Frame 0 carries the data-sync pattern `55 2D 16` in place of slow data | [SRC] `ircDDBGateway:DStarDefines.h:29` |
| RF header | 41 bytes (MYCALL, URCALL, RPT1/2, flags), sent once with FEC before voice frames. Late entry via the `0x5X` header-retransmission slow-data blocks. | [SRC] `MMDVMHost:DStarDefines.h:24` |
| End of transmission | Dedicated end pattern | [SRC] `MMDVMHost:DStarDefines.h:27` |
| Slow-data scrambling | XOR `70 4F 93` per 3-byte group | [SRC] `ircDDBGateway:DStarDefines.h:111-113` |
| Slow-data block | Two frames = 6 bytes = 1 mini-header + up to 5 payload bytes. Header high nibble = type; low nibble = valid length | [SRC] `ircDDBGateway:Common/DRATSServer.cpp`, `SlowDataEncoder.cpp` |
| Mini-header types | `0x3X` GPS / user data (**D-RATS rides here**), `0x4X` 20-char text message, `0x5X` header retransmission, `0x8X/0x9X` **Fast Data**, `0xCX` code squelch | [SRC] `ircDDBGateway:DStarDefines.h:85-92`; `MMDVMHost:DStarDefines.h:47-54` |

### 1.1 Conventional DV slow data

- Raw capacity: 20 data-bearing frames per superframe = 10 blocks × 5 payload bytes = **50 B per 420 ms = 119 B/s (≈952 bit/s)**. This is the origin of the "≈950 bps" figure. [SRC + arithmetic]
- In practice the radio also spends blocks on the 20-character TX message (`0x4X`, 4 blocks) and on header retransmission (`0x5X`), and GPS-TX (`0x3X`) competes with application data. The usable rate is therefore **below 119 B/s** and depends on radio settings. [SRC: `SlowDataEncoder.cpp` interleaves text/GPS/header] [UNK: exact Icom scheduling, to be measured in HW-5]
- No FEC protects slow-data bytes. The AMBE codec's FEC protects only voice bits. One bit error corrupts the DDT2 frame, and the CRC drops it. [SRC/DOC]
- **Coexistence with voice**: slow data is sent *alongside* voice. A D-RATS station can transmit data while someone talks, and voice PTT sessions carry the radio's own text message and GPS, so they consume slow-data capacity too. Data received during a voice over is decoded if the radio forwards slow data to the port (e.g. TH-D75 Menu 618 "Data Frame Output = All") [COMM: macrats].
- **Packet-loss marker**: per JARL STD6 §6.2, `E7 84 76` in slow data next to the silence AMBE pattern means "packet loss", and receivers discard it. D-RATS escapes `E7`/`84` since PR #302 [SRC/COMM: D-Rats#305].
- **How D-RATS uses it**: it writes DDT2 bytes to the radio's data port. The radio packs them into `0x3X` blocks, keys up automatically ("DV Data TX: Auto" on Icom), and unkeys after a configurable idle tail. D-RATS sees none of this. ircDDBGateway's repeater-side bridge confirms the on-air encapsulation: it only extracts `0x3X` blocks and reassembles bytes until `[EOB]` (`ircDDBGateway:Common/DRATSServer.cpp`). [SRC]

### 1.2 DV Fast Data: on-air mechanism

- In Fast Data frames, the 9 voice bytes **and** the 3 slow-data bytes carry data. Fast Data blocks are marked by mini-header `0x80`–`0x9C`. The low 5 bits are a length of up to 28 bytes per block ("0x80 + 28 = 0x9C, as per specs chapter 6.1.3"). [SRC: `MMDVMHost@9384d68:DStarControl.cpp:394-399,434-439`]
- Receivers detect Fast Data **per frame** from the mini-header and must then **skip AMBE processing**: FEC regeneration, DTMF decoding/blanking, and silence substitution. Any of those corrupts Fast Data. [SRC: `ircDDBGateway:Common/RepeaterHandler.cpp:742-754` ("Don't do AMBE processing when in Fast Data mode"); MMDVMHost commits `b2844bc` (2020) and `876f58c` (2026) "prevent first voice frame to get killed by FEC"]
- Gross ceiling: 12 B per 20 ms = 600 B/s. Icom's advertised **3480 bit/s ≈ 435 B/s** is consistent with block headers and sync frames removed. [MODEL; DOC figure from Icom — see §3]

### 1.3 The infrastructure path is a compatibility variable

| Path element | Slow data `0x3X` (D-RATS) | Fast Data `0x8X/0x9X` | Evidence |
|---|---|---|---|
| Simplex radio ↔ radio | Yes | Only if both radios support FD (§3) | [DOC/COMM] |
| Icom repeater (ID-RP2/RP4000 era) + RS-RP3C/ircDDBGateway | Yes | **[UNK]** RF repeat is likely; gateway D-RATS bridge does not carry FD | [SRC: `DRATSServer.cpp` handles only `0x3X`] |
| ircDDBGateway (G4KLX) repeater handler | Yes | Detected and AMBE processing skipped (`RepeaterHandler.cpp:742-754`) | [SRC] |
| MMDVMHost repeaters/hotspots | Yes | **Depends on MMDVMHost version**: added 2020-11 (PR #667), removed 2025-02-27 (`351c119`, "mangling D-Star DTMF data"), re-added 2025-03-12 (`78fc96b`), **switched off 2025-04-24** (`8e0705e`), re-enabled 2026-01/02 (`d3ed3ce`, `876f58c`, merged as PR #852). Deployed images (Pi-Star, WPSD, etc.) vary. | [SRC: MMDVMHost git history] |
| Reflectors (REF/XRF/DCS/XLX) | Slow data is transported transparently in DV frames | **[UNK]** frames likely pass if every hop leaves AMBE bytes untouched; any transcoding or FEC regeneration breaks FD | [UNK] |

**Consequence:** "Is Fast Data possible to station X?" depends on *both* radios and *every repeater/hotspot/gateway hop*. Aether cannot know the path in advance, so it must learn path capability from end-to-end evidence (`05` §7, ADR-0005).

Additional Fast Data facts (source: [`research-notes/fastdata_research.md`](research-notes/fastdata_research.md), which quotes the JARL standard ch. 6–7, Icom patent US9961634B2 and Icom manuals; spot-checked items are marked ✓):
- Introduced in JARL standard **v5.0 (Aug 2014)**. ID-51 PLUS supported it at launch; ID-5100 from firmware Release E2 (Oct 2014). [DOC]
- Per superframe: block 1 carries 28 B, blocks 2–10 carry 20 B each, giving 208 B per 420 ms ≈ 495 B/s before overhead. Voice-frame byte 4 is a fixed `0x02` "mitigation" byte to reduce vocoder noise on non-FD receivers. [DOC: JARL]
- A **beep (normal AMBE) block is inserted about every second**, so non-FD radios hear periodic beeps. This is why the rate is ≈3480 bps, not ≈3960. [DOC: JARL §7.5, patent]
- **Header retransmission (`0x5X`) is not sent during Fast Data**, so late-entry receivers cannot recover the RF header. [DOC]
- **Voice PTT reverts to slow data.** Text messages and code squelch take priority. [DOC]
- What a slow-only radio outputs on its data port when it receives Fast Data: **[UNK]**. Probably nothing, since there is no `0x3X` block. HW-8 will measure it.

## 2. The computer-facing interface

Documented for every current Icom model checked [DOC: manuals, ✓ ID-52 Advanced Manual]:
- One **"DV Data" serial function carries both slow and fast data.** The Fast Data on/off choice is a radio setting, not a different port or protocol.
- 8N1, **Xon/Xoff flow control** (no RTS/CTS documented). USB-CDC ports accept "your desired speed"; DATA-jack radios use 4800/9600.
- "Before transmitting the data, the transceiver sends a carrier sense signal approximately 500 milliseconds." Every over therefore pays ≈0.5 s plus key-up.
- "Only ASCII code can be used for data communication." In practice D-RATS sends yEnc-escaped binary and works, so the restriction is about specific reserved byte values.
- **Reserved bytes in simple (slow) data** per JARL §6.2: `00 11 13 76 84 E7 FE`. D-RATS escapes all of these **except `0x76`** (`yencode.py:38`, where the comment mentions 0x76 but the set omits it). Whether `0x76` alone causes loss is **[UNK]**; the documented loss trigger is the sequence `E7 84 76`, which escaping E7/84 already breaks. Aether's sender may add `0x76` to its escape set (decode-compatible with legacy receivers) if HW-2 shows any effect.
- Whether the port is byte-transparent for `0x00/0x11/0x13/0xFD–0xFF` **in Fast mode** is **[UNK]**. Keep escaping in both modes until HW-2 proves otherwise.

**Programmatic control: documented CI-V commands exist** (✓ verified by reading the Icom CI-V Reference Guides for ID-52, ID-52 PLUS, ID-50, ID-5200, IC-705, IC-9700 and IC-905):

| Radio | DV Data TX (PTT/Auto) | **DV Fast Data (OFF/ON)** | GPS Data Speed (Slow/Fast) | TX Delay (PTT) |
|---|---|---|---|---|
| ID-52, ID-52 PLUS, ID-50, ID-5200 | `22 02` | **`22 03`** | `22 04` | `22 05` (00–10 s) |
| IC-705 | `1A 05 0092` | **`1A 05 0093`** | `1A 05 0094` | `1A 05 0095` [notes] |
| IC-9700 | `1A 05 0076` | **`1A 05 0077`** | `1A 05 0078` | `1A 05 0079` |
| IC-905 | `1A 05 0097` | **`1A 05 0098`** | [notes] | [notes] |
| ID-4100, ID-51 PLUS/PLUS2, ID-31 PLUS | `22 xx` per notes | `22 03` per notes, **not spot-checked** | | |
| ID-5100 | **[UNK]** | **[UNK]** | | |

Also documented: CI-V `22 00` sends up to 30 bytes of DV TX data, and `22 01` controls RX data output. This is a binary-safe framed data path with `FA–FF` escaped. **Whether `22 00` data is transmitted as Fast Data is [UNK].**

Practical constraint: control needs a **CI-V channel open at the same time as the DV Data channel.**
- IC-705, IC-9700 and IC-905 expose two USB serial ports: A for CI-V and B, which can be set to DV Data. Both work at once [DOC].
- On ID-52 / ID-50 / ID-52 PLUS / ID-5200, the USB "Serialport Function" is **either** CI-V **or** DV Data (✓ ID-52 Advanced Manual §12). Simultaneous use needs USB plus Bluetooth (each with its own Serialport Function), or the CI-V `22 00/22 01` data path on a single port.
- DATA-jack radios (ID-4100, ID-51/31 PLUS) have a "CI-V (DATA Jack)" option that the D-RATS wiki says to keep OFF for data use. Simultaneous operation is **[UNK]**.

Unknowns about the control path, all hardware-test items:
1. Whether toggling Fast Data mid-session is safe (buffer flush? key-up glitch?).
2. Whether the setting is written to non-volatile memory on every change (flash wear if toggled per transmission).
3. Settling time after a change.
4. Behaviour while transmitting.

## 3. Radio compatibility matrix (Phase A, documentation level; nothing hardware-verified)

| Radio | DV slow data (D-RATS) | DV Fast Data | Firmware notes | Data interface | Bluetooth data | FD readable/settable by software |
|---|---|---|---|---|---|---|
| **IC-705** | Yes | Yes [DOC ✓ manual list "as of July 2020"] | v1.40 (2025-08): end-of-FD beep | USB port B = DV Data (port A = CI-V) | Built-in SPP, Serialport Function = DV Data | **Yes**, `1A 05 0093` ✓, concurrent via USB A |
| **IC-905** | Yes | Yes [DOC ✓] | — | USB (B) = DV Data | None documented | **Yes**, `1A 05 0098` ✓ |
| **IC-9700** | Yes | Yes [DOC ✓] | v1.50 (2021-07): FD beep improvements | USB (B) or DATA jack, 4800/9600 (D-Rats#263: use the port not used by the programming software) | None | **Yes**, `1A 05 0077` ✓ |
| **ID-52 A/E** | Yes | Yes [DOC] | — | USB: Serialport Function = DV Data (exclusive with CI-V) ✓ | Built-in SPP | **Yes**, `22 03` ✓; concurrent only via USB plus BT |
| **ID-52 PLUS** | Yes | Yes [DOC] | — | USB-C, as ID-52 | Built-in SPP (+ Android terminal mode) | **Yes**, `22 03` ✓ |
| **ID-50 A/E** | Yes | Yes [DOC] | — | USB, as ID-52 | None | **Yes**, `22 03` ✓; **no concurrent channel** (single USB function) → use the CI-V data path or manual |
| **ID-5200 A/E** | Yes | Yes [DOC] | 2025–26 model | USB-C or DATA jack | Built-in SPP | **Yes**, `22 03` ✓ |
| **ID-4100 A/E** | Yes | Yes [DOC] | **V1.02 (2019-11-06) fixes FD data loss** | DATA jack (OPC-2350LU / OPC-2218LU), Xon/Xoff | Optional UT-137 | `22 03` [notes, not spot-checked] |
| **ID-5100 A/E** | Yes | Yes with firmware **E2+** (CPU M 1.10, S 1.00, C 1.10, DSP 1.10 or later ✓ IC-705 manual note); later data-loss fix | — | DATA jack | Optional UT-133 | **[UNK]** |
| **ID-51 PLUS / PLUS2 / 50th** | Yes | Yes [DOC ✓] | 2019-11 data-loss fixes; E3/E4 call-end-frame fixes (F4FXL report of the E3 GPS bug) | DATA jack, 4800/9600 | None | `22 03` [notes] |
| **ID-31 PLUS** | Yes | Yes [DOC ✓] | 2019-11 data-loss fix | DATA jack | None | `22 03` [notes] |
| IC-7100, IC-9100, IC-R30, ID-31/ID-51 (non-PLUS), ID-880/IC-80/IC-92/IC-2820 etc. | Yes | **No** | — | various (IC-91/92/2820 at 38400 per D-RATS wiki) | — | n/a |
| **Kenwood TH-D74** | **Receive only (reported)** | Yes, "Data Mode", reportedly Icom-compatible [COMM: MMDVMHost#470] | — | USB (`ttyACM`, D-Rats#309) | Yes | n/a |
| **Kenwood TH-D75** | Reported as for TH-D74 [COMM] | Yes (manual wording "fast data TX") [COMM: macrats] | Menu 614 ≠ Off required [COMM] | USB-C CDC (DTR must be asserted) | SPP via RFCOMM ch. 2 (Menu 984) | n/a |

Matrix maintenance: this table becomes `profiles/*.toml` in the codebase, and each cell carries its evidence level. The UI shows "unverified" for anything below Hardware level.

## 4. Automatic mode selection: what is and isn't feasible

Keep three questions separate.

**(a) Can Aether tell whether the *local* radio is FD-capable?**
- The *model* can be identified via CI-V "Read transceiver ID" (`19 00`) on a port the operator designates as CI-V. Aether must not probe unknown ports blindly; the ID is then looked up in the profile DB. On DV-Data-only connections (e.g. an ID-50 on its single USB function, or a DATA-jack radio with CI-V off) there is no identification channel, so the operator picks the model.
- The *firmware* version is generally not readable via CI-V [UNK per model], so older ID-5100/ID-4100 firmware can only be flagged as "verify".

**(b) Can Aether tell whether the radio is *currently configured* for FD, and change it?**
**Yes, on the current Icom lineup, if a CI-V channel is available concurrently** (§2 table). Otherwise no, and the operator must set the menu. Kenwood: no documented control path.

**(c) Can Aether tell whether the *remote station and path* support FD?**
**Not from the radio.** The DV data port provides only decoded bytes and no RF-header or mode metadata [UNK whether any radio exposes received-mode info via CI-V; to be investigated]. Software-level signals:
1. A capability record from an Aether peer (via `RPCGetVersion` extension) can state "my radio is FD-capable and enabled". That is a *declaration*, not proof of the path.
2. Received evidence: if an FD-configured local radio receives a peer's data while that peer's capability record says FD-on, the peer can at least *send* fast. Reception at our side then says nothing new about the reverse path, but the radio pair is FD-capable.
3. **Proof**: an end-to-end ACK of data sent while local FD was on. Paths change (different repeater, hotspot firmware), so proofs must age.

A legacy D-RATS peer never declares capabilities. Its FD status is unknown unless the operator declares it.

**Feasibility verdict.** "Auto" as *capability negotiation* (protocol level) is feasible between Aether peers. "Auto" as *radio reconfiguration* is technically feasible on CI-V-equipped Icom models. But there is a hazard: **a station that switches its radio to FD for one peer cannot be heard by any slow-only station on the same channel during that time.** On a shared net frequency that is a real operational cost. Recommended policy (ADR-0005):
- Default **Slow** on nets with unknown or legacy stations.
- **Fast** for point-to-point or nets where every participant is proven fast.
- **Auto** switches only (i) between transfers, (ii) toward peers with proven-fast status, (iii) on radios with a verified CI-V path, (iv) restoring the previous state afterwards and on failure, with every switch logged and shown.
- If an FD transfer stalls while the peer stays reachable in slow, mark the path "disproven" and fall back. ARQ makes this safe: unacknowledged data is simply resent in slow mode.
- Kenwood TH-D74/TH-D75 peers reportedly cannot send slow data. Talking to them needs FD on the Icom side. Profile data will flag this.

## 5. Radio behaviour, scheduling, and the limits of what the app can know

| Topic | What D-RATS does | What is actually knowable | Aether approach |
|---|---|---|---|
| TX/RX turnaround | Random 0.5–2 s pre-TX delay (`transport.py:344-357`); 12 s minimum ARQ timeout | Key-up latency, tail and minimum re-key gap are radio-specific and measurable (HW-4) | ModeProfile constants from measurement; RTO = measured RTT distribution + margin |
| Auto vs manual PTT | Relies on the radio's "DV Data TX = Auto" | Whether the radio keys on data, and how long it holds after the last byte (Kenwood Menu 614: Off silently disables data TX [COMM: macrats]) | Profile-guided setup checklist; HW-1/HW-4 verify |
| Carrier sense | None | The data port shows only decoded data; voice-only or noise activity is invisible. CI-V may expose squelch/busy on some models [UNK]. | RX-activity hold-off + bounded random back-off; optional CI-V busy input where documented |
| Half-duplex contention | Random delay | Collisions are only inferable from loss | Short bursts toward unknown/legacy peers; ACK-clocked bursts |
| Radio buffer / flow control | Soft XON/XOFF; 8-byte writes (`comm.py:430-463`) | Buffer size and XOFF thresholds are radio-specific (HW-3) | Explicit per-profile flow control; never discard bytes; pace to the measured air rate, so the radio buffer never holds more than one burst |
| Serial throughput and latency | 8-byte chunk + flush loop | USB-CDC is fast; Bluetooth SPP has added latency/jitter | Measure per interface; at most one burst in flight to the radio |
| Partial writes / drops | Not detected | `write()` success = accepted by OS/radio only | **Never treat write success as delivery**; only end-to-end ACK counts |
| Voice during data | Not handled | On FD radios, voice PTT transmits voice with slow data [DOC/COMM — see §3]; a data burst cannot pre-empt voice | Pause bulk while local PTT/voice RX is detected (if observable); otherwise rely on ARQ |
| Recovery after interruption | Session timeouts (90 s idle, 10 retries) | Port loss is detectable (EOF/IO error, DSR, POLLHUP) | Transfer suspension state; resume by offset/hash |

## 6. RF efficiency: inherent to D-STAR versus introduced by D-RATS

| Limitation | Inherent (D-STAR / radio) | Introduced by D-RATS (fixable) |
|---|---|---|
| ~119 B/s slow ceiling; ~435 B/s Fast ceiling | ✔ | |
| No FEC on data bytes; superframe-sync-loss erasures | ✔ | (application FEC possible but deferred) |
| Per-over cost: key-up, RF header, tail, receiver lock | ✔ (magnitude radio-specific) | |
| Half-duplex; no carrier sense on the data port | ✔ | |
| Radio inserts text/header/GPS blocks into slow data | ✔ (settings can reduce it) | |
| Random 0.5–2 s pre-TX delay on every burst | | ✔ |
| 12 s minimum RTO; rate estimate from serial write time | | ✔ |
| 4 KB in-flight cap, window 2–8, fixed 512 B blocks regardless of loss | | ✔ (sender-local) |
| Compressing small/control frames (they get larger) and double compression | | ✔ |
| yEnc expansion ~4.6 % | partly (radio byte filtering) | escaping set could be tuned per radio |
| Poison `[EOB]` blocks | | ✔ |
| Half-duplex behaviour even on TCP | | ✔ |
| Byte loss from flow-control handling (fixed in 0.4.2) | | ✔ (historical) |

Model results (`tools/arq_model.py`, `07` §4.1): with D-RATS defaults, Fast Data yields ≈2.45× slow, not 3.66×. Sender-local tuning raises slow by ≈1.3–2.5× (more at higher BER) and restores fast/slow to ≈3.3–3.6×. **These are hypotheses for HW-5/HW-7/D4, not results.**

## 7. Research track: frame-level adapters (not in the first release)

With byte-stream adapters (D-RATS, Aether v1) the radio owns framing and mode. A frame-level adapter would let software build the 12-byte DV frames itself: header, slow-data mini-headers, and Fast Data blocks. That gives exact control of mode per transmission and visibility of received mode and RF header.

| Option | Evidence | Status |
|---|---|---|
| MMDVM modem (hotspot board, or a TH-D75 in terminal mode) | MMDVM firmware transmits whatever 12-byte frames the host supplies [SRC: MMDVMHost]; the TH-D75 terminal mode speaks the MMDVM host protocol at 38400 [SRC: macrats `MMDVMProtocol.swift`] | Plausible; hotspot power only (mW). TH-D75 "terminal mode" does not use RF [notes]. |
| Icom Access Point / Terminal Mode protocol | Undocumented by Icom; implemented in G4KLX DStarRepeater `IcomController` and community docs (38400 baud, `[len][type]…0xFF` packets) [notes] | Terminal Mode = no RF; Access Point Mode = RF. FD pass-through **[UNK]**. |
| CI-V `22 00` / `22 01` DV data | Documented, binary-safe, ≤30 bytes per command | Possibly a cleaner byte path than the DV Data port; FD behaviour **[UNK]** (HW-10) |

The adapter trait in `05` §4 (`ByteLink` vs a future `FrameLink`) keeps this option open without designing for it now.
