# 08 — Risk Register and Milestone Roadmap

## 1. Risk register

Likelihood and impact use H/M/L.

| ID | Risk | L | I | Mitigation | Early signal / test |
|---|---|---|---|---|---|
| R1 | Icom radio behaviour around DV data (flow control, buffering, banned bytes, key-up) differs per model and firmware and is undocumented | H | H | Data-driven hardware profiles; HW-2/3/4 per radio; never hard-code | HW tests disagree with profile defaults |
| R2 | Programmatic Fast Data control (documented CI-V `22 03` / `1A 05 xxxx`) proves unusable in practice: no concurrent CI-V channel on some models/cables, flash wear, glitches when toggled, Kenwood has none | M | M | ADR-0005: CI-V control only on verified profiles, between transfers; manual guided steps otherwise | HW tests of toggle timing and behaviour |
| R19 | Switching the local radio to FD makes it unreadable to slow-only stations on a shared net | H | M | Default Slow on mixed nets; Auto only point-to-point toward proven peers; visible indicator | Field trials |
| R20 | Path elements (MMDVM hotspots/repeaters on 2025-02…2026-01 builds, transcoding reflectors) corrupt Fast Data | H | M | Path-evidence model (proven/disproven, aging); fall back to slow | HW-8 via hotspot/repeater |
| R21 | Kenwood TH-D74/D75 reportedly cannot transmit slow data, so mixed Icom/Kenwood groups need FD | M | M | Profile flag; mode policy treats Kenwood peers as FD-only | Test with a TH-D75 owner (macrats author) |
| R3 | Fast Data real-world goodput is far below 3480 bps (overhead, radio pacing, loss without FEC) | M | M | Measure first (Phase D); tune ModeProfile; don't promise ratios | HW-5, HW-7 results |
| R4 | Remote fast-capability cannot be known, so data is sent that the peer can't decode | H | H | Evidence states (unknown/declared/proven); warnings; slow default toward unknown peers | HW-8 matrix |
| R5 | Legacy receivers mis-handle Layer-1 optimisations (large windows, uncompressed control frames, no warm-up) | M | H | ADR-0003 gate: interop test per optimisation; per-peer clamp | Interop suite failures |
| R6 | Mixed 0.3.x / 0.4.x nets behave differently (flow control; DR#262) | L | L | **Owner decision D4: 0.4.x only**; 0.3.x best-effort, no test gate | Field reports |
| R7 | Ratflector loops or amplification caused by Aether bridges or by mixed server meshes (simulated: legacy triangle = 1000× amplification in 60 s; mitigated by per-link windows, RF links ≥30 s) | M | H | Cross-link duplicate suppression, hop limits, rate limits, loop simulation suite | Loop-suite frame counts |
| R8 | Dedup heuristics drop legitimate legacy repeats | M | M | Cross-link-only rule (`04` §5.2); per-link TTL tuning; metrics | Simulated "ok ok" test |
| R9 | No access to enough real radios for validation | H | H | Recruit beta testers with specific models early; publish a hardware-test kit (procedures + capture tool) | Phase C staffing |
| R10 | Regulatory: auto-TX data, unattended bridges, identification, third-party traffic differ by country | M | H | Per-jurisdiction notes; control-operator settings; beacon defaults off; ID via radio MYCALL | Pre-release review |
| R11 | Rust/Tauri contributor pool too small for a hobby project | M | M | Small crate set; language-neutral spec; generous docs; UI in mainstream TS | Contributor count after the first public release |
| R12 | WebKitGTK / Linux packaging variance breaks UI on some distros | M | M | AppImage with tested baseline; deb/rpm for current LTS only; the headless engine still works | CI matrix |
| R13 | Serial-port reliability on Windows (port wedging after write timeout, DR#148) | M | H | No write timeouts on the hot path; reopen strategy; USB stable-id tracking | HW-9 on Windows |
| R14 | Scope creep (Winlink, APRS, email, maps) dilutes core reliability | H | M | Phase gates; deferred list in `01`; "stability over features" principle | Roadmap reviews |
| R15 | Public ratflector ecosystem is small or partly dead (ref.d-rats.com gone, DR#295) | M | M | Ship `aetherdv-rfd` early as an easy-to-deploy server; keep legacy compatibility | Connection telemetry (local) |
| R16 | The D-RATS community perceives a competing fork | L | M | Engage upstream: share the spec, test vectors and hardware findings; credit sources | Upstream response |
| R17 | Assumed JARL/Icom facts (e.g. Fast Data framing) prove wrong | M | M | Phase A marks every claim with its evidence level; hardware tests before reliance | — |
| R18 | Licensing contamination (GPL-2.0-only code, API keys) | L | H | ADR-0008; CI licence scan | CI |
| R22 | Legacy ARQ wrap and stale-ACK defects (P19/P20) make large transfers to D-RATS stations fail on one side only | M | M | Block sizing ≤256 blocks; RTO discipline; `wrapWarning` in UI; receiver diagnostics; reproduce against real D-RATS in Phase C | Interop test (arq-model-findings reproduction plan) |

## 2. Milestone roadmap with acceptance criteria

Each phase ends with a review. **A phase is not complete until every criterion holds at the stated validation level.**

### Phase A — Technical investigation (this deliverable)
- A1 Feature inventory with source references ✔ (`01`)
- A2 DDT2 wire analysis with defects, golden vectors matched to upstream ✔ (`02`)
- A3 Ratflector analysis ✔ (`04`)
- A4 Radio capability matrix and Fast Data assessment, every claim tagged by evidence level (`03`)
- A5 Risks, licensing ✔ (`08`, `09`)
- **Exit:** owner review of the open questions in `README.md` §5.

### Phase B — Architecture (2–4 weeks of design work)

**Status 2026-10-29:** B1–B3 and B5 done; B4 built and audited, **awaiting owner acceptance**. Details: `phase-b/README.md`.
- B1 ADRs 0001–0008 accepted or amended.
- B2 Formal state machines (TLA+ or model-based test specs) for legacy ARQ, the scheduler, the ratflector client and message delivery, with legacy defects P1–P18 encoded as negative cases.
- B3 API schema v0 (engine ↔ UI) and storage schema v0.
- B4 Hi-fi mockups of Chat, Message, Connections and Transfers using the tokens; accessibility review.
- B5 Tooling spikes (ADR-0001 validation): serial loopback + replug on 3 OSes; Tauri packaging of an empty shell for all targets in CI.
- **Exit:** spikes pass; the owner accepts the mockups.

### Phase C — Minimal interoperable prototype
Scope is deliberately small: engine, codec, serial adapter, ratflector client, CLI/TUI or minimal UI, durable chat/message store, diagnostics.
- C1 **Interop (I)**: Aether ⇄ D-RATS 0.4.2 chat, ping, status, ICS-213 form, and a 10 KB file in both directions, via PTY and via a legacy ratflector. 100 % of 50 scripted exchanges.
- C2 **Ratflector (I)**: reconnect with back-off after server restart and after a NAT-style half-open, detected within 2× the keepalive interval. No message loss for queued items.
- C3 **Hardware (H)**: HW-0…HW-7 on the **IC-9700 and ID-52 PLUS** in **DV slow** mode, with results published as a hardware profile.
- C4 **Over the air (A)**: one chat + form + 10 KB file between Aether and D-RATS over a real simplex path.
- C5 **Diagnostics**: `.aecap` capture of every test, replayable in the simulator; support-bundle export.
- C6 **Baseline measurements**: D-RATS defaults vs Aether Layer-0 (identical parameters) on the same rig. This is the reference for all later claims.
- **Exit:** C1–C6 at the stated levels.

**Status 2026-10-29 (branch `phase-c`):** everything that can be built and tested without D-RATS or radios is done at Simulated/Interoperability level:
- the engine
- legacy chat, ping, version, file and form transfer, ICS-213 and positions
- ratflector client and server
- capture and support bundle (C5)
- `aetherdv-bench` for C1/C6
- a minimal UI

C1 and C3–C4 need a D-RATS station and the owner's radios, and C6 needs the hardware. Procedures: `phase-c/README.md`. Phase D groundwork (CI-V Fast Data check, read and guarded set) is in place, at Simulated level.

### Phase D — DV Fast Data
- D1 Hardware profiles for the **IC-9700 and ID-52 PLUS** in Fast mode, plus the **TM-D750** (capability determined by test), with HW-2…HW-5 in fast mode.
- D2 HW-8 compatibility matrix published.
- D3 End-to-end fast messaging and file transfer Aether ⇄ Aether (A level) and Aether ⇄ D-RATS on fast-configured radios (A level).
- D4 Performance report: goodput and latency for all workloads in `07` §4, slow vs fast, D-RATS vs Aether Layer-1, with N ≥ 10.
- D5 Mode policy (ADR-0005) implemented; fallback behaviour demonstrated when a peer is slow-only.
- **Exit:** a report with hypotheses H1–H3 confirmed or refuted; mode UX validated with ≥3 operators.

### Phase E — Enhanced transport and full UI
- E1 Layer-1 optimisations individually gated (ADR-0003), each with a measured gain ≥10 % or dropped.
- E2 ALP v1 (receipts, resumable content-addressed transfers, SACK) **only for features whose need is shown by Phase C/D data**.
- E3 Full UI: messaging, forms (**ICS-213** for the first release; others after), transfers, map with offline tiles, connections, diagnostics; screen-reader pass.
- E4 Crash/power-loss tests pass (ADR-0007).
- **Exit:** a 72-hour soak test with fault injection, with no lost queued messages and no stuck transfers.

### Phase F — Ratflector server and release
- F1 `aetherdv-rfd`: legacy-compatible (I against D-RATS 0.4.2 clients); loop suite passes; 200 simulated clients on a Raspberry Pi 4 at <30 % CPU; graceful restart.
- F2 Packaging: MSI, DMG (signed and notarised), AppImage + deb + rpm, arm64/armv7 server builds; reproducible builds; signed update notifications; config and DB migrations tested across versions.
- F3 Docs: operator guide, radio setup guides per profile, server admin guide, protocol spec (CC-BY).
- F4 Validation matrix published in the README from `validation.yaml`.

## 3. Deferred (explicitly out of scope until after the first release)
Winlink/Pat integration, APRS-IS gateway, email gateway, KISS/AGWPE TNCs, socket forwarding, QST RSS/CAP/weather, form designer, mobile clients, MMDVM/Icom terminal-mode frame-level adapters (research track; see `03`).
