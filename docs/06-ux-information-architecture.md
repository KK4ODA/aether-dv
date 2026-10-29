# 06 — UX: Information Architecture, Wireframes, Design Tokens

Status: proposal for review. These are low-fidelity wireframes; visual mockups follow approval of the IA and tokens (Phase B exit).

## 1. Design principles (derived from operator workflow and legacy pain points)

1. **The link is the product.** Transport, state and queue are always visible in a persistent strip and never hidden behind a tab. Legacy D-RATS shows intended presence instead of real link state (DR#234).
2. **Honesty over reassurance.** Every outbound item shows *evidence-based* state: Queued → Sending → Handed off → Delivered | Unconfirmed | Failed. Nothing says "sent" when only the serial port accepted the bytes.
3. **Dense, calm, keyboard-first.** A net-control operator handles many stations for hours. Use compact rows, no cards-for-everything, no decorative gradients, and no oversized controls. Every action is reachable from the keyboard.
4. **Explain the radio.** When something is unavailable ("Fast Data not possible"), say *why* and *what to change on which radio menu*.
5. **Accessible by construction.** Every status has text and shape as well as colour; ARIA live regions announce RX and delivery changes; it works with VoiceOver, NVDA and Orca. macrats shows real demand (DR#315).
6. **Works degraded.** With no map tiles, no Internet, or a disconnected UI, the engine keeps running and the UI reconnects and catches up from the event log.

## 2. Information architecture

```
Aether DV
├── Overview            station identity · ports summary · unread · pending queue · recent events
├── Chat                channels per port/net (e.g. "RF 145.670 – Slow", "Ratflector sewx")
│   └── Direct          per-station conversation (receipts shown where the peer supports them)
├── Messages            Inbox · Outbox (queue) · Sent · Drafts · Archive
│   └── Message view    form render · attachments · delivery timeline (per hop, per path)
├── Forms               templates (ICS-213, ICS-214, HICS-260, Radiogram, custom) · new · import legacy XML
├── Transfers           active · queued · suspended · completed; per-transfer stats
├── Stations & Map      station table ⇄ map (split) · position history · heard-via paths
├── Connections         Radios (ports, profiles, setup assistant, tests) · Ratflectors (saved, favourites, status)
├── Diagnostics         live link metrics · frame log (decoded) · wire capture · self-tests · support bundle
└── Settings            station · callsign/ID · beacons · storage/retention · map regions · appearance · updates
```

Persistent chrome:
- **Transport strip** (top). One segment per active port: name · **mode badge** (`DV SLOW` / `DV FAST` / `NET`) · state (Ready / TX / RX / Listening / Lost / Backoff) · queue depth · current goodput · retries. Click to open the port's diagnostics.
- **Left rail**: sections with icon + label (labels always visible at ≥1280 px, collapsible).
- **Activity drawer** (bottom, toggled with `` Ctrl+` ``): timestamped engine events. Ctrl+F filters it.

## 3. Wireframes

### 3.1 Main window: Chat

```
┌───────────────────────────────────────────────────────────────────────────────────────────┐
│ AETHER DV  N0CALL   │ IC-705 /USB1  [DV FAST] ● Ready  Q:2  1.9 kb/s  rtx 3% │ sewx [NET] ● Up  Q:0 │ ⏻ │
├────────┬──────────────────────────────────────────────────────────┬───────────────────────┤
│ ◉ Over │ # RF 145.670 · DV FAST                       ⌕   ⋯       │ STATIONS ON THIS NET  │
│ ✉ Chat │──────────────────────────────────────────────────────────│ W1AW     RF·fast  12s │
│ ▤ Msgs │ 14:02:11  W1AW      Net check-in, all stations stand by  │ K7ABC    RF·slow  2m  │
│ ▦ Forms│ 14:02:40  K7ABC     K7ABC portable, 3 msgs for Red Cross │ N5XYZ    NET·sewx 4m  │
│ ⇅ Xfer │ 14:03:05  N0CALL ✓  Roger K7ABC, send traffic       [⇢ RF]│ …                     │
│ ⌖ Map  │ 14:03:30  ⚑ K7ABC position 33.12N 96.70W  (DPRS)         │                       │
│ ⎔ Conn │                                                          │ PENDING TO THIS NET   │
│ ⚕ Diag │                                                          │ ICS-213 → W1AW  2/3 ▓▓░│
│ ⚙ Set  │──────────────────────────────────────────────────────────│ photo.jpg → K7ABC ⏸   │
│        │ To: CQCQCQ ▾   via: IC-705 DV FAST ▾      [Send ⌃⏎]       │                       │
│        │ > _                                                      │                       │
├────────┴──────────────────────────────────────────────────────────┴───────────────────────┤
│ ▸ 14:03:06 TX 59 B on IC-705 (air ≈0.9 s) · 14:03:05 RX W1AW ping reply (Aether 0.3, ALP1)   │
└───────────────────────────────────────────────────────────────────────────────────────────┘
```
`✓` on our own line means the frame was transmitted (serial accepted plus estimated air time). For broadcast chat **no delivery claim is possible**, and the tooltip says so.

### 3.2 Message view with delivery timeline

```
┌ ICS-213  "Shelter supply request"   MID N0CALL.1c9f3a2.4be1          [Reply] [Fwd] [Export]┐
│ To: EOC-1 (via W1AW)    From: N0CALL    Priority: Urgent                                  │
│ ┌ form render ─────────────────────────────────────────────┐ ┌ DELIVERY ──────────────────┐│
│ │ Incident: Flood 26-10  Date/Time: 2026-10-29 14:05       │ │ 14:05:12 Queued (durable)  ││
│ │ To: EOC Logistics  From: Shelter 3                        │ │ 14:05:40 Sending → W1AW    ││
│ │ Message: 40 cots, 2 cases water, generator fuel…          │ │          IC-705 DV FAST    ││
│ └───────────────────────────────────────────────────────────┘ │ 14:06:02 Handed off W1AW ✓ ││
│ Attachments: none                                             │          (hop ACK, ARQ)    ││
│                                                               │ —        End-to-end: ◌     ││
│                                                               │   W1AW is legacy D-RATS:   ││
│                                                               │   no receipt possible.     ││
│                                                               └────────────────────────────┘│
└────────────────────────────────────────────────────────────────────────────────────────────┘
```

### 3.3 Connections: radio port and mode

```
┌ IC-705 · /dev/serial/by-id/usb-Icom_IC-705_…-if02 (DV data) ───────────────────────────────┐
│ Profile: Icom IC-705 (fw ≥ 1.30 assumed – verify) [Change]     State: ● Ready             │
│                                                                                            │
│ Data mode      ( ) Slow   (•) Fast   ( ) Auto                                              │
│   Radio setting (read via CI-V on USB A, 2 s ago): DV Fast Data = ON ✓  [Refresh]          │
│   No CI-V link? Aether can't see the setting: set SET › DV SET › DV Fast Data › ON          │
│     [I've set it]  [Show steps]  [Run loopback test]                                        │
│ Peer compatibility   W1AW: proven fast (ACK 14:06) · K7ABC: slow only (declared)            │
│   → Sending to K7ABC while this radio is in Fast mode will not be received. [Details]       │
│                                                                                            │
│ Serial: 9600 8N1 · flow: software XON/XOFF (profile) · DTR on        [Advanced]            │
│ Measured: key-up 0.62 s · tail 0.48 s · goodput (last xfer) 231 B/s · retx 4.1%             │
└────────────────────────────────────────────────────────────────────────────────────────────┘
```
The menu path, firmware and figures in this wireframe are **illustrative placeholders**, not verified facts. Menu paths shown in the real UI come from the verified hardware-profile database. Unverified entries are labelled "unverified".

### 3.4 Transfers

```
 ID   Item               Peer   Path            Size    Progress        Rate     Retx  State
 T12  ICS-213 #4be1      W1AW   IC-705 FAST     1.2 kB  ▓▓▓▓▓▓▓▓▓▓ 100% 240 B/s   0%   Handed off
 T13  photo.jpg          K7ABC  IC-705 SLOW    38.0 kB  ▓▓▓░░░░░░░  31%  86 B/s   7%   Sending (burst 4/12)
 T14  roster.csv         N5XYZ  NET sewx        4.1 kB  ░░░░░░░░░░   0%    —       —   Suspended: link down
                                              [Pause] [Resume] [Reprioritise ↑↓] [Cancel] [Details]
```

## 4. Keyboard map (initial)

| Keys | Action |
|---|---|
| Ctrl+1…9 | Jump to section |
| Ctrl+K | Command palette (send to…, switch port, ping, beacon now, export log) |
| Ctrl+N / Ctrl+Shift+N | New chat message / new form message |
| Ctrl+Enter | Send |
| Alt+↑/↓ | Previous/next conversation |
| Ctrl+` | Toggle activity drawer |
| Ctrl+Shift+P | Ping selected station |
| Ctrl+. | Pause/resume all bulk transfers (chat unaffected) |
| F6 | Cycle focus regions (rail → list → detail → composer) |

## 5. Design-token system (draft v0)

Identity: an **instrument panel at night**. Warm graphite surfaces, hairline rules, tabular numerals, and colour reserved for *signal meaning*. Typeface: **IBM Plex Sans / Plex Sans Condensed / Plex Mono** (SIL OFL), bundled for offline use.

```jsonc
{
  "color": {
    "bg.base":       "#111315", "bg.raised":  "#171A1D", "bg.sunken": "#0C0E10",
    "line.hair":     "#262B30", "line.strong":"#3A4148",
    "text.primary":  "#DCE1E5", "text.secondary": "#9AA4AC", "text.disabled": "#5C656D",
    "transport.slow": "#7FA7E8",   // DV SLOW  – shape ◇, label "SLOW"
    "transport.fast": "#E8A24A",   // DV FAST  – shape ◆, label "FAST"
    "transport.net":  "#B596E6",   // NET      – shape ⌁, label "NET"
    "state.ok":       "#5BC08A",   // delivered / up
    "state.pending":  "#C9B458",   // queued / handed-off
    "state.unknown":  "#8A949C",   // unconfirmed (hollow glyph ◌)
    "state.fail":     "#E0645C",
    "tx.active":      "#F2C14E", "rx.active": "#59C3D6",
    "focus.ring":     "#8AB4FF"
  },
  "light": { "bg.base": "#F4F5F6", "bg.raised": "#FFFFFF", "text.primary": "#15181B", "...": "mirrored, contrast-checked" },
  "font": { "ui": "IBM Plex Sans", "dense": "IBM Plex Sans Condensed", "mono": "IBM Plex Mono",
            "size": { "xs": 11, "sm": 12, "md": 13, "lg": 15, "xl": 18 }, "numeric": "tabular-nums" },
  "space": { "1": 2, "2": 4, "3": 8, "4": 12, "5": 16, "6": 24 },
  "row":   { "compact": 24, "default": 28, "comfortable": 34 },
  "radius":{ "sm": 2, "md": 4 },
  "motion":{ "fast": "80ms", "base": "140ms", "reduced": "0ms" },
  "z":     { "drawer": 10, "popover": 20, "modal": 30, "toast": 40 }
}
```

Rules:
- Transport colours appear only on the mode badge and path tags, always alongside shape and label.
- Contrast: all text is at least WCAG AA against its surface; the light theme must be verified separately.
- No colour-only state; reduced-motion is respected; TX/RX indicators use a steady state, not blinking.

## 6. UX decisions (owner, 2026-10-29)

- Launch screen: **Chat** (D6).
- Multi-port chat: **per-port channels plus an optional merged "All traffic" view** (D7).
- Forms: **ICS-213 only** in the first release; designer deferred (D8).

### Original questions (for the record)

1. Primary screen at launch: Chat (net-centric) or Overview (dashboard)? The proposal is **Chat**, with Overview one keystroke away.
2. Does a multi-port station (one radio + two ratflectors) want a merged net view, or strictly per-port channels? The proposal is per-port channels plus an optional "All traffic" merged view.
3. Should forms be ICS-first (with templates) or generic designer-first? The proposal is templates first, designer deferred.
