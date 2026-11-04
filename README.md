# Aether DV

An open-source D-STAR data communications application for amateur radio. It works alongside [D-RATS](https://github.com/ham-radio-software/D-Rats): it speaks the D-RATS protocol, so it can exchange chat, files and forms with D-RATS stations and use the same ratflectors. It adds Icom DV Fast Data and repeater-aware operation to conventional DV slow data.

Aether DV exists because of D-RATS and the people who built and maintain it; see [Acknowledgements](#acknowledgements).

> **Status: early releases (0.2.x).** The desktop app and engine handle chat, files, ICS-213 messages, position reports, ratflectors and repeater operation. Bench-tested on an IC-9700 and an ID-52 PLUS on simplex; not yet tested through repeaters or against running D-RATS stations. Current state and how to run it: [docs/phase-c/README.md](docs/phase-c/README.md); bench tests: [docs/bench-tests.md](docs/bench-tests.md).

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


## Licence

Code: [GPL-3.0-or-later](LICENSE). Protocol specification documents: CC-BY-4.0. See [ADR-0008](docs/adr/0008-licence.md).

## Acknowledgements

Aether DV is built on what D-RATS established: the DDT2 protocol, the ratflector, and many years of operating experience on D-STAR. Its compatibility, and much of what it knows about radios, come from the D-RATS code and its issue history. Thank you to:

- **Dan Smith, KK7DS**, who created [D-RATS](https://github.com/ham-radio-software/D-Rats) (2008–2012): DDT2 framing, the yEnc encoding, stateful sessions, file and form transfer, and the ratflector.
- **Maurizio Andreotti, IZ2LXI**, who revived D-RATS in 2015–2020 and kept it running for a new generation of operators.
- **John Malmberg, WB8TYW**, who ported D-RATS to Python 3 and GTK 3 and maintains it. His analysis of serial flow control (D-Rats#253, #262, #268) shaped how Aether DV talks to radios.
- **F4HOF (dscp46)**, who found that the bytes `E7 84 76` in slow data are read by receiving radios as packet loss (JARL STD6 §6.2; D-Rats#305, PR #302). Aether DV escapes them the same way.
- The [ham-radio-software](https://github.com/ham-radio-software) community, for [D-RATS](https://github.com/ham-radio-software/D-Rats), [drats2](https://github.com/ham-radio-software/drats2) and [ratflectors](https://github.com/ham-radio-software/ratflectors), and everyone who reported issues there.
- [MacRats](https://github.com/w9fyi/macrats), another independent D-RATS-compatible client.
- **Jonathan Naylor, G4KLX**, whose [ircDDBGateway](https://github.com/g4klx/ircDDBGateway) and [MMDVMHost](https://github.com/g4klx/MMDVMHost) document how D-STAR gateways and repeaters behave.

No code from these projects is included in this repository. Aether DV is a separate implementation written from reading their source and documents. Test vectors include a few sample position sentences from D-RATS's source, and the ICS-213 template uses D-RATS's form and field identifiers so the two interoperate. D-RATS is GPL-3.0-or-later, like Aether DV.
