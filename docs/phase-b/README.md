# Phase B — Architecture (work in progress)

Branch: `phase-b`. Started 2026-10-29 at the owner's request; executed unattended.

Phase B acceptance criteria come from `../08-risks-and-roadmap.md`. Status values:
- **Done**: criterion met as written
- **Partial**: deliverable exists but part of the criterion is unmet
- **Owner**: needs owner review or approval
- **Blocked**: cannot be done in this environment

| ID | Criterion | Deliverable | Status |
|---|---|---|---|
| B1 | ADRs 0001–0008 accepted or amended | `../adr/` | 5 of 8 accepted by the owner; 0003/0004/0007 to be reviewed with the Phase B results |
| B2 | Formal state machines with legacy defects as negative cases | [`state-machines.md`](state-machines.md), [`arq-model-findings.md`](arq-model-findings.md), `../../spec/models/` | **Done** (spec). Legacy ARQ model-checked; found defects P19/P20. TLA+ tooling unavailable here, so a project-written exhaustive checker was used. |
| B3 | API schema v0 and storage schema v0 | [`../../spec/`](../../spec/README.md) | **Done**: 49 methods / 9 event types; schema applies cleanly and passes 20 constraint checks |
| B4 | Hi-fi mockups plus accessibility review | `../../design/mockups/` | pending (owner acceptance needed) |
| B5 | Tooling spikes: serial loopback and replug on 3 OSes; Tauri packaging in CI | `../../crates/`, `../../app/`, `../../.github/workflows/` | pending |

Extra work, labelled as Phase C groundwork: the `aether-ddt2` codec crate, which turns the protocol spec into executable, golden-vector-tested code. Its validation level is **Simulated** only.

Results and evidence are appended below as each item completes.
