# Aether DV

A modern, open-source D-STAR data communications platform for amateur radio. It aims to be an independent successor to D-RATS: interoperable with existing D-RATS stations and ratflectors, and supporting both conventional DV slow data and Icom DV Fast Data.

> **Status: Phase A (technical investigation) complete; Phase B (architecture) in review.**
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
docs/                 investigation, architecture, ADRs, plans
docs/tools/           analysis scripts (DDT2 model, ARQ throughput model) — not production code
docs/research-notes/  raw research notes (issue history, Fast Data hardware research)
```

Planned layout (Phase C onward; see docs/05-architecture.md §4): a Rust workspace under `crates/` (codec, link, radio, store, ratflector client/server, simulator) and a desktop UI under `ui/`.

## Licence

Not yet chosen; [ADR-0008](docs/adr/0008-licence.md) proposes GPL-3.0-or-later for code and CC-BY-4.0 for the protocol specification. Until a LICENSE file is added, all rights are reserved by the authors.

## Acknowledgements

The analysis builds on study of [D-RATS](https://github.com/ham-radio-software/D-Rats) (Dan Smith KK7DS and maintainers), [drats2](https://github.com/ham-radio-software/drats2), [ratflectors](https://github.com/ham-radio-software/ratflectors), [MacRats](https://github.com/w9fyi/macrats), and G4KLX's [ircDDBGateway](https://github.com/g4klx/ircDDBGateway) and [MMDVMHost](https://github.com/g4klx/MMDVMHost). No code from these projects is included in this repository.
