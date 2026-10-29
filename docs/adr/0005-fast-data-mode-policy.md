# ADR-0005: Fast Data is a radio setting plus a ModeProfile; no blind automatic switching

Status: **Accepted** 2026-10-29 (owner decision) · Proposed: 2026-10-29

## Context
On every radio studied, DV Fast Data on/off is a radio setting. The computer-facing DV Data port carries the same byte stream in both modes (`03` §2). On current Icom models the setting is **documented as readable and settable via CI-V** (`22 03` on ID-52/ID-50/ID-52 PLUS/ID-5200; `1A 05 0093` IC-705, `0077` IC-9700, `0098` IC-905, all verified in Icom CI-V guides). This requires a CI-V channel concurrent with the data channel, which is not possible on every model or cable setup. Kenwood has no documented control path. Remote and path receive capability cannot be inferred from the radio at all. A wrong choice makes data undecodable for the peer, and while the local radio is in FD it is unreadable to every slow-only station on the channel.

## Decision
- Three operator modes: Slow, Fast, Auto. **Auto** selects the ModeProfile (timers, burst sizing) from evidence. It changes the radio's setting only on models with a documented and hardware-tested control path (e.g. CI-V), never mid-transfer, and with an operator-visible log entry. Otherwise Auto *recommends* and asks.
- Peer fast-capability states: unknown → declared (operator or capability record) → proven (end-to-end ACK while local FD on) → disproven (repeated loss in FD mode while slow succeeds). Proofs expire.
- Fallback: if a peer is not proven fast, show a send-time warning and offer "switch radio to slow" steps.

## Consequences
Honest UX; no hidden radio reconfiguration; a possible extra manual step for operators. Revisit if a documented control path is verified (HW-8 plus CI-V tests).

## Decision record (2026-10-29)
Owner chose **Slow by default on shared nets, Fast opt-in**. Fast is for point-to-point or for nets whose participants are all proven Fast. Auto switches only between transfers, toward proven-Fast peers, on hardware-verified CI-V profiles. First CI-V control targets are the owner's IC-9700 (`1A 05 0077`) and ID-52 (`22 03`). The Kenwood TM-D750 has no documented control path and is manual-only until shown otherwise.
