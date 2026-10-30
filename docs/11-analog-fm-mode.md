# 11 — Analog FM mode: design note

Date: 2026-10-30 (digipeaters and easy setup added the same day) · Status: **proposal for a later decision; nothing implemented**

Evidence tags as in `03`:
- **[SRC]** read in source (repo@commit:file:line)
- **[DOC]** manufacturer or protocol documentation
- **[COMM]** community report
- **[UNK]** unknown, needs testing

Sources read:
- D-Rats `d0e7a39`: `comm.py`, `agw.py`
- Aether Term `bf7afb0`: `src/tnc/transports.py`, `src/tnc/ble.py`, `src/ax25/kiss.py`, `src/vara/agwpe.py`

## 0. Summary

Aether DV could work over any FM radio, on simplex or through an FM repeater, using an audio packet modem (a "TNC"). The application layers stay unchanged:
- D-RATS frames;
- file and form transfers with their retries;
- chat, ICS-213;
- the store and the UI;
- the scheduler's courtesy rules.

What's new is one port kind, `tnc`. It speaks **KISS** or **AGWPE** to a TNC over **TCP, serial/USB/Bluetooth Classic, or Bluetooth LE**, the same choice of transports Aether Term offers:

| TNC | Typical link |
|---|---|
| Direwolf | KISS over TCP (port 8001) |
| UZ7HO Soundmodem / QtSoundModem | AGWPE over TCP (port 8000) |
| Kenwood TH-D74/D75 built-in TNC | KISS over Bluetooth serial or USB |
| Bluetooth LE TNCs (Mobilinkd, VR-N76/VR-N7600) | KISS over a GATT service |
| Hardware KISS TNCs | Serial |

At 1200 baud AFSK, which passes through FM repeaters, the usable rate is about the same as D-STAR slow data, roughly 80-110 bytes/s against 119. The hard parts aren't protocol:
- **Audio levels:** the most common reason packet fails.
- **Containment:** linked FM systems (AllStar, EchoLink, IRLP) carry audio, and nothing on the radio side reveals the link.

A side benefit: TH-D74/D75 owners, whose radios reportedly can't send D-STAR slow data (`03` §3), could use Aether DV through the radio's built-in packet TNC.

Two owner requirements (added 2026-10-30):
- **Packet digipeaters** are supported as a feature: explicit digipeater paths per station, learned from what is heard, with timing and courtesy that account for every hop (§5).
- **Setup is easy** for Soundmodem, Direwolf and radio TNCs:
  - running TNCs are found automatically;
  - a Direwolf configuration is generated;
  - the modem can be started from Aether DV;
  - received audio levels are shown (§6).

## 1. What D-RATS does

D-RATS has three TNC port types, beside its D-STAR serial port [SRC: `comm.py`]:

| D-RATS port | Framing on air | Notes |
|---|---|---|
| TNC (KISS serial) | Each write of the D-RATS byte stream becomes one KISS data frame, **without an AX.25 header** (`TNCSerial.write` → `kiss_send_frame`, `comm.py:60-83`, `:256`) | The TNC sends it as an HDLC frame. Other D-RATS stations accept it. APRS and packet software see an invalid AX.25 frame. |
| TNC-AX25 | AX.25 UI frame: destination `DRATS`, source path = call + digipeaters, control `03`, PID `F0`, then the payload, **plus a second FCS appended inside the frame** (`TNCAX25DataPath.write`, `comm.py:922-990`) | Source-path loop keeps only the **last** call (`send_src` is overwritten each iteration). |
| AGWPE | Raw `K` frame: AX.25 UI from **`RELAY` to `CQ`** (`transmit_data(conn, "CQ", ["SRC", "RELAY"], buf)`, `comm.py`; `agw.py:686-720`) | Same loop defect: the source field becomes the literal `RELAY`, not the station's call sign. |

Two more findings:
- **The send paths may crash.** `agw.encode_call` joins `str` pieces into `bytes` (`b"".join([chr(ord(x) << 1) …])`, `agw.py:681`). On Python 3, which the D-Rats 0.4.x line runs, that raises a TypeError. So **TNC-AX25 and AGWPE transmit are probably broken in 0.4.x**, and only the plain TNC port sends [SRC; inferred, not run].
- **Receivers are forgiving.** Every D-RATS TNC port passes the received payload, AX.25 header and all, into the same stream search for D-RATS frame markers. So a D-RATS station decodes D-RATS frames whatever packaging carried them [SRC: `TNCSerial.read`, `TNCAX25DataPath.read`, `agw.receive_data`].

Consequences for Aether DV:
- **Send proper AX.25 UI frames**, with our call sign as source. Correct identification, readable by packet tools, and still decoded by D-RATS.
- **Receive all three D-RATS forms:** raw KISS without a header, AX.25 with the extra FCS, and `RELAY`→`CQ`.
- **Don't copy the defects.**

## 2. Modems and transports

| Modem | Rate on air | Through FM repeaters | Notes |
|---|---|---|---|
| AFSK 1200 (Bell 202) | 1200 bit/s; about 80-110 B/s of D-RATS payload after AX.25, D-RATS framing and key-up [MODEL] | **Yes**, the standard | Every TNC above |
| AFSK 1200 with FX.25 or IL2P error correction | A little less | Yes | Direwolf; the other end needs it too, or falls back to plain AX.25 [DOC: Direwolf] |
| 9600 G3RUH | ~800 B/s | **Usually not**: needs flat audio; repeaters filter, pre-emphasise and add tones | Simplex, radio data jack only |

| Transport | Protocol | Status in Aether Term |
|---|---|---|
| TCP (Direwolf 8001; network TNCs) | KISS | Implemented |
| TCP (Soundmodem 8000) | AGWPE | Implemented |
| Serial: USB, or Bluetooth Classic once paired (TH-D74/D75 appear as a "Standard Serial over Bluetooth link" COM port) | KISS | Implemented |
| Bluetooth LE: GATT service `00000001-ba2a-46c9-ae49-01b0961f68bb` (Mobilinkd's, also used by Benshi radios), one notify and one write characteristic | KISS | Implemented, **not yet tested with a radio** |

Aether Term is Python and Aether DV is Rust, so the two apps can't share code directly. They can share:
- the transport model (open; bytes in on a reader; write; "lost" once);
- the KISS parameter handling (TXDELAY, persistence and slot time sent as KISS commands, `ax25/kiss.py`);
- the BLE service and characteristic discovery;
- the settings vocabulary and layout, so an operator who knows one recognises the other;
- test vectors (KISS escapes, AX.25 addresses, AGWPE headers).

## 3. Proposed architecture

```toml
[[port]]
kind = "tnc"
name = "FM"
transport = "tcp"            # tcp | serial | ble
host = "127.0.0.1"           # tcp
tcp_port = 8001              # Direwolf KISS; 8000 with protocol = "agwpe"
# path = "COM7"              # serial (USB or paired Bluetooth Classic)
# ble = "AA:BB:CC:DD:EE:FF"  # Bluetooth LE address from a scan
protocol = "kiss"            # kiss | agwpe
modem = "afsk1200"           # afsk1200 | afsk1200-fec | g3ruh9600 (timing profile)
txdelay_ms = 400             # KISS parameters sent at connect (kiss only)
persistence = 63
slottime_ms = 100
path = []                    # default digipeater path, e.g. ["N4ABC-1"]; empty = direct (§5)

[port.modem]                 # optional: start the sound modem with the port (§6)
program = "C:/direwolf/direwolf.exe"
args = ["-c", "direwolf.conf"]

[port.route]                 # reuses the repeater section (docs/10)
via = "fm-repeater"          # direct | fm-repeater
linked = "unknown"           # no | yes | unknown: declared, can't be detected
```

- **Framing.** Each send is an AX.25 UI frame:
  - destination `DRATS` (what D-RATS's TNC-AX25 port uses);
  - source = station call sign (with SSID);
  - control `03`, PID `F0`;
  - information field ≤ 256 bytes (the AX.25 default N1).

  The D-RATS byte stream is split across consecutive UI frames in one over. Receivers reassemble **per source call sign**, so two stations' frames can't interleave into garbage, which a single shared stream allows. Raw KISS frames without an AX.25 header (legacy D-RATS) go to a per-port stream.
- **Keying** is the TNC's job: Direwolf's CAT/RTS/GPIO/VOX, Soundmodem's settings, or the radio's own TNC. Aether DV never keys the radio on this port.
- **Scheduler.** The same SM-2 scheduler and courtesy rules as D-STAR ports (`10` §4.4), with an `afsk1200` profile:
  - payload rate about 100 B/s;
  - key-up = TXDELAY + the repeater's key-up;
  - shorter overs.

  Every value is a placeholder until measured. The TNC still does its own carrier-sense back-off (persistence and slot time) under ours.
- **Channel sense:**
  - Received frames count as busy data, as today.
  - Neither KISS nor AGWPE reports data carrier detect to the host. So voice is sensed only through the radio's squelch over CAT: CI-V `15 01` on Icom (documented for the IC-9700); later Hamlib `rigctld` for others.
  - Without CAT, the "unsensed" preset and the operator's pause apply, as on older D-STAR radios (`10` §4.11).
- **Frequency guard.** With CAT, read the frequency and refuse to send bulk data on APRS frequencies (144.390 MHz in North America, 144.800 MHz in Europe and elsewhere): D-RATS traffic there would swamp APRS. Without CAT, a warning in setup.
- **Captures and radio tests** work unchanged:
  - the capture records the KISS/AGWPE bytes and the timeline;
  - the summary reports per-transmission timing when CAT TX state is available.

## 4. FM repeaters and containment

| Aspect | D-STAR (`10`) | Analog FM |
|---|---|---|
| Who receives | Decided by R2 in every header | Whoever the repeater's audio reaches |
| Route check | Enforced, read over CI-V or confirmed | **Not possible**: there is no routing field |
| Linked networks | Reflector via R2 = G: our choice | AllStar, EchoLink, IRLP, analog bridges: **the repeater's choice, invisible to us** |
| Data through links | Bit-exact on most paths | AllStar carries audio in µ-law (probably fine); EchoLink uses GSM 06.10, which may or may not pass AFSK 1200 **[UNK]** |
| Digipeaters | n/a | Supported deliberately (§5). Explicit call-sign paths only by default; generic APRS-style aliases (`WIDEn-N`) would spread frames over APRS digipeaters, so they need confirmation and one hop at most |

Safeguards:
- **Declare the link status at setup** (`linked = no | yes | unknown`).
  - `yes` and `unknown` get the reflector rules from `10` §4.6: a confirmation each session, the smaller air-time budget, files limited, no broadcast auto-replies.
  - The setup text tells the operator to ask the trustee.
- **Digipeater paths are empty by default** and always explicit (§5).
- **The APRS-frequency guard** (§3).
- **Timing.** TXDELAY must cover:
  - our radio's key-up;
  - the repeater's receiver and tone-squelch decode;
  - any anti-kerchunk delay;
  - the repeater's transmitter key-up.

  So expect 300-800 ms more than simplex, and a repeater-specific value. Courtesy beeps and hang time look like signals to a squelch sensor, so the hold-off covers them, much as the D-STAR repeater tail does.

## 5. Packet digipeaters

Packet digipeaters relay AX.25 frames on the same frequency. They extend range where no repeater reaches, for example in an ARES net with one digipeater on a hill. They're common on packet frequencies, usually run as nodes such as BPQ32 or JNOS, or by Direwolf in digipeater mode.

**How paths work**
- Each AX.25 frame can name up to 8 digipeaters, in order [DOC: AX.25 v2.2].
- A digipeater relays a frame only when it's the next unused entry, and marks its entry "repeated" (the H bit).
- A receiver can see which digipeaters a frame passed through.

**Design**
- **Paths belong to destinations.** Each station in the Stations list can have a path: "W4XYZ via N4ABC-1". The port also has a default path (`path` in §3) for broadcasts and stations without one. Both are empty by default, meaning direct.
- **Learning paths.** When a frame arrives through digipeaters (H bits set), Aether DV notes the reverse path for that sender and offers it: "W4XYZ was heard via N4ABC-1. Reply the same way?" One click stores it. Paths are never adopted silently, because a heard path can be one-way.
- **Explicit call signs, not aliases.**
  - Generic APRS aliases (`WIDE1-1`, `WIDE2-2`) are flooding paths meant for APRS. On APRS frequencies Aether DV doesn't send bulk data at all (§3).
  - Elsewhere an alias in a path needs the reflector-style confirmation (`10` §4.6), and at most one hop.
  - Aether DV never adds an alias on its own.
- **Air time counts every hop.** With single-frequency digipeaters, every hop retransmits the whole frame on the same channel, so the channel is busy about (hops + 1) times as long. The scheduler uses this throughout:
  - the air-time estimate, budget and maximum over all scale by (hops + 1);
  - frames per over are smaller;
  - the ARQ retry timer starts from (hops + 1) × air time + key-ups at each hop.

  Compose and Transfers show it: "via N4ABC-1: about 2× air time".
- **Duplicate suppression.** Digipeaters drop a frame identical to one relayed shortly before. Direwolf's default window is 30 s [DOC: Direwolf user guide, to check]; others differ [UNK]. A D-RATS retransmission is byte-identical, so a retry inside that window would be silently dropped. Over digipeaters the ARQ retry timer is therefore at least the dedupe window, 30 s by default and adjustable per port.
- **Hidden stations.** Through a digipeater, stations often can't hear each other, so carrier sense helps less and collisions rise. The digipeater profile uses lower persistence, more jitter and smaller windows. Radio tests measure the retry rate (AN-9).
- **Interop.**
  - D-RATS stations receive digipeated frames fine, since they search the payload (§1).
  - D-RATS's own path handling keeps only the last call (§1), so it can't send through more than one digipeater correctly.
  - Its raw-KISS frames have no address field and can't be digipeated at all.
- **Not a digipeater ourselves.** Relaying is the TNC's job: Direwolf's `DIGIPEAT` setting, or the node software. The setup guide says how; Aether DV doesn't relay.

## 6. Easy setup: Soundmodem, Direwolf and radio TNCs

Most packet setups fail on configuration, not protocol, so setup does the work:

1. **Find running TNCs.** Setup looks for TNCs on this computer (localhost only) and lists what it finds, each with a "Use this" button:
   - **Direwolf:** KISS on TCP 8001, AGWPE on TCP 8000.
   - **Soundmodem / QtSoundModem:** AGWPE on TCP 8000, KISS server if enabled (8100 by default [UNK, to check]).

   For AGWPE it asks the version (`R`) and the radio ports (`G`), so it can show "Soundmodem: port 1 AFSK 1200, port 2 …" and you pick the channel.
2. **Pick the hardware:**
   - **Digirig or another CAT+PTT interface:** PTT by RTS or CAT.
   - **SignaLink:** VOX PTT.
   - **A radio with built-in USB audio** (IC-9700, IC-705, IC-7100): PTT over CI-V.
   - **The radio's own TNC** (TH-D74/D75 over Bluetooth or USB, or a BLE TNC): no sound modem needed. Setup shows how to put the radio in KISS mode, as Aether Term's manual does.

   The choice fills in the TNC's PTT method and sensible KISS parameters: TXDELAY, persistence and slot time for simplex, or repeater values (§4).
3. **Direwolf without editing files.** If Direwolf isn't running, setup can write a `direwolf.conf` for you. It contains:
   - `MYCALL`;
   - the sound device chosen from Direwolf's own device list;
   - `MODEM 1200`, optionally with FX.25;
   - the PTT line for the chosen hardware;
   - `KISSPORT 8001` and `AGWPORT 8000`;
   - for digipeater users, a commented `DIGIPEAT` example.

   You confirm the file before it's written.
4. **Start it from Aether DV (optional).** Like Aether Term's modem auto-launch, a `[port.modem]` setting with the program's path starts Direwolf or Soundmodem with the port and stops it on exit. Its console output goes to the engine log and into radio-test bundles, so a support bundle shows Direwolf's own messages.
5. **Check audio levels.** Direwolf prints the audio level of every received packet; around 50 is right [DOC: Direwolf user guide]. When Aether DV starts Direwolf, it reads those lines and shows the level on the port plate: "audio 48, good" or "audio 140, too loud: turn down the receive level". For Soundmodem, which has no level interface [UNK], the guide shows where its own level bar is.
6. **Test.** "Send a test packet" sends one short UI frame to `TEST`. The guide suggests checking it with a second receiver or another station. The radio test card (Diagnostics) records the rest.

Port setup also shows **Where**: simplex, an FM repeater (and whether it's linked, §4), or through digipeaters (§5).

The port plate shows "FM packet", the modem, the audio level (when known), the default path, and the same channel and courtesy states as D-STAR ports.

The Radio Setup guides gain an "FM packet" section per radio and interface: Digirig, SignaLink, built-in USB audio, TH-D74/D75 TNC, Mobilinkd.

## 7. Plan, if adopted

| Step | Content | Test |
|---|---|---|
| A1 | `tnc` port: KISS over TCP to Direwolf; AX.25 UI framing and per-source reassembly; receive all D-RATS forms (§1); `afsk1200` profile; APRS-frequency guard; finding running TNCs, Direwolf config generation and start, audio level display (§6) | Unit (KISS and AX.25 vectors shared with Aether Term); two Direwolf instances connected by an audio loopback (null sink) on the bench; CI at the KISS level with a simulated TNC |
| A2 | Serial and Bluetooth Classic KISS (TH-D74/D75, hardware TNCs) | Bench with a TH-D75 |
| A3 | AGWPE (Soundmodem, QtSoundModem): version and port listing, channel choice, start from Aether DV | Bench on Windows |
| A4 | Bluetooth LE KISS (`btleplug`) | Bench with a Mobilinkd or VR-N76. Untested in Aether Term too. |
| A5 | CAT squelch and frequency through Hamlib `rigctld` for non-Icom radios | Bench |
| A6 | FM repeater: TXDELAY per repeater, linked declaration, timing measured with radio tests | On air, with a trustee |
| A7 | Digipeaters: per-station and default paths, learned reverse paths, hop-aware timing and budget, dedupe-safe retry timer, digipeater profile (§5) | Bench: Direwolf as a digipeater between two stations on separate sound cards; then on air through a packet node |

Interop tests against D-RATS 0.4.x: its plain TNC port sends and receives; its TNC-AX25 and AGWPE ports, which may fail to transmit (§1), are checked for receive only.

## 8. Validation register

| ID | Question |
|---|---|
| AN-1 | D-RATS 0.4.x interop over its TNC, TNC-AX25 and AGWPE ports. Do the AX.25/AGWPE send paths really fail on Python 3? |
| AN-2 | AFSK 1200 throughput and retry rate through an FM repeater, against simplex |
| AN-3 | TXDELAY needed per repeater (receiver, tone decode, anti-kerchunk) |
| AN-4 | Largest frame the TH-D74/D75 TNC, Mobilinkd and Soundmodem accept and send |
| AN-5 | AFSK 1200 data through AllStar and EchoLink links |
| AN-6 | Squelch-based voice yield on FM (CI-V `15 01`, Hamlib) |
| AN-7 | FX.25/IL2P gain on weak signals, and behaviour with a peer that lacks it |
| AN-8 | Duplicate-suppression windows of Direwolf, BPQ32, JNOS and others; does the 30 s retry floor avoid dropped retries? |
| AN-9 | Retry rate and throughput through one and two digipeaters with hidden stations |
| AN-10 | Soundmodem / QtSoundModem KISS server port and AGWPE `G` port listing across versions |

## 9. Decisions for later

1. Adopt the analog mode, and when. Recommendation: after the D-STAR bench and on-air tests, so the shared scheduler values are measured.
2. Name: keep "Aether DV" or move to "Aether" once it isn't D-STAR only.
3. Direwolf (GPL-2.0-or-later, compatible with our GPL-3.0): require it as a separate install (recommended; as Aether Term does with its modems), or bundle it.
4. Whether to also send legacy raw-KISS frames for very old D-RATS stations. Recommendation: receive them, send proper AX.25 only.
5. Generic digipeater aliases (`WIDEn-N`) away from APRS frequencies: allow one hop with confirmation (recommended), or explicit call signs only.
