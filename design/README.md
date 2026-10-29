# design/: high-fidelity mockups (Phase B, B4)

Static HTML/CSS mockups of the four core screens, built on the design tokens. **Needs owner review**: Phase B exit requires your acceptance of these screens.

| Screen | File | Screenshot (1440×900) |
|---|---|---|
| Chat (launch screen, decision D6) | `mockups/chat.html` | `mockups/screens/chat-1440.png` |
| Message with delivery timeline | `mockups/message.html` | `mockups/screens/message-1440.png` |
| Connections: radio port and Fast Data mode | `mockups/connections.html` | `mockups/screens/connections-1440.png` |
| Transfers | `mockups/transfers.html` | `mockups/screens/transfers-1440.png` |

Each screen is also rendered at 1280×800 (`*-1280.png`) to check that the layout fits on smaller laptops.

Open any `.html` file directly in a browser; everything is local, including the fonts.

## What the mockups demonstrate
- **The transport strip** is the app's signature element. Every connection shows its path (Slow / Fast / Internet as colour + shape + word), live state, queue depth and throughput, on every screen.
- **Honest delivery states.**
  - Broadcast chat ends at "Transmitted" (hollow ◌), never "Delivered".
  - A relayed ICS-213 to a D-RATS station shows "Unconfirmed at EOC-1" with the reason.
  - Only Aether-verified transfers show "Delivered, checksum verified".
- **Fast Data explained in place.**
  - The ID-52 PLUS port shows the radio's actual setting (read over CI-V).
  - It shows who can receive fast data, with evidence for each station.
  - It warns which stations a Fast transmission would miss.
- **The legacy wrap risk (P19)** surfaces as a plain-language note on large transfers to D-RATS stations.
- **Recovery**: the transfer inspector explains what happens if the link drops.

Content is illustrative. Callsigns other than KK4ODA, names, firmware versions and measurements are placeholders, not real stations or test results.

## Design tokens v1 (`mockups/tokens.css`), changes from v0 (`docs/06` §5)
- Warm graphite surfaces (`#1c1b18` base) replace the v0 near-black, which read as a generic dark theme.
- Text `--text-2` changed to `#b5afa2`; `--text-3` changed to `#99938a` after the contrast audit (below).
- New `--line-control` (`#7a7569`) for interactive control borders, to meet WCAG 1.4.11 non-text contrast (≥3:1). `--line-strong` stays for structural rules.
- Typography: IBM Plex Sans throughout, with tabular figures. Plex Sans Condensed semibold is used only for callsigns. Plex Mono is reserved for raw wire data, so it doesn't spread to every small label.

## Accessibility review
`tools/a11y.js` (project-written; the axe-core package was not used) renders each page in Chromium and checks:
- WCAG 2.2 AA text contrast of every visible text element against its effective background
- accessible names for buttons, links, controls, images and progress bars
- landmarks, a single h1, `lang`
- a visible focus indicator at every Tab stop

| Run | Result |
|---|---|
| First pass | 97 issues, all from one token (`--text-3` at 3.6–4.2:1) |
| After token fix | **0 issues** across 497 text elements and 75 focus stops |
| Non-text contrast (manual calculation) | Control borders were 1.7–2.0:1; fixed with `--line-control` (3.4–4.0:1) |

Not yet done: screen-reader passes with VoiceOver, NVDA and Orca (ADR-0006 validation), the light theme, and reduced-motion review once there is motion.

## Rebuild
```bash
python3 design/tools/build.py
NODE_PATH=/opt/node-tools/node_modules node design/tools/shoot.js chat message connections transfers
NODE_PATH=/opt/node-tools/node_modules node design/tools/a11y.js chat message connections transfers
```
`build.py` assembles each page from the shared frame in `mockups/src/`. Edit `src/*.main.html` and `src/*.css`, not the generated pages. (The `NODE_PATH` above points at this environment's Playwright install; adjust it locally.)

Fonts: IBM Plex, SIL Open Font License 1.1 (`mockups/fonts/OFL-NOTICE.txt`).
