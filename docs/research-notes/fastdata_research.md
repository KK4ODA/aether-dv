# Icom D-STAR "DV Fast Data": research notes

Compiled 2026-10-29 for the architecture study of a D-RATS-compatible data application.

Evidence tags:
- **[documented: X]**: a primary source says it (JARL standard, Icom/Kenwood manual, firmware notes, source code).
- **[community report: X]**: forum, issue tracker, blog or third-party README.
- **[inferred: X]**: my own reading of source code or arithmetic. No source states it directly.
- **[unknown]**: no evidence found.

All PDFs cited were downloaded and searched as full text. Icom manuals came from icomjapan.com/support/manual (2026 editions) and rigpix.com. URLs are listed at the end.

---

## 0. Corrections to the background assumptions

| Assumption | Verdict |
|---|---|
| DV = 4800 bps GMSK, 20 ms frames of 72 bit AMBE + 24 bit data, 21-frame superframe, sync every 420 ms | Correct [documented: JARL STD 6.0 §4.1, ch. 6 and 7] |
| Slow data is about 950 bps usable | Correct. JARL says "about 950bps", and Icom manuals say "approximately 950 bps" [documented: JARL §6.1.2; ID-52 manual p.12-10] |
| Fast Data is about 3480 bps | Correct. JARL §7.1 says "a transfer rate of approximately 3480 bps can be obtained", and Icom manuals use the same figure ("approximately 3.5 times faster") [documented] |
| Introduced in the ID-51 PLUS2/ID-4100 era, 2016–17 | **Wrong. It is older.** JARL added Fast Data in standard **Ver. 5.0, August 2014**. The ID-51A/E PLUS manual says "The DV Fast Data can be used with this product (As of August 2014)". ID-5100A/E **Release E2 (Oct 30 2014)** says "DV Fast Data is now supported" [documented]. The Icom patent has a 2015-02-19 priority date [documented: US9961634B2] |
| It is not DD mode | Correct. Fast Data runs in DV mode on any DV band [documented] |

---

## 1. On-air mechanism

### 1.1 Frame and block structure [documented: JARL STD 6.0 ch. 7, pp. 78–85]

- Fast Data reuses the conventional simple-data block concept (ch. 6). It differs in two ways: the **voice frames carry data too**, and it has its own mini-header codes.
- One superframe is 420 ms and 21 frames, numbered 1–21 in the standard. Frame 1's data field is the sync pattern. The superframe is divided into **10 blocks** (block numbers 1–10):
  - **Block 1** = frames 1, 2, 3 (it contains the sync frame). Byte order in the block: data(2), data(3), voice(2), voice(3), voice(1). The sync frame's voice field is used but placed last. **28 valid bytes.**
  - **Blocks 2–10** = frame pairs (4,5), (6,7) … (20,21). Byte order: data(even), data(odd), voice(even), voice(odd). **20 valid bytes each.**
- Byte layout of the 6 data-frame bytes in a block: `mini-header(1) | data(2) | guard(1) | data(2)`. That gives 4 valid bytes.
  - **Guard** byte: "an arbitrary value that does not match the packet loss pattern". It stops a block from being read as the network packet-loss marker.
- Byte layout of each 9-byte voice frame: `data(4) | mitigation(1) | data(4)`. That gives 8 valid bytes.
  - **Mitigation** byte is always **0x02**. Its stated purpose is "to mitigate vocoder noise at the receiving side when received by an existing model".
- Capacity per superframe: 28 + 9×20 = **208 bytes per 420 ms**, about 495 B/s or about 3960 bps before overhead [inferred: arithmetic]. JARL gives "approximately 3480 bps" because a beep is inserted periodically (next section) [documented]. My arithmetic makes that overhead roughly 12%, about one slow block of beep audio per few hundred ms [inferred].

### 1.2 Mini-header (how a receiver recognises Fast Data) [documented: JARL §6.1.3 table, ch. 7]

| Mini-header | Meaning |
|---|---|
| 0x3X (X = 1–5) | Simple data (D-PRS and user serial data) |
| 0x4X | Message (20 characters) |
| 0x5X | Radio header retransmission |
| 0x66 | No data |
| **0x8X, X = 1–F** | **Fast Data, length 1–15 bytes** |
| **0x9X, X = 0–C** | **Fast Data, length 16–28 bytes** |
| 0xCX | Code squelch |

- In practice the header is **0x80 + n** for n valid bytes (n = 1–28). A full block 1 is 0x9C and a full block 2–10 is 0x94 [inferred: table; MMDVMHost comment "0x80 + 28d = 0x9C"].
- The mini-header sits in the first data frame of each block, which is the same position as the slow-data mini-header. Because a voice frame arrives *before* its data frame, a receiver has to **buffer voice frames** until it has read the block's mini-header [inferred]. MMDVMHost added exactly that "lookback" buffering in Jan 2026 to stop the first frame being corrupted by FEC [documented: MMDVMHost commit 876f58c].
- Icom patent US9961634B2 describes receiver behaviour [documented]:
  - Mute after 8x/9x is seen in **3 consecutive blocks (120 ms)**.
  - Unmute after 3 consecutive non-8x/9x blocks.
  - Audio is delayed by 3 blocks so the mute takes effect before any noise is heard.
  - The receiver unmutes immediately when the periodic beep is detected.

### 1.3 Scrambling [documented: JARL §7.4, Ap1]

- In Fast Data the **voice frames are scrambled too**. In normal DV only the data frames are scrambled.
- The scrambler is S(x) = x^7 + x^4 + 1, initialised to all ones. It is **re-initialised at the start of every voice frame and every data frame** (diagram on p. 81). Sync frames are not scrambled.
- The 9-byte PN sequence is `70 4F 93 40 64 74 6D 30 2B`. The first 3 bytes are the familiar slow-data XOR [documented: MMDVMHost DStarDefines.h `DSTAR_SCRAMBLER_BYTES`].
- So on air the mitigation byte 0x02 at voice byte 4 appears as 0x66 [inferred].

### 1.4 Switching, beep, PTT, coexistence [documented: JARL §7.5–7.6; Icom manuals]

- **Beep:** "a beep sound indicating fast data shall be inserted periodically for … approximately one second". The sender switches to a normal slow-data block (real AMBE beep audio in the voice frames) at the end of a fast block, then switches back.
  - The patent says a beep goes "every 1 sec".
  - So non-Fast-Data radios hear a periodic beep plus whatever the mitigated data decodes to [documented intent; actual sound unknown].
- **PTT during fast data:** at the end of the current fast block, the radio switches to voice/slow data. Every Icom manual repeats this: "Even if 'ON' is selected, when you push [PTT], the data is sent at the slow speed".
- **After PTT release:** the radio may switch back to fast data (slow→fast), "for example, to transmit GPS data when the PTT is off".
- **Message (0x4X) and code squelch (0xCX)** take priority: Fast Data is suspended while they are sent.
- **Header retransmission (0x5X) is not performed during Fast Data.** Late-entry receivers therefore cannot recover the header from slow data while fast data is running [documented, with consequence inferred].

### 1.5 RF header

- JARL ch. 7 defines **no header change** for Fast Data [documented by absence].
- Header Flag1 bit 7 (data/voice) exists. JARL §4.2 repeater logic says to treat a transmission as voice only if this flag = 0 [documented].
- Fast Data travels through DV repeaters and MMDVM detects it only by mini-header. So the header very probably keeps bit 7 = 0, an ordinary DV header [inferred].
- G4KLX in 2018: "a header bit indicates data or voice, but it isn't checked" [community report: MMDVMHost #470]. Status: **[unknown, probably unchanged]**.

### 1.6 Leading frames observed on air [community report: MMDVMHost #470, Nov 2018]

- Kenwood TH-D74: the first 21 voice frames carry `EE C2 A1 C8 42 6E 52 51 C3` before the data.
- Icom ID-51/ID-4100: the first voice frame is `B2 4D 22 48 C0 16 28 26 C8`, or a scrambled all-zero frame (`70 4F 93 40 64 74 6D 30 2B`).
- MMDVMHost briefly used these as signatures (2018) before switching to mini-header detection (2019).
- Purpose is unknown. It may be the beep or a lead-in [unknown].

### 1.7 Infrastructure pass-through

**Root cause of corruption:** most software repeaters "regenerate" AMBE FEC (Golay) on voice frames and blank DTMF. Applied to fast-data voice frames, that **destroys the data**. TH-D74 data through an unpatched ZUMspot showed about 12.6% BER [community report: #470].

| Component | Behaviour |
|---|---|
| **MMDVMHost** (G4KLX) | History, from git log [documented: commits]:<br>• Nov 2018: first detection (Kenwood/Icom signatures).<br>• Jan 2019: mini-header plus guard check.<br>• Nov–Dec 2020: per-frame detection by Tim Stewart, PR #667.<br>• **Feb 27 2025: removed** ("mangling D-Star DTMF data").<br>• Mar 2025: re-added.<br>• **Apr 24 2025: switched off (`#ifdef notdef`)**.<br>• **Jan 2026: re-enabled with lookback by F4FXL**, merged as PR #852 on Feb 26 2026.<br>Current code: on 0x80–0x9C it enters `DATA` state, skips FEC regeneration, DTMF blanking and silence insertion, and forwards to network and RF. Returns to audio on any non-8x/9x block. **Pass-through depends on the build date.** Pi-Star/WPSD images from Apr 2025 to Feb 2026 will corrupt fast data [inferred]. Dec 2025: about 16% BER on D-RATS fast data through WPSD, before the fix [community report: #470]. |
| **MMDVM / MMDVM_HS firmware** | The modem transmits whatever 12-byte frames the host gives it (`CDStarTX::writeData` copies 12 bytes) [documented: source]. On RX it rewrites only the sync bytes. MMDVM_HS PR #135 (merged Dec 31 2020) and MMDVM PR #306 changed EOT detection to match all **48 bits**: some fast-data images had falsely triggered end-of-transmission with 32-bit matching [community report/documented: PR]. Older firmware may cut transmissions short. |
| **ircDDBGateway** (G4KLX) | CHANGES "2018xxxx": "Add audio bypass processing for fast data mode". Checks `(byte ^ 0x70) & 0xF0` for 0x80/0x90 and then skips DTMF decode/blanking and silence counting [documented: source]. It forwards frames to DPlus/DExtra/DCS/G2 unchanged [inferred]. |
| **DStarGateway** (F4FXL) | Same logic as ircDDBGateway (`m_fastData`) [documented: source]. |
| **DStarRepeater** (G4KLX) | No fast-data detection found. It has DTMF blanking (`DTMF_MASK`/`DTMF_SIG`) that could corrupt data matching the pattern [inferred: source]. |
| **xlxd** | D-STAR→D-STAR frames are relayed as received. Slow data is synthesised only for cross-mode (transcoded) AMBE+ output [inferred: source `ccodecstream.cpp`]. A transcoded module turns fast data into garbage audio on DMR/YSF [inferred]. |
| **urfd** | Inserts a sync marker only when the source is not D-STAR [documented: source]. Behaviour on transcoded modules **[unknown]**. |
| **REF/DCS/XRF protocols** | Carry 9+3 bytes per frame with no AMBE processing in the protocol itself [inferred]. "D-Star Picture Net on REF072D" is cited in the MMDVMHost issue [community report]. No explicit end-to-end fast-data-through-reflector test found **[unknown]**. Network packet loss inserts the silence pattern and rewrites mini-headers per JARL §6.6, which corrupts a block [documented]. |
| **Icom ID-RP2C / RS-RP3C / ID-RP4000** | **[unknown]**. No Icom statement found. |

---

## 2. Per-radio matrix

**Icom's own compatibility list.** The latest copy is in the ID-5200 Advanced Manual, "As of August 2026" [documented]:

> IC-705, IC-905, IC-9700, ID-31A/E PLUS, ID-4100A/E, ID-5100A/E*, ID-5200A/E, ID-50A/E, ID-51A/E (PLUS, PLUS2, 50th Anniversary), ID-52A/E, ID-52A/E PLUS
>
> \* "Usable only when firmware versions CPU M 1.10, S 1.00, C 1.10, and DSP 1.10 or later are installed."

Models **not** on any list: IC-7100, IC-9100, IC-R30, ID-31A/E (non-PLUS), ID-51A/E (original), ID-880H, ID-800H, IC-2820H, IC-92AD, IC-80AD, IC-E2820 and others. They support slow data only.
- The IC-7100 basic manual has zero "fast data" mentions [documented].
- The IC-R30 advanced manual has no DV-data output at all [documented by absence].
- "Original ID-51 ignores it" [community report: TH-D74 groups.io thread].

**Common Icom set-mode items.** These are identical in every manual from the ID-51 PLUS onward [documented]:

- `DV Set (or DV/DD Set) > DV Data TX`: **PTT / Auto** (default Auto). Auto means "When data is input from a PC … the transceiver automatically transmits it".
- `DV Fast Data > Fast Data`: OFF / ON (default OFF). OFF ≈ 950 bps, ON ≈ 3480 bps.
- `DV Fast Data > GPS Data Speed`: **Slow / Fast** (default Slow). Use Slow when sending to radios that receive only slow speed data.
- `DV Fast Data > TX Delay (PTT)`: **OFF, 1–10 s** (default 2 s). "After releasing [PTT], the transceiver sends data using the DV Fast Data mode for this set period. When the TX data is completely sent within this set period, the transceiver automatically returns to receive." "Usable only when 'DV Data TX' is set to 'PTT'."
  - This corrects the background note: it is **not** a batching delay. It is a post-PTT fast-data tail for data typed during a voice over.
- PC app settings: 8N1, **Flow control: Xon/Xoff**. Baud is "your desired speed" on USB CDC ports and **4800/9600** on DATA-jack radios ("Data Speed").
- "Before transmitting the data, the transceiver sends a carrier sense signal approximately 500 milliseconds."
- "Only ASCII code can be used for data communication." The ID-51 PLUS manual says "for **low-speed** data communication".

**CI-V.**
- Command **0x22 sub 0x02/0x03/0x04/0x05** = DV Data TX / **Fast Data (00 = OFF, 01 = ON)** / GPS Data Speed / TX Delay (PTT, 00–10). Present in IC-705, IC-905, IC-9700, ID-52, ID-52 PLUS, ID-50, ID-5200, ID-51 PLUS, ID-51 PLUS2, ID-31 PLUS and ID-4100 [documented: CI-V guides/manuals].
- The HF/all-mode rigs also expose the same items under **1A 05**:
  - **IC-705**: 1A 05 **0092** DV Data TX, **0093** Fast Data, **0094** GPS Data Speed, **0095** TX Delay (PTT).
  - **IC-9700**: 1A 05 **0076 / 0077 / 0078 / 0079**.
  - **IC-905**: 1A 05 **0097 / 0098 / 0099 / 0100**.
  - [documented: CI-V reference guides, current editions; numbering may shift with firmware]

| Radio | Fast Data | Firmware note | Menu path | PC data interface (DV Data port) | Bluetooth DV Data | CI-V Fast Data |
|---|---|---|---|---|---|---|
| **IC-705** | Yes [doc] | From launch (list "As of July 2020"). v1.40 (Aug 21 2025): beep sounds after a Fast Data TX ends [doc] | SET > DV SET > DV Fast Data > Fast Data | USB "Serial Port B" with `SET > Connectors > USB (B) Function = DV Data`, GPS Out OFF. Baud "your desired speed" [doc] | Yes, built in: `Bluetooth Set > Data Device Set > Serialport Function = DV Data` (SPP) [doc] | 22 03; 1A 05 0093 |
| **IC-905** | Yes [doc] | From launch (Mar 2023 list) | SET > DV/DD Set > DV Fast Data | USB (B) Function = DV Data [doc] | None in manual (no BT mentions) [doc by absence] | 22 03; 1A 05 0098 |
| **IC-9700** | Yes [doc] | From launch (Mar 2019 list). v1.50 (Jul 9 2021): end-of-Fast-Data beep and Share Pictures improvements [doc] | MENU > SET > DV/DD SET > DV Fast Data | USB (B) Function = DV Data **or** [DATA] jack (DATA Function = DV Data, OPC-2350LU/OPC-2218LU). "DV Data/GPS Out Baud Rate" 4800/9600, default 9600 [doc]. Some users report USB-only setups not passing data [community: IC-9700Users groups.io] | No BT | 22 03; 1A 05 0077 |
| **ID-52A/E** | Yes [doc] | Listed Nov 2021 manual | [MENU] > SET > DV Set > DV Fast Data > Fast Data | USB: `USB Connect = Serialport`, `USB Serialport Function = DV Data`, GPS Out (USB) OFF [doc] | Yes, built in, SPP Data Device Serialport Function = DV Data. With BLE (iOS) a priority setting chooses between Fast Data and headset audio [doc] | 22 03 |
| **ID-52A/E PLUS** | Yes [doc] | List Jan 2025 | as ID-52 | as ID-52 (USB-C) [doc] | as ID-52, plus Android BT terminal mode [doc] | 22 03 |
| **ID-50A/E** | Yes [doc] | List Jul 2023 | as ID-52 | USB Serialport Function = DV Data [doc] | No BT | 22 03 |
| **ID-5200A/E** (2025–26) | Yes [doc] | List Aug 2026 | [MENU] > SET > DV Set > DV Fast Data | USB-C ("desired speed") or [DATA] jack 4800/9600 (OPC-2350LU) [doc] | Yes, SPP Serialport Function = DV Data [doc]. Also has a KISS TNC mode (CI-V 1A 00) [doc] | 22 03 |
| **ID-4100A/E** | Yes [doc] | List Apr 2017. **V1.02 (Nov 6 2019)**: "Fixed an issue where data loss may occur … DV Fast Data mode" [doc] | DV Set > DV Fast Data > Fast Data | [DATA] jack, OPC-2350LU/OPC-2218LU, Data Speed 4800/9600, Xon/Xoff [doc] | Optional **UT-137**, Serialport Function = DV Data [doc] | 22 03 [doc] |
| **ID-5100A/E** | Yes, **firmware-dependent** [doc] | **Release E2 (Oct 30 2014)** added it: CPU M 1.10, S 1.00, C 1.10, DSP 1.10. **E4 (Nov 6 2019)** data-loss fix (DSP 1.12) [doc] | DV Set > DV Fast Data | [DATA] jack (OPC-2350LU/OPC-1529R/OPC-2218LU), Data Speed 4800/9600 [doc] | Optional **UT-133** [doc]. DV-Data-over-BT option not verified [unknown] | not verified [unknown] |
| **ID-51A/E PLUS & 50th Anniv.** | Yes [doc] | "As of August 2014". **E1 (Jun 9 2015)**: "improves DSP processing of DV Fast Data" (DSP 1.00→1.01). **E2 (Nov 2019)**: data-loss fix. E3/E4: call-ending frame with GPS [doc] | MENU > DV Set > DV Fast Data > Fast Data | [DATA] jack OPC-2350LU/OPC-2218LU, 4800/9600, Xon/Xoff. Set `CI-V (DATA Jack)` OFF; ON only "when you make a data communication with a CI-V command" [doc] | No BT | 22 03 [doc] |
| **ID-51A/E PLUS2** | Yes [doc] | **E1 (Nov 6 2019)** data-loss fix. E3: fixed missing call-ending frame when sending D-PRS/NMEA [doc] | same | same as PLUS [doc] | No BT | 22 03 [doc] |
| **ID-31A/E PLUS** | Yes [doc] | List Nov 2017. **E1 (Nov 6 2019)** data-loss fix [doc] | MENU > DV Set > DV Fast Data | [DATA] jack, Data Speed 4800/9600 [doc] | No BT | 22 03 [doc] |
| IC-7100 | **No** [doc by absence] | n/a | n/a | USB2/DATA1 Function = DV Data (slow), 4800/9600 [doc] | optional (headset only) | n/a |
| IC-9100 | **No** [doc by absence] | n/a | n/a | slow data only | n/a | n/a |
| IC-R30 | **No** (receiver; no DV data output documented) | n/a | n/a | n/a | n/a | n/a |
| ID-31A/E, ID-51A/E (original), ID-880H, etc. | **No** | | | slow only | | |
| **Kenwood TH-D74A/E** | Yes, "DV fast data mode … sending data on unused voice frames" [doc: JVCKENWOOD 2016 press release]. Fw 1.07 (Dec 27 2017): "Fast Data communication may fail depending on the transmission timing" fixed [doc] | | `[F][MODE] > Data Mode` (Digital Function Menu). Menu 614 Data TX End Timing Off/0.5/1/1.5/2 s. Menu 618 Data Frame Output All / Related to DSQ / DATA Mode [doc: manual] | micro-USB | Yes | n/a |
| **Kenwood TH-D75A/E** | Yes: "Fast data communications can be performed by using the voice band for data communications" [doc: manual §16] | | Data Mode. **Menu 614** "Data TX End Timing … delay time until return from fast data TX to RX in accordance with the TX timing of the PC software" (Off/0.5/1/1.5/2 s). **Menu 984** Data Band (DV/DR) Interface = USB/Bluetooth. Menu 618 Data Frame Output [doc] | USB-C | Yes (Menu 984 = Bluetooth) | n/a |

### Behaviour questions

- **Same serial port for slow and fast data?** Yes. There is a single "DV Data" port function, and the Fast Data menu only changes the on-air format [documented: all manuals]. The port descriptions literally say "Inputs or outputs **low speed** data for the DV mode", yet the same port carries Fast Data [documented: IC-705/IC-9700 set mode].
- **Behaviour during voice:** the radio reverts to slow data while PTT is held [documented].
- **Received Fast Data on a slow-only radio:**
  - Icom: "DV Fast Data mode is not compatible with the DV mode low-speed data communication" [documented: Icom America ID-4100A page].
  - What such a radio actually outputs is **[unknown]**. Probably no serial output, since there is no 0x3X mini-header, plus muted or noisy audio with periodic beeps [inferred from JARL/patent].
  - Icom warns not to send pictures or GPS in Fast mode to slow-only radios [documented].
- **Picture sharing:**
  - Built-in Share Pictures (IC-705/IC-9700/ID-52 etc.) uses Fast Data when "<<TX All>>" is selected [documented].
  - RS-MS1A/RS-MS1I apps send pictures via Fast Data [documented: ID-4100/ID-51 PLUS manuals; Icom RS-MS1I page].
  - IC-9700: to transmit voice while an image goes out in Fast Data mode, set the MIC PTT pin to "PTT Input" [documented].
- **Maximum continuous data size, radio buffer size:** **[unknown]**. No Icom figure found. Icom says "Depending on the combination of your PC and your communication software, some data may be lost" [documented].
- **Auto-TX unkey timing in "Auto" mode:** **[unknown]** for Icom. TX Delay (PTT) applies only to PTT mode. Kenwood exposes this as Menu 614. MacRats reports that **Menu 614 = Off silently stops all auto-TX of PC data** and that 0.5 s works [community report: w9fyi/macrats README, 2026-04].
- **TH-D75 Menu 984:** the radio routes DV/DR data to exactly one of USB or Bluetooth. A mismatch means TX/RX silently fails [documented: manual menu list; failure mode from community report: macrats].

---

## 3. Measured throughput and known problems

- **No measured D-RATS file-transfer rates (bytes/s) over Fast Data vs slow data were found** in groups.io (pages require JS/login and could not be fetched), QRZ, reddit, D-RATS wiki, GitHub, or Icom literature **[unknown]**.
- Theoretical ceilings [inferred: arithmetic]:
  - Slow data: 10 blocks × 5 bytes = 50 B per 420 ms ≈ **119 B/s**, shared with GPS/D-PRS, messages and header resend.
  - Fast data: ≈ **435 B/s** at 3480 bps (≈ 495 B/s before beep overhead).
  - At 4800 baud the serial link (≈ 480 B/s) is barely above the Fast Data rate. Use 9600 or more.
  - Every transmission also pays ≈ 500 ms carrier sense, plus radio TX/RX turnaround.
- D-RATS defaults [documented: D-Rats source/wiki]: `ddt_block_size` 512 (wiki: "512 for RADIO or TNC"), `ddt_block_outlimit` 4, `warmup_length` 16, `warmup_timeout` 0. A 4×512-byte burst is about 4.7 s of air time on Fast Data versus about 17 s on slow data [inferred].
- Anecdotes:
  - "on file transfers, well, it blows D-Rats away" (picture/Fast Data vs D-RATS slow) [community report, via search snippet].
  - D-RATS works between two TH-D74s and between a TH-D74 and newer Icoms "when all the rigs are in Fast Data Mode" [community report: TH-D74 groups.io].
  - "ID 5100's send Hi speed data … make certain to reset the 5100 to low speed when talking to older radios" [community report].
- Known problems:
  1. **Hotspot corruption** (section 1.7): MMDVMHost FEC regeneration and DTMF blanking; MMDVM EOT false-trigger in old firmware. Dec 2025: about 16% BER for D-RATS Fast Data via WPSD [community report: #470].
  2. **Icom firmware bug, fixed Nov/Dec 2019:** ID-51 (and others) on E3 switched into Fast Data when sending GPS (DV-A) and omitted the end-of-transmission frame. This caused about 3 s receiver hang and garbled audio on MMDVM. Fix: Icom firmware of Nov 6 / Dec 5 2019, or GPS TX off [community report: F4FXL blog; documented: Icom release notes].
  3. **Data loss in Fast Data mode** fixed on ID-5100 E4, ID-4100 V1.02, ID-51 PLUS E2, ID-51 PLUS2 E1, ID-31 PLUS E1 (all dated Nov 6 2019) [documented]. Radios on older firmware should be updated.
  4. D-RATS wiki ID-51 PLUS recipe [documented: D-Rats wiki]: GPS Select / GPS TX Mode / GPS Auto TX / GPS Out = OFF, DV Data TX = Auto, Fast Data ON, CI-V (Data Jack) OFF, 9600 baud. Note: "Fast Data only works with another ID-51A Plus or ID-5100A with firmware update".
  5. IC-705 on Raspberry Pi: checksum errors until the D-RATS port speed matched the radio. Windows users report 9600 baud on the *second* virtual COM port [community report: d-rats groups.io].
  6. Kenwood TH-D74 "does not support slow DV data" (TX). Users could receive D-RATS slow data but not transmit [community report: Kenwood reply quoted in MMDVMHost #470; TH-D74 groups.io]. **A D-RATS-compatible app that must also talk to TH-D7x radios therefore needs Fast Data on the Icom side.**

---

## 4. Serial port transparency, flow control, filtered bytes

- **Flow control:** Icom manuals specify **Xon/Xoff** for the DV Data port on every model checked (IC-705, IC-905, IC-9700, ID-52, ID-52 PLUS, ID-50, ID-5200, ID-4100, ID-51 PLUS/PLUS2, ID-31 PLUS) [documented]. No RTS/CTS is documented. 0x11 and 0x13 must therefore be treated as reserved on the PC↔radio link in both directions [inferred].
- **On-air reserved bytes for simple data (0x3X)** [documented: JARL §6.2]: **0x00, 0x11 (XON), 0x13 (XOFF), 0x76, 0x84, 0xE7, 0xFE**.
  - Note: the packet-loss rule fires on the data-frame sequence `E7 84 76` combined with voice silence `9E 8D 32 88 26 1A 3F 61 E8`.
  - Packet-loss rewriting (§6.6) can inject "80", "80 00" or "80 00 00" into simple data.
- **"Only ASCII code can be used for data communication"** [documented: all Icom manuals]. The older ID-51 PLUS text restricts this to "low-speed" data.
- **Fast Data on-air framing** has an explicit length (0x80 + n) and a guard byte designed against false packet-loss detection [documented]. On air it is therefore *designed* to be binary-capable.
- **But** the PC port still uses Xon/Xoff, and Icom still says ASCII. Whether the radio passes 0x00/0x11/0x13/0xFD–0xFF transparently in Fast Data mode is **[unknown]**. Keep D-RATS-style escaping.
- **D-RATS yEnc banned set** [documented: d_rats/yencode.py]: `DEFAULT_BANNED = 11 13 1A 00 84 E7 FD FE FF C0 DB`, offset 64.
  - The comments list 0x76 but it is *not* in the banned set.
  - Escaping a KISS FEND/FESC and 0xFD/0xFF covers CI-V framing risk.
  - A compatible app must use the identical set to interoperate.
- **CI-V binary-safe DV data path** [documented: IC-705/IC-9700/IC-905/ID-52/ID-50/ID-5200/ID-51 PLUS CI-V guides]:
  - `22 00` "Set the DV TX data (Up to 30 byte)".
  - `22 01 00` Auto DV RX data output on/off.
  - `22 01 01` DV RX data for transceive.
  - Bytes **0xFA–0xFF are escaped as `FF 0A`–`FF 0F`** (up to 60 bytes on the wire).
  - This is a framed, binary-capable alternative to the raw DV Data stream.
  - Whether CI-V-injected DV data goes out as Fast Data when Fast Data = ON is **[unknown]**. Must be tested.

---

## 5. GPS, auto-reply and Auto-TX interactions (Icom statements)

- GPS Data Speed = Slow by default [documented]. With Fast Data ON and GPS Data Speed = Fast, D-PRS/NMEA is sent in Fast format, which slow-only receivers cannot decode. Icom explicitly tells users to choose Slow for mixed fleets [documented].
- D-RATS guidance is to turn GPS TX off when running data [documented: D-Rats wiki; Icom manuals tell you to set GPS Out OFF and GPS Select not "External GPS" on the shared port].
- The 2019 GPS/Fast Data EOT bug shows that GPS beacons and Fast Data interact in firmware [community report + documented fix].
- Auto Reply has options ON / Voice / Position [documented]. No statement found on how it interacts with Fast Data **[unknown]**.
- DV Data TX Auto: the radio transmits automatically on serial input, preceded by about 500 ms carrier sense [documented]. No documented TX hold or idle timeout for Auto mode. The TOT presumably applies **[unknown]**.

---

## 6. Kenwood TH-D74 / TH-D75 compatibility

- **On-air compatibility with Icom:**
  - "The TH-D74A's fast data is compatible with Icom's fast data" (Kenwood answer relayed by Tim Stewart, KG4BXH) [community report: MMDVMHost #470].
  - D-RATS reported working TH-D74 ↔ new Icoms in Fast Data [community report].
  - Kenwood's manual uses the same JARL term, and JARL ch. 7 is public [documented].
  - Kenwood's lead-in frames differ from Icom's (section 1.6) [community report].
  - I treat it as interoperable, with moderate confidence.
- **TH-D75 DV data port** is a **raw byte stream** in Data Mode, like Icom's. MacRats's USB transport opens it raw 8N1, default 9600, writes DDT2/yEnc bytes directly [documented: macrats source `USBSerialTransport.swift`].
  - The MacRats README contradicts itself. Its config table says "Menu 650 = Off (NOT Reflector TERM Mode)", but its prose says it speaks MMDVM at 38400 in terminal mode. The source supports a raw 9600 port for Data Mode [inferred].
- **TH-D75 Reflector Terminal Mode** (Menu 650): "D-STAR operation is available over the Internet instead of using Amateur Radio waves", so there is **no RF** [documented]. Third parties report it speaks the **G4KLX MMDVM host↔modem protocol at 38400** [community report: macrats README].
- Kenwood's own firmware notes report a fixed Fast Data timing bug (TH-D74 fw 1.07, Dec 2017) [documented].
- Slow-data TX on the TH-D74 is reportedly unsupported [community report].
- TH-D75 slow-data TX behaviour **[unknown]**.

---

## 7. Frame-level access: Icom Terminal / Access Point mode and MMDVM

- **Icom Terminal/Access Point serial protocol** is not publicly documented by Icom [documented by absence]. It **has been reverse-engineered and implemented in open source**:
  - **G4KLX DStarRepeater `IcomController`** (CHANGELOG: "Add Icom Terminal and Access Point modes"): 38400 baud.
  - **dimant/dstardocs `IcomTerminalMode.md`** (ID-52): 38400 8N1, no handshake.
  - **manuel-alcocer/dstar705 / qdstar** (IC-705 over USB or WiFi; code ported from DStarRepeater/ircDDBGateway).
  - [documented: source/docs]
- **Packets** [documented: dimant doc + IcomController source]:

  | Direction | Packet | Contents |
  |---|---|---|
  | both | `[len][type]…[0xFF]` | general form |
  | PC→radio | 0x02 | ping |
  | radio→PC | 0x03 | pong |
  | radio→PC | 0x10 | header: flags, RPT1/RPT2/UR/MY/suffix, CRC |
  | radio→PC | 0x12 | frame: seq, `frame# \| 0x40 = end`, **9 AMBE bytes + 3 slow-data bytes** |
  | PC→radio | 0x20 | header |
  | PC→radio | 0x22 | frame (same 12-byte payload) |
  | radio→PC | 0x21 / 0x23 | acks |

  The PC must wait for an ack per chunk. The radio sends to the PC without flow control.
- **So software gets full 12-byte DV-frame access**, enough to build JARL ch. 7 Fast Data blocks itself: mini-header 0x80 + n, guard byte, mitigation 0x02, per-frame scrambling, periodic beep blocks.
  - **Terminal Mode:** "the transceiver does not transmit or receive RF signals through the antenna" [documented: ID-52 PLUS Terminal Mode Guide]. Useless for RF data.
  - **Access Point Mode:** the radio relays between RF and the PC application. This is the RF path. It also requires the app to act as a gateway (RS-MS3W/RS-MS3A role).
  - Whether the radio passes PC-supplied fast-data voice frames to RF **bit-exact** (no AMBE re-encode, no DTMF processing), and whether it delivers RF-received Fast Data frames to the PC unmodified, is **[unknown]**. Must be bench-tested. Icom's own DV Fast Data works through RS-MS1A remote control, which is a different path [documented].
  - Supported on ID-4100, ID-5100, ID-51 PLUS2, ID-31 PLUS, ID-52(+), ID-50, IC-705, IC-9700, IC-905, ID-5200 [documented: Icom guide model lists]. The exact list varies by guide.
- **MMDVM hotspots:**
  - The modem transmits whatever 12-byte frames the host writes: `MMDVM_DSTAR_HEADER 0x10`, `MMDVM_DSTAR_DATA 0x11`, `MMDVM_DSTAR_EOT 0x13` [documented: MMDVM source].
  - So a custom host **can transmit Fast-Data-formatted frames** [inferred, high confidence].
  - Requirements: firmware with the 48-bit EOT fix (MMDVM PR #306 / MMDVM_HS PR #135, Dec 2020). With stock MMDVMHost, a build from Feb 2026 or later (PR #852), else FEC regeneration corrupts the data.
  - Through stock MMDVMHost, network-originated Fast Data is passed to RF without FEC regeneration in `DATA` state (current master) [documented: source].
  - Kenwood TH-D75 Reflector Terminal Mode reportedly speaks the same MMDVM protocol but has no RF [community report + doc].
- **Architecture implication:** a D-RATS-compatible app could use three transports:
  - (a) the radio's DV Data byte stream: simplest, Icom-supported, Xon/Xoff, ASCII-only caveat;
  - (b) CI-V `22 00` / `22 01` framed DV data with FA–FF escaping;
  - (c) frame-level Access Point / MMDVM, where the app does its own JARL ch. 7 framing.

  (c) gives the most control but depends on the unverified Access Point pass-through and on hotspot software versions.

---

## 8. Biggest unknowns, to settle by bench test

1. Actual D-RATS throughput over Fast Data versus slow data, and radio buffer limits and maximum burst size.
2. Byte transparency of the DV Data port in Fast Data mode (0x00/0x11/0x13/0xFD–0xFF). Icom still states Xon/Xoff and "ASCII only".
3. Whether CI-V `22 00` data is sent as Fast Data.
4. The Auto-mode unkey/idle timeout on Icom.
5. Whether Icom Access Point mode passes PC-built Fast Data frames bit-exact.
6. Behaviour of Icom ID-RP2C/RS-RP3C repeaters and of transcoded XLX/URF modules.
7. What slow-only radios output on serial and audio when receiving Fast Data.
8. Whether the RF header flag changes (probably not).

---

## URLs

Primary / documented
- JARL D-STAR standard (Ver. 6.0a, Fast Data ch. 7): https://www.jarl.org/English/7_dstar/STD6_0_E.pdf
- Icom patent US9961634B2 (Fast Data mute control): https://patents.google.com/patent/US9961634
- Icom Japan manual downloads (search page): https://www.icomjapan.com/support/manual/
  - ID-5200 Advanced Manual (compat list "As of August 2026"): https://www.icomjapan.com/support/manual/4735/
  - ID-52 Advanced Manual: https://www.icomjapan.com/support/manual/3412/
  - ID-52 CI-V Reference: https://www.icomjapan.com/support/manual/3413/
  - ID-52 PLUS Advanced: https://www.icomjapan.com/support/manual/4162/
  - ID-52 PLUS CI-V: https://www.icomjapan.com/support/manual/4163/
  - ID-52 PLUS Terminal Mode Guide (Android): https://www.icomjapan.com/support/manual/4522/
  - ID-50 Advanced: https://www.icomjapan.com/support/manual/3886/
  - ID-50 CI-V: https://www.icomjapan.com/support/manual/3889/
  - ID-5200 CI-V: https://www.icomjapan.com/support/manual/4686/
  - ID-4100 Advanced: https://www.icomjapan.com/support/manual/1634/
  - ID-51 PLUS Advanced: https://www.icomjapan.com/support/manual/2089/
  - ID-51 PLUS2 Advanced: https://www.icomjapan.com/support/manual/2095/
  - ID-31 PLUS Advanced: https://www.icomjapan.com/support/manual/1735/
  - IC-705 Basic: https://www.icomjapan.com/support/manual/3060/
  - IC-705 CI-V: https://www.icomjapan.com/support/manual/3064/
  - IC-9700 Basic: https://www.icomjapan.com/support/manual/2156/
  - IC-9700 CI-V: https://www.icomjapan.com/support/manual/2161/
  - IC-905 CI-V: https://www.icomjapan.com/support/manual/3792/
  - ID-5100 Basic: https://www.icomjapan.com/support/manual/1524/
  - IC-7100 Basic: https://www.icomjapan.com/support/manual/2289/
- Advanced manuals mirror: https://rigpix.com/icom/ic705_advanced.pdf, https://rigpix.com/icom/ic9700_advanced.pdf, https://rigpix.com/icom/ic905_advanced.pdf, https://rigpix.com/icom/icr30_advanced.pdf
- Icom America firmware notes:
  - ID-5100 E2 (Fast Data added): https://icomamerica.com/support/firmware_driver/1964
  - ID-5100 E4: https://icomamerica.com/support/firmware_driver/2433
  - ID-51 PLUS E1 (2015): https://www.icomamerica.com/support/firmware_driver/2447/
  - Related Nov 2019 fixes: https://www.icomamerica.com/support/firmware_driver/2448/, https://www.icomamerica.com/support/firmware_driver/2455/, https://www.icomamerica.com/support/firmware_driver/2437/, https://www.icomamerica.com/support/firmware_driver/2463/
  - IC-9700 v1.50: https://icomamerica.com/support/firmware_driver/4407/
  - IC-705 v1.40: https://icomamerica.com/support/firmware_driver/4478/
- Icom America ID-4100A page ("not compatible with low-speed data"): https://www.icomamerica.com/lineup/products/ID-4100A/
- Kenwood TH-D75 User Manual: https://rigpix.com/kenwood/thd75ae_manual.pdf
- Kenwood TH-D74 manual: https://rigpix.com/kenwood/thd74ae_manual.pdf
- Kenwood TH-D74 firmware history: https://www.kenwood.com/i/products/info/amateur/thd74_update_e.html
- JVCKENWOOD TH-D74 press release: https://www.jvckenwood.com/en/press/2016/08/press_160808.html

Source code
- MMDVMHost (DStarControl.cpp, DStarDefines.h; commits 18398ef, a9b7179, 317bda0, 8874d12, 351c119, 78fc96b, 8e0705e, d3ed3ce, 876f58c; PR #667, #852): https://github.com/g4klx/MMDVMHost
- MMDVM firmware: https://github.com/g4klx/MMDVM
- MMDVM_HS PR #135: https://github.com/juribeparada/MMDVM_HS/pull/135
- ircDDBGateway: https://github.com/g4klx/ircDDBGateway
- DStarGateway: https://github.com/F4FXL/DStarGateway
- DStarRepeater (IcomController): https://github.com/g4klx/DStarRepeater
- xlxd: https://github.com/LX3JL/xlxd
- urfd: https://github.com/nostar/urfd
- D-Rats (yencode.py, config_defaults.py) and wiki: https://github.com/ham-radio-software/D-Rats, https://github.com/ham-radio-software/D-Rats/wiki/030-Connecting-D%E2%80%90Rats-with-a-D%E2%80%90Star-radio
- Icom Terminal Mode notes: https://github.com/dimant/dstardocs/blob/main/IcomTerminalMode.md
- dstar705 / qdstar: https://github.com/manuel-alcocer/dstar705, https://github.com/manuel-alcocer/qdstar
- MacRats (TH-D75): https://github.com/w9fyi/macrats

Community
- MMDVMHost issue #470 "Support for D-Star DV Fast Data": https://github.com/g4klx/MMDVMHost/issues/470
- F4FXL, buggy ID-51/ID-5100 firmware (GPS → Fast Data, missing EOT): https://www.f4fxl.org/buggy-icom-id-51-and-id-5100firmware/
- TH-D74 groups.io "anyone got low speed data": https://kenwood-radios.groups.io/g/TH-D74/topic/anyone_got_low_speed_data/44340951
- d-rats groups.io threads: https://groups.io/g/d-rats/topic/d_rats_via_rf_experience/101334675, https://groups.io/g/d-rats/topic/d_rats_ic_705_raspberry_pi/80762270, https://groups.io/g/d-rats/topic/drats_ic_9700/78072818
- IC-9700Users "working with D-RATS": https://groups.io/g/IC-9700Users/topic/working_with_d_rats/74157438
- W5AW D-RATS notes: https://www.qsl.net/w5aw/d-rats.htm
- klop.solutions IC-705 terminal mode: https://klop.solutions/terminal-mode-with-icom-ic-705-and-pi-star/
