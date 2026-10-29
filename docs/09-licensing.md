# 09 — Licensing Considerations

Not legal advice; a project-planning summary.

| Source | Licence | Implication for Aether |
|---|---|---|
| D-RATS (code, forms, XSL, docs) | GPL-3.0-or-later (`COPYING`, file headers) | Copying or translating code, including a "faithful port" line by line, makes the result a derivative work under GPL-3.0. Forms/XSL, if bundled, carry GPL. |
| drats2 | GPL-3.0 | Same; little code worth reusing |
| macrats | GPL-3.0 | Golden vectors and findings are facts; copying Swift code would make Aether a derivative |
| ratflectors.yml | MIT | Free to fetch and bundle with notice |
| ircDDBGateway | GPL-2.0-or-later | Reference only. Can be combined under GPL-3.0 via "or later". |
| MMDVMHost | GPL-2.0 (header text in this checkout does not say "or later"; check per file) | **Do not copy into a GPL-3.0 codebase.** GPL-2.0-only is incompatible with GPL-3.0. Use it as reference for facts only. |
| JARL D-STAR standard (STD6 etc.) | Published spec | Implementing it is fine; quoting large passages is not |
| Icom/Kenwood manuals | Copyright | Paraphrase menu paths; don't reproduce manual pages |
| D-RATS config defaults | contain third-party **API keys** (`config_defaults.py:146-162`) | Never copy them. Aether must not ship third-party keys. |

Facts about a protocol (field layouts, constants, byte values, algorithms as ideas) are not protected by copyright, so an implementation written from a specification is not a derivative work. **But the implementers here have read the GPL source**, so a defensible "clean-room" claim would require a spec-writer/implementer separation that this project does not have.

**Recommendation (ADR-0008): license Aether DV under GPL-3.0-or-later.** This:
- removes the derivative-work question entirely,
- permits bundling D-RATS forms/XSL and reusing test vectors,
- matches the community's expectations,
- is compatible with the MIT/Apache-2.0 Rust and Tauri ecosystem (those licences are GPL-3.0-compatible in that direction).

Publish the protocol specification (`02` plus later formal versions) under **CC-BY-4.0** so other implementers (macrats, future clients) can use it without GPL obligations.

Consequences: binaries must ship with source offers and licence texts for all dependencies. Generate an SBOM and a third-party notice file in CI (`cargo-about`, plus the npm licence checker). Bundled fonts (IBM Plex, OFL-1.1) and map data (OSM, ODbL; attribution required) need their own notices.
