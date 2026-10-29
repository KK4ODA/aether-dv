# Decision Log

Owner decisions that close Phase A open questions. ADRs hold the detail.

| # | Date | Question | Decision | Recorded in |
|---|---|---|---|---|
| D1 | 2026-10-29 | Technology stack | Rust engine (separate process) + Tauri 2 + TypeScript/Svelte UI | ADR-0001 (Accepted), ADR-0006 (Accepted) |
| D2 | 2026-10-29 | Licence | GPL-3.0-or-later for code; CC-BY-4.0 for protocol spec | ADR-0008 (Accepted), `LICENSE` |
| D3 | 2026-10-29 | Test hardware available | **Icom IC-9700**, **Icom ID-52 / ID-52 PLUS**, **Kenwood TM-D750** (new 2026 D-STAR/APRS mobile) | `03` §3, `07` §3, `08` Phase C/D |
| D4 | 2026-10-29 | Legacy interop targets | D-RATS **0.4.x only**; 0.3.x best-effort | ADR-0002 (Accepted), `08` |
| D5 | 2026-10-29 | Default data-mode policy | Slow default on shared nets; Fast opt-in; Auto only toward proven-Fast peers | ADR-0005 (Accepted) |
| D6 | 2026-10-29 | Launch screen | Chat | `06` §6 |
| D7 | 2026-10-29 | Multi-port chat | Per-port channels + optional merged "All traffic" view | `06` §6 |
| D8 | 2026-10-29 | First-release forms | **ICS-213 only**; other templates and the form designer deferred | `01`, `08` |

Still open:
- ADR-0003/0004/0007 remain *Proposed* until Phase B review; they are technical and need no owner input yet.
- Whether the ID-52 unit is the original or the PLUS (same CI-V Fast Data command `22 03`; matters only for USB-C and BT details).
- TM-D750 DV data / Fast Data capability is unverified and will be tested on the owner's unit.
