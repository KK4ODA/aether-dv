# ADR-0004: Aether Link Protocol (ALP) rides inside DDT2; discovery via RPCGetVersion

Status: Proposed (amended with Phase B evidence) · Date: 2026-10-29

## Context
Needed for Aether↔Aether: end-to-end receipts, robust resumable transfers, 16-bit sequence/SACK, and priorities. Legacy ratflectors decode and re-encode DDT2 and drop anything else (`04` §3). An unknown control session type makes a legacy peer silently refuse, which costs the opener up to 10 × (5–15 s).

## Decision
- **Discovery**: the existing RPC job `RPCGetVersion` (answered by every D-RATS) returns extra dict keys from Aether stations (`aether`, `alp`, `rcpt`, `fd`). Status and ping replies carry a short capability token for passive learning. Results are cached per (callsign, path) with an age.
- **Transport**: ALP sessions use a new stateful session class id carried in DDT2 frames, opened **only** toward peers with a fresh confirmed capability.
- **Versioning**: the capability record has `alp=<major>.<minor>`. Majors must match; minors are feature-flagged.

## Consequences
Works through legacy ratflectors. Mixed nets degrade to Layer 0/1 per peer. FEC and changes below DDT2 are out of scope until hardware data justifies them.

## Validation
Interop test proving that a legacy D-RATS receiving the Aether capability probe, extended GetVersion reply, and status token behaves normally (no errors, no UI noise beyond a version string).

## Phase B amendment (2026-10-29)
Model checking set the ALP reliability rules (`../phase-b/state-machines.md` SM-5):
- **Span limit**: newest − oldest unacknowledged < 2^15. A window limit alone is insufficient, because a stuck block lets the span grow (L3).
- **Window-relative duplicate detection** at the receiver.
- **Poll id echoed in ACKs** (an efficiency rule; the model shows correctness without it).
- **SACK bitmap, SHA-256 transfer identity, explicit `Complete(sha256)`.**

The span-limit plus window-relative dedup combination passed exhaustive checks for every configuration that fit the state bound. The `RPCGetVersion` discovery payload is implemented and tested in `aether-ddt2::session` (encode and decode of the extended dict). Status stays Proposed until the legacy-harmlessness interop test runs.
