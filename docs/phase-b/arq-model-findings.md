# Legacy ARQ: Model-Checking Findings (Phase B, B2)

Tool: `spec/models/legacy_arq_check.py`, a small explicit-state (breadth-first) model checker written for this project. The standard TLA+ tools could not be fetched in this environment. It models the D-RATS stateful-session sender and receiver (`d_rats/sessions/stateful.py@d0e7a39`) over a lossy FIFO channel and explores **every** reachable state for the given parameters. Sequence numbers are reduced modulo `MOD` (legacy uses 256) so that wrap-around is reachable. Timers are abstracted: a timeout may fire at any moment, which covers every possible delay.

Properties checked:
- **P1**: blocks are delivered in order, without duplicates, and with the right content.
- **P2**: no false acknowledgement. Every block the sender considers ACKed is actually held by the receiver.
- **P3**: when the sender considers the transfer complete, the receiver has every block.

Evidence level: **model** (Simulated). These are design-level defects found by exhaustive search of a faithful abstraction. Confirming them against running D-RATS is a Phase C interop task (see *Reproduction plan* below).

## Results

| Sender | Receiver | Parameters (MOD/blocks/window/channel) | States | Result |
|---|---|---|---|---|
| legacy | legacy | 4/6/2/4 | 4,360 | **P2 violated** (L1) |
| legacy | legacy | 8/10/2/4 | 67,591 | **P2 violated** (L1) |
| legacy | legacy | 6/9/3/4 | 21,121 | **P2 violated** (L1) |
| legacy, block 0 sent twice | legacy | 4/6/2/5 | 8,996 | **P2 violated**: redundancy doesn't remove the race |
| legacy | legacy | 4/4/2/4 (no wrap into a new cycle) | 10,996 | **P1 violated** (L2) |
| legacy | legacy | 8/8/3/5 | 685,736 | **P1 violated** (L2) |
| legacy | window-based dedup | several | — | P1/P2 violated: legacy sender span is unbounded (L3) |
| **span-limited (ALP rule)** | **window-based dedup** | 4/6/2/4 | 172,124 | **no violation** (exhaustive) |
| **span-limited** | **window-based** | 4/9/2/5 | 1,728,087 | **no violation** (exhaustive) |
| **span-limited** | **window-based** | 8/10/2/4 | 933,946 | **no violation** (exhaustive) |
| **span-limited** | **window-based** | 8/12/2/4 | 1,196,554 | **no violation** (exhaustive) |
| **span-limited** | **window-based** | 6/9/3/4 | 2,073,213 | **no violation** (exhaustive) |
| span-limited | window-based | 8/12/3/6, 8/18/4/7, 8/20/4/8 | >5,000,000 | inconclusive (state bound) |

Reproduce, for example:

```bash
python3 spec/models/legacy_arq_check.py --mod 4 --blocks 6 --window 2 --chan 4 --receiver legacy
```

### L1: false ACK after sequence wrap (silent loss)

Shortest counterexample (MOD=4; in D-RATS read seq 0 as the 257th block):

```
 1. TX burst [#0/s0, #1/s1] + REQACK          … cycle 0 delivered and ACKed normally …
 6. TX burst [#2/s2, #3/s3] + REQACK
10. RX ACK [2, 3]
11. TX burst [#4/s0, #5/s1] + REQACK          first blocks of the new cycle
12. LOSS of DAT #4/s0                          one frame lost
13. RX DAT #5/s1 DISCARDED as duplicate        receiver still holds old seq 1
14. RX REQACK [0, 1] -> TX ACK [0, 1]          answers from the OLD cycle's recv_list
15. RX ACK [0, 1]                              sender believes #4 and #5 delivered
```

Cause: the receiver forgets the previous cycle only when the new block 0 arrives after seq 255 (`stateful.py:397-401`). Until then, every new-cycle seq looks like an old duplicate and is acknowledged.

Impact on a real transfer:
- Any stateful transfer longer than 256 blocks is at risk: with D-RATS defaults (512 B blocks) that is >128 KiB of *compressed* data.
- One lost frame (the first block after each wrap, or its whole burst) is enough.
- The sender completes and reports success. The receiver's zlib/Adler-32 check fails, so the file is kept as `.part` and the transfer "fails" on one side only.
- Larger files are more realistic over ratflectors and Fast Data, so this matters more in Aether's target use than it did historically. It is a plausible contributor to D-Rats#315's "files stuck pending" (unconfirmed).

### L2: stale ACK resurrects a burst (duplicate delivery)

Counterexample (MOD=4, no new cycle needed):
1. A timeout re-poll REQACK is answered late.
2. That stale ACK clears `waiting_for_ack` after the next burst was sent.
3. The sender later retransmits a burst the receiver already has.
4. If the receiver has just completed a cycle (`iseq == MOD-1`), the duplicate old block 0 triggers the legacy reset and is **delivered again as new data**.

Cause: an ACK carries no identifier linking it to the REQACK it answers (`stateful.py:375-392`).

Impact:
- At the end of a file the extra block falls after the zlib stream end and is ignored (verified: Python `zlib.decompress` ignores trailing data).
- At a mid-transfer wrap the stream is corrupted. The receiver fails while the sender reports complete, as in L1.
- Needs a premature timeout plus a late ACK, which is plausible given the 12 s minimum RTO, slow-data rates and serial-write-based rate estimates.

### L3: legacy sender span is unbounded

The legacy window limits the *number* of outstanding blocks (2–8), not the distance between the oldest and newest. A block that keeps getting lost lets the span grow until the rollover pause, up to 255 sequence numbers. So no receiver can use sequence-number distance to tell old blocks from new ones in the legacy protocol. The only epoch signal is "seq 0 after 255", which is exactly what fails in L1 and L2.

## Consequences for Aether (normative for the specs)

1. **ALP (Aether↔Aether) rule, verified in the model:**
   - The sender never lets `newest − oldest_unacked` reach half the sequence space.
   - The receiver deduplicates relative to its in-order point.
   - With a 16-bit sequence and windows ≤64 the margin is huge, but the span limit must be enforced explicitly and not assumed from the window size.
   - ACKs echo the REQACK identifier, so stale ACKs are discarded. L2 is not covered by the window rule alone; this is a design rule for `state-machines.md` §ARQ-2, and the model will be extended to it.
2. **Aether sending to legacy D-RATS (cannot change the receiver):**
   - Prefer block sizes that keep a transfer within 256 blocks: up to 1024 B blocks gives 256 KiB compressed per session on good links.
   - For larger transfers the residual L1 risk remains. Sending the first post-wrap block redundantly lowers the probability but, as the model shows, cannot remove it.
   - Set the RTO from measured air time and never re-poll before the expected ACK time (reduces L2).
   - The UI must warn that delivery of large files to legacy stations is "unconfirmed" beyond the hop.
3. **Aether receiving from legacy D-RATS:** keep legacy semantics but add extra checks:
   - On reaching the end of a cycle, retain the previous cycle's block hashes.
   - Treat a post-cycle arrival whose content differs from the stored block of the same seq as new-cycle data, not a duplicate.
   - Flag possible L1/L2 events in diagnostics.
   
   This is a heuristic. It must be model-checked with content before adoption (open item).

## Reproduction plan against real D-RATS (Phase C, interop rig)

1. Transfer a 300 KiB incompressible file (more than 256 × 512 B blocks) between two D-RATS 0.4.2 instances over a PTY pair, with a fault injector that drops the first DDT2 data frame with seq 0 after a seq-255 frame.
2. Expected: the sender reports "Complete"; the receiver keeps `.part` with a zlib error.
3. Record the result in the validation matrix at level **Interoperability**.
