# Bench results

Results of the tests in [bench-tests.md](bench-tests.md). Validation level: **Hardware** (two radios on simplex, low power, a few metres apart). Not yet through a repeater or against D-RATS.

## 2026-11-03/04: IC-9700 and ID-52 PLUS on simplex (KK4ODA)

Setup: IC-9700 on a mini PC (CI-V on USB A through a VSPE splitter, nothing else on the second port; DV data on USB B). ID-52 PLUS on a second PC, USB DV data only. Engines 0.2.1–0.2.4.

| Test | Question | Result |
|---|---|---|
| 1 (HW-R3) | Does a radio send data while receiving voice? | **ID-52 PLUS waits.** It held a 50-byte chat 16.6 s, until the IC-9700's voice ended, then sent it. Reverse direction not yet run. |
| 2 (HW-R4) | How fast does the IC-9700 report voice over CI-V? | **Within 0.2–0.4 s** of key-up and release, operator click delay included; 0.25 s of it is the status poll interval. A chat sent during voice was held and went out 1.2 s after the voice ended (simplex holdoff). |
| 5 (HW-R10) | Key-up and air time | See below. |

### Air time (TX on to TX off, IC-9700 over CI-V)

| Bytes | 39–50 | 95 | 120 | 179 |
|---|---|---|---|---|
| Seconds on air | 0.99–1.12 | 1.42 | 1.67 | 2.13 |

Fit: **0.67 s + 1 s per 122 bytes**. The engine's profile uses these figures since this test (it estimated 1.1 s + 1 s per 119 bytes before).

### Key-up (data handed to the radio until TX on)

- **IC-9700: 0.07–3.44 s, mean 1.5 s,** on a quiet channel too, unrelated to size. The radio's XOFF on the data port arrives at the same moment as the CI-V TX-on reading (within 0.03 s), so the delay is real, not a CI-V or VSPE timing artefact. Cause unknown; to be retested with CI-V polling off and on the faster PC.
- **ID-52 PLUS: about 0.3 s** every time, judged from when the IC-9700 sensed it (that includes up to 0.25 s of CI-V polling; both PCs' clocks assumed in sync).

### Other findings

- **Flow control.** Both radios send **XOFF (0x13) then XON (0x11)** on the data port for blocks of about 95 bytes or more: the ID-52 PLUS about 50 ms into the write, the IC-9700 when it starts transmitting. XOFF lasted 0.3–1.0 s. Nothing was lost at these sizes, but the engine now honours them by default (`flow = "xonxoff"`): it writes in 32-byte pieces, pauses on XOFF, and drops the two bytes from received data.
- **Data reported as voice.** The IC-9700's CI-V receive status shows another station's *data* as voice. Without a fix, a repeater path would wait the 10 s voice gap after every data over. The scheduler now treats a "voice" period in which D-RATS frames arrived as data.
- **Caller reports.** The IC-9700 reports the caller (`20 00`) about 0.5 s after a transmission starts and again about 1.6 s after it ends.
- **CI-V call-sign reads** work in VFO mode: UR `CQCQCQ`, R1 `W4DOC  D`, R2 `W4DOC  G` read correctly (first part of HW-R1).
- **Files.** A 2 kB text file (180 bytes compressed) crossed in 8 overs, about 25 s each way, with no retransmissions. Throughput still needs a less compressible file.
