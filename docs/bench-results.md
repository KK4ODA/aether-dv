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

## 2026-11-05: Test 5b repeated (0.3.1) and the first large file

**5b with 0.3.1:** each block went out in one transmission (745 bytes sent and received on both sides), with no duplicate offers. Each file took 26–29 s each way, against 65 s with 0.3.0. With CI-V paused while writing, the IC-9700's quiet-channel write times were 1.40, 0.52 and 0.87 s, as fast as with CI-V off. Its slowest writes (2.3–3.7 s) all came 2–3 s after it had received the other station; the radio seems to need that long after a reception. Air-time estimate within 0.05 s on average.

**Large file (20,000 random bytes, IC-9700 → ID-52 PLUS): stuck at 0 blocks.** Each ARQ burst was about 1,260 bytes (4 × 256-byte blocks plus REQACK), written in one go. The IC-9700 transmitted only about 260 bytes of each (2.2 s on air against 11 s estimated). The ID-52 PLUS received every block cut short (bad checksums) and answered `ACK []` 25 times. The IC-9700's flow control during the write: XOFF after about 100 bytes, then everything arriving before the next XON was dropped (99 + 99 + 42 bytes got through). So the radio keeps roughly one over's worth (writes up to 179 bytes always went out whole) and loses the rest.

Since 0.3.2, DV slow data is written in pieces of at most 180 bytes of whole frames, each left to go on the air before the next. File blocks are 120 bytes (about 170 on the wire), so each block fits one piece. A frame larger than a piece is written in XOFF-checked 32-byte chunks: split over several transmissions, but nothing lost. The cost is one transmission per block: about 25–30 bytes per second on DV slow data, measured over a simulated link.

## 2026-11-05: Large file again (0.3.2): delivered, but slowly

5,000 random bytes IC-9700 → ID-52 PLUS: **received**, in about 7 minutes (~12 B/s). Power was raised mid-test on both radios (IC-9700 at 129.8 s, ID-52 PLUS to Mid).

- **Pieces worked:** the ID-52 PLUS received each piece as one over of about 160 bytes, with no truncated frames from oversized writes.
- **Why so slow: retries fired during long writes.** A 4-block burst now takes 12–20 s to write, over by over. The retry timer kept running from hand-off and expired before the last piece was on the air. The IC-9700 then sent an acknowledgement request at the same moment the ID-52 PLUS answered: 52.2 s and 70.5 s on the two clocks, which are 0.1 s apart once aligned. The answer was lost, followed by resends (9,835 bytes sent for ~7,200 needed) and gaps of up to 20 s. Moving the timers after the write (0.3.1) was too late. Since 0.3.3 the transfer timers are held from hand-off until the whole burst has been written.
- The summary's key-up for these bursts was not meaningful (CI-V is quiet during the whole write). It now shows the number of overs and leaves key-up blank for multi-over bursts.

## 2026-11-05: CI-V Transceive off; 8-block windows

**Transceive OFF** (the only IC-9700 setting changed after a full settings check): 5 kB in 322 s, against 328 s with it on. Per-window write times were unchanged: 15–38 s, against 16–45 s. Both runs were clean (7,464–7,467 bytes, no resends). The IC-9700's 1–3 s before it takes the next piece after each over looks built in. Transceive stays OFF: it does no harm, and the radio's announcements are unneeded load.

**Stale-ACK guard after a held write (fixed in 0.3.4).** Releasing the write hold shifted every sender timer by the whole write time, including the guard that ignores ACKs arriving too early. That guard already allowed for the burst's air time, so it was counted twice: the real ACK was ignored and the sender waited for its retry timer, about 12 s per window. On the simulated link a 3 kB file took 104–112 s with 4-block windows. With the fix it takes 92–93 s, and with 8-block windows (also 0.3.4) 81–82 s.

**`piece_bytes`** (new, per serial port, default 180) sets the most bytes per over. It's for experiments: at 220 the REQACK would join the last block of each window, but the radios haven't been shown to take 220-byte writes whole.

## 2026-11-05: 0.3.4 on the radios; 0.3.5 changes

**0.3.4, 5 kB IC-9700 → ID-52 PLUS: about 250 s (~20 B/s, against 322–328 s with 0.3.3).** No resends: 7,277 bytes sent and received. Each 8-block group took 9–10 transmissions and 28–46 s to write. Each new group started about 1.6 s after the answer. The ID-52 PLUS showed gaps of 1–7 s between pieces (each about 1.6 s on air), and sometimes several pieces in one transmission: 643 bytes in one 8.8 s over. The IC-9700 can chain pieces, but not reliably.

**0.3.5:**
- **CI-V quiet for the whole write.** The 10 s cap let polling resume during long writes, visible as CI-V readings mid-write; the cap is now 120 s.
- **Transmission size in the app**, per DV slow data port: 180 (tested, default) or 220 bytes, also `ports.setPieceBytes`. At 220 the acknowledgement and the request ride with a block: 8 transmissions per 8-block group instead of 10. Simulated link, 3 kB: 76 s against 81–82 s. On the radios, 220 is unproven; if they drop bytes at that size, the blocks are resent.
