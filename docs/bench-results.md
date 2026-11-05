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

## 2026-11-05: Tests 5b and 5c (0.3.0)

Same setup. 5b repeats Test 5 with flow control on and the new chat screen; 5c sends three chats from the IC-9700 with CI-V turned off.

### The IC-9700 takes data only when it is ready to transmit

The summary's new **write s** column (data handed to the OS until the write returns) matches **keyup s** on the IC-9700: the USB write itself waits until the radio keys up. A slow PC can't explain this (47 bytes take milliseconds); the radio holds off its USB input.

| Radio | write s |
|---|---|
| IC-9700, CI-V polling on (5b) | 0.05–6.5 s; quiet-channel chats 4.45, 5.68, 6.52 |
| IC-9700, CI-V off (5c) | 0.11, 1.71, 1.34 |
| ID-52 PLUS (no CI-V) | 0.06–0.84, mostly ~0.2 |

So CI-V polling (status 4×/s, TX state 10×/s during tests) roughly triples the IC-9700's delay, and the radio still takes up to ~2 s by itself. Since 0.3.1 the engine stops all CI-V polling while a burst is being written.

### Other findings

- **Split writes became split overs.** With 0.2.5–0.3.0 writing in 32-byte pieces, the IC-9700 transmitted what it had and took nothing more until the over ended. A 179-byte burst reached the ID-52 PLUS as 32 + 32 + 83 bytes over about 12 s. Each burst now goes to the driver in one write; XOFF holds the next burst back.
- **Retry timers counted from hand-off.** The file offer was written 6.5 s late, so its retry fired while the first copy was barely on the air, and the ID-52 PLUS answered twice. Transfer timers now move by the write delay.
- **Flow control** worked on both radios (XOFF/XON 0.4–1.4 s apart, writes paused).
- **Air time** matched the new estimate within about 0.15 s on average.
