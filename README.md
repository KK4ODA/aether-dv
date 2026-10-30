# Aether DV

A modern, open-source D-STAR data communications platform for amateur radio. It aims to be an independent successor to D-RATS: interoperable with existing D-RATS stations and ratflectors, and supporting both conventional DV slow data and Icom DV Fast Data.

> **Status: Phase A (technical investigation) complete; owner decisions recorded ([docs/decisions.md](docs/decisions.md)); Phase B (architecture) next.**
> There is no application code yet, by design. Nothing in this repository has been tested on radio hardware or against a running D-RATS installation.

## Goals, in priority order

1. Reliable RF data communication over DV Slow Data and DV Fast Data
2. Interoperability with existing D-RATS stations and ratflectors
3. A polished, accessible, keyboard-first desktop UI
4. Robust operation over intermittent RF and Internet links
5. First-class ratflector support, including a standalone server
6. Cross-platform, offline-capable, easy to install and maintain

## Where to start

| Document | What it covers |
|---|---|
| [docs/README.md](docs/README.md) | Phase A summary, answers to the ten investigation questions, compatibility matrix, open decisions |
| [docs/02-legacy-protocol-analysis.md](docs/02-legacy-protocol-analysis.md) | D-RATS DDT2 wire protocol, with source references and a defect register |
| [docs/03-dstar-rf-transport.md](docs/03-dstar-rf-transport.md) | Slow Data vs Fast Data, radio compatibility matrix, CI-V control |
| [docs/05-architecture.md](docs/05-architecture.md) | Proposed architecture |
| [docs/adr/](docs/adr/) | Architecture Decision Records (all *Proposed*) |
| [docs/07-test-and-validation-plan.md](docs/07-test-and-validation-plan.md) | Validation levels and first hardware tests |
| [docs/08-risks-and-roadmap.md](docs/08-risks-and-roadmap.md) | Risks and phased roadmap with acceptance criteria |

## Validation levels

Every feature claim in this project carries one of these levels:
- **Simulated**: passes in the software simulator
- **Interoperability**: verified against unmodified D-RATS or a legacy ratflector
- **Hardware**: verified with an identified radio and firmware
- **Over the air**: verified between real stations over RF

Passing through a virtual serial port is never described as RF-tested.

## Repository layout

```
crates/aether-ddt2    D-RATS DDT2 codec (frames, yEnc, envelopes, sessions), golden vectors
crates/aether-link    ARQ-1 (legacy-compatible reliability) and the half-duplex channel scheduler
crates/aether-rfl     ratflector client (sans-I/O)
crates/aether-rfd     ratflector server (aetherdv-rfd)
crates/aether-store   SQLite storage with forward-only migrations
crates/aether-forms   D-RATS form documents (ICS-213)
crates/aether-geo     position reports (NMEA style, DPRS)
crates/aether-civ     Icom CI-V codec, Fast Data commands
crates/aetherdv        the engine (aetherdv) plus aetherdv-cli, aetherdv-bench, aetherdv-cap
app/                  desktop app (Tauri 2 shell + UI on the engine API)
spec/                 machine-checked specs: storage schema, API (OpenRPC), ARQ model
design/               mockups and design tooling
docs/                 investigation, architecture, ADRs, plans, phase reports
examples/             engine configurations for the IC-9700 and ID-52 PLUS
```

Current status and how to run it: [docs/phase-c/README.md](docs/phase-c/README.md).

## Licence

Code: [GPL-3.0-or-later](LICENSE). Protocol specification documents: CC-BY-4.0. See [ADR-0008](docs/adr/0008-licence.md).

## Acknowledgements

The analysis builds on study of [D-RATS](https://github.com/ham-radio-software/D-Rats) (Dan Smith KK7DS and maintainers), [drats2](https://github.com/ham-radio-software/drats2), [ratflectors](https://github.com/ham-radio-software/ratflectors), [MacRats](https://github.com/w9fyi/macrats), and G4KLX's [ircDDBGateway](https://github.com/g4klx/ircDDBGateway) and [MMDVMHost](https://github.com/g4klx/MMDVMHost). No code from these projects is included in this repository. Test vectors include a few sample position sentences from D-RATS's source, and the ICS-213 template uses D-RATS's form and field identifiers so the two interoperate (D-RATS is GPL-3.0-or-later).
