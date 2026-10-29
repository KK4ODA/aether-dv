# ADR-0003: Sender-local optimisations within the legacy wire format

Status: Proposed · Date: 2026-10-29

## Context
Source analysis shows that the legacy *receiver* accepts any window, block size, and magic byte (compressed or not) per frame. Pre-TX delays, RTO and REQACK timing are purely sender-side. The model (`tools/arq_model.py`) predicts that these parameters, not the wire format, dominate throughput, especially in Fast mode.

## Decision
Tune block size (64–1024 B), window (≤ 64), compression per frame, RTO from air-time-corrected RTT, and channel hold-off, all from the active ModeProfile and observed loss. Avoid `[SOB]/[EOB]` poison blocks.

## Gate
Each optimisation ships only after an interop test (level I) against unmodified D-RATS 0.4.2. If 0.3.x stations are confirmed in use, a 0.3.10 test is also required. If a legacy receiver misbehaves, that parameter is clamped to legacy defaults for legacy peers.
