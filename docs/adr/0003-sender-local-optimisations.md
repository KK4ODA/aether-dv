# ADR-0003: Sender-local optimisations within the legacy wire format

Status: Proposed (amended with Phase B evidence) · Date: 2026-10-29

## Context
Source analysis shows that the legacy *receiver* accepts any window, block size, and magic byte (compressed or not) per frame. Pre-TX delays, RTO and REQACK timing are purely sender-side. The model (`tools/arq_model.py`) predicts that these parameters, not the wire format, dominate throughput, especially in Fast mode.

## Decision
Tune block size (64–1024 B), window (≤ 64), compression per frame, RTO from air-time-corrected RTT, and channel hold-off, all from the active ModeProfile and observed loss. Avoid `[SOB]/[EOB]` poison blocks.

## Gate
Each optimisation ships only after an interop test (level I) against unmodified D-RATS 0.4.2. If 0.3.x stations are confirmed in use, a 0.3.10 test is also required. If a legacy receiver misbehaves, that parameter is clamped to legacy defaults for legacy peers.

## Phase B amendment (2026-10-29)
The legacy ARQ model check (`../phase-b/arq-model-findings.md`) adds two sender-side rules for legacy peers:
- **Prefer block sizes that keep a transfer within 256 blocks.** Legacy receivers can false-ACK the first block of a new sequence cycle (P19). With blocks up to 1024 B this covers 256 KiB of compressed data per session. Larger transfers to legacy peers carry the `wrapWarning` flag in the API, and the UI states that arrival can't be fully confirmed.
- **Never re-poll before the RTO estimate.** This reduces stale-ACK duplicate bursts (P20).

The codec crate implements the compression part of this ADR: `PayloadEncoding::IfSmaller` sends small frames uncompressed. Remaining gate (unchanged): an interop test against D-RATS 0.4.2 for each optimisation. Status stays Proposed until those tests run in Phase C.
