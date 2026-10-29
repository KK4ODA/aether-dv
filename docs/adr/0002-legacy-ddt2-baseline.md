# ADR-0002: Legacy DDT2 is the mandatory baseline protocol

Status: **Accepted** 2026-10-29 (owner decision) · Proposed: 2026-10-29

## Context
The installed base speaks DDT2 (`02-legacy-protocol-analysis.md`). The maintainer will not change it (DR#315). Session ids are positional (chat = 1, RPC = 2).

## Decision
Implement DDT2 byte-exactly in a pure crate (`aether-ddt2`), driven by golden vectors. Preserve the wire quirks: little-endian file size, `~` padding, CRC-16/XMODEM, and positional ids. When sending, use the post-#302 banned-byte superset. Accept any valid escaping on receive.

## Consequences
Every Aether station is a full legacy D-RATS peer. Enhancements must layer on top (ADR-0003/0004). The codec crate has no I/O and no clock, and is fuzzed.

## Decision record (2026-10-29)
Accepted on the basis of brief priority 2 and owner answer Q4: **the interop gate targets D-RATS 0.4.x (0.4.2) only.** D-RATS 0.3.x (Python 2) is best-effort and is not part of the test gate.
