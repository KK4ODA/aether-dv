# 01 — D-RATS Ecosystem Feature Inventory

Sources:
- D-RATS `ham-radio-software/D-Rats@d0e7a39` (GPL-3.0-or-later)
- drats2 `@e7e5277` (GPL-3.0)
- ratflectors `@4552e3a` (MIT)
- macrats `w9fyi/macrats@db33eba` (GPL-3.0)
- ircDDBGateway `g4klx/ircDDBGateway@f5ab9f7` (GPL-2.0-or-later)

Issue-history catalogue with links: [`research-notes/issues_history.md`](research-notes/issues_history.md). Issues are written `DR#n` = `https://github.com/ham-radio-software/D-Rats/issues/n`.

Disposition key:
- **Retain**: keep the behaviour and the wire format.
- **Redesign**: keep the wire format, rebuild the implementation and UX.
- **Replace**: new mechanism, with legacy kept only as an interop shim.
- **Defer**: later phase.
- **Drop**: not carried forward.

## 1. Project landscape

| Project | State (2026-10) | What it contributes to Aether |
|---|---|---|
| D-RATS 0.4.x (Python 3/GTK 3) | Maintenance only. Last tag 0.4.2 (2023-11). Trickle of fixes through 2026-08. The maintainer has declined DDT2 changes (DR#315). | The reference implementation of the wire protocol and the de-facto interoperability target |
| D-RATS 0.3.x (Python 2) | Unsupported, though some stations may still run it (mixed-version nets reported in DR#262) | Second interop target. Behaviour differs in flow control. |
| drats2 | Stalled. Last commit 2025-03-22. About 1,900 lines of packaging scaffolding and hello-world GUIs; **no protocol code** | Lessons only: toolkit survey (Tk/wx worked everywhere, GTK4 has no native Windows build); stalled under packaging boilerplate with one developer |
| ratflectors | YAML list of 7 public servers (2023-06) | Seed data for server discovery |
| macrats | Alpha Swift/SwiftUI, VoiceOver-first. TX over the air with a Kenwood TH-D75 (2026-04). RX from a real peer **not yet tested**. | Second independent DDT2 implementation, upstream golden vectors, Kenwood TH-D75 configuration findings, accessibility precedent |
| ircDDBGateway `DRATSServer` | Active G4KLX code | Repeater-side RF↔TCP D-RATS bridge. Slow data `0x3X` only, no Fast Data. |

## 2. Feature inventory

| Function | What legacy does | How (evidence) | Known limitations / failure modes | Disposition | Backward-compat requirement |
|---|---|---|---|---|---|
| **Public chat** | Broadcast text to `CQCQCQ` | Chat session id 1, type 0, uncompressed DDT2 (`sessions/chat.py`, `02` §4.3) | No message id, ack or dedup. Text containing `[EOB]` is lost (P2). Shares the type-0 slot with GPS sentences, parsed by content. | **Redesign** UX; keep wire | Byte-exact session-1/type-0 frames. Always send broadcast chat in legacy format. |
| **Private chat** | Same frame with `d_station = CALL` | `ChatSession.write(dest)`. Receivers accept session-1 frames for any destination (`sessionmgr.py:181`), so "private" is only a display filter | Not private, and no delivery confirmation | **Redesign**: optional per-message receipt between Aether peers (ALP); legacy peers marked "unconfirmed" | Same frame format |
| **Ping / echo** | Request/response with version string; echo of N bytes | Chat types 1–4 | Broadcast ping produces a reply storm after random 0–5 s delays | **Retain**, rate-limited; the echo becomes the link-quality test tool | Must answer legacy pings |
| **Station status** | Online / unattended / offline + text | Chat type 5, ASCII digit + text | Shows *intended* presence, not link state (DR#234) | **Redesign**: separate presence from link health in the UI | Wire retained |
| **Station discovery / heard list** | Remember `s_station → last heard time and port` | `SessionManager._stations_heard`; UI `main_stations.py` | Last port wins, so stations flap between ratflectors (DR#71). Expiry 60 min (`expire_stations`). | **Redesign**: per-path heard records (RF-slow / RF-fast / which ratflector), signal and source evidence | None on the wire |
| **Position reporting** | NMEA-GGA + call/comment, or DPRS `$$CRC` APRS-style, as chat type 0. Also parses the radio's own GPS-TX output. | `gps.py:629-766`, `transport.py:282-333` | Two parsers. No-fix NMEA breaks detection (DR#306). DMS/decimal bug (DR#211, fixed). Radio GPS-TX competes for slow-data capacity. | **Redesign**: one parser, fuzz-tested; beacon scheduler aware of channel load | Emit and parse both formats exactly |
| **Maps** | GTK slippy map, online tiles (Thunderforest/OSM), markers | `d_rats/map/*` | Provider churn and API keys **committed in config defaults** (`config_defaults.py:146-162`); weak offline support; Pi performance (DR#135) | **Replace**: MapLibre with offline vector tiles (PMTiles/MBTiles), user-supplied regions | None |
| **Messages / forms** | XML forms (ICS-213, HICS-260, radiogram, memo, email) with path/src/dst/mid | `formgui.py`, `forms/*.xml/.xsl` | Fragile XML handling (DR#76, #175, #89). lxml with default parser (XXE hardening needed). Form designer is GTK-bound. | **Redesign** storage (SQLite plus attachments); **retain** the XML interchange format for the wire | Import, export and transmit legacy form XML unchanged. Preserve `mid` format. |
| **Store-and-forward routing** | Outbox scan every 30 s. Direct if heard within 600 s, else `routes.txt`, default route, gratuitous `A;B;C`, email, `WL2K:` | `msgrouting.py:134-330` | Per-hop confirmation only. Lock files go stale (DR#122). Nothing sends until *some* station is heard. Self-sends lock forever. | **Redesign**: durable DB queue, explicit per-hop and end-to-end state, operator-visible reasons | Path-element semantics (loop prevention) and gratuitous-route syntax retained |
| **File transfer** | Offer (LE size + name), OK/RESUME, zlib stream over stateful ARQ | `sessions/file.py`, `02` §4.6 | Resume by name only. Double compression. Completion inferred from session close. "Stuck pending" bug (DR#315). Slow over TCP (DR#261). | **Redesign** (Layer 1 tuning) + **Replace** for Aether peers (ALP content-addressed transfers) | Full legacy send and receive, including RESUME |
| **Remote file ops (RPC)** | List / pull / delete files, list / pull forms, position request, version, mail check | `sessions/rpc.py` | No retry (30 s timeout). Path traversal in pull (`rpc.py:900`). Spoofable auth. | **Retain** wire; **harden** receiver; remote ops off by default | Answer GetVersion and position requests by default. Others opt-in. |
| **QSTs (scheduled broadcasts)** | Text, file, exec (shell), GPS, GPS-A, RSS, CAP, weather, station | `qst.py` | `QSTExec` runs shell commands. Weather providers churn and need keys. | **Redesign** a minimal set (text, position, file); **Defer** RSS/CAP/weather; **Drop** exec | Output must be legacy chat frames |
| **Socket forwarding** | TCP port forwarding over a stateful session | `sessions/sock.py` | Niche. Bandwidth-inappropriate on slow data. | **Defer** (probably Drop) | Must not crash on an incoming socket-session request; refuse it cleanly |
| **Email gateway (SMTP/POP3)** | Local SMTP/POP3 servers bridging forms ↔ email | `mailsrv.py`, `emailgw.py` | `smtpd` removed in Python 3.12+ (DR#286, #296). Untested by the maintainer. | **Defer** (Phase F+) | — |
| **Winlink (WL2K)** | Send/receive via CMS (network) or RMS | `wl2k.py`, `lzhuf.py` | lzhuf little-endian only (DR#153); py3 byte bugs | **Defer**, later via a standard client library or Pat integration | — |
| **Radio connections** | Serial (9600 typical), TNC/KISS, AX.25, AGWPE, "dongle" | `comm.py` | Lost bytes from XON/XOFF handling (DR#253/#262). Windows port wedges (DR#148). Port config is a comma-delimited string (DR#269). Polling I/O. | **Replace** with the `aether-radio` adapter layer; serial first, KISS/AGWPE deferred | Same byte stream to the radio |
| **Ratflector client** | TCP 9000, optional USER/PASS | `comm.py:1027-1107` | Silent disconnects, no keepalive, fixed 5 s retry (DR#75/#109) | **Redesign** (`04` §6) | Exact handshake incl. old-school no-banner |
| **Ratflector server** | Flooding hub plus serial bridge, GTK-coupled | `d-rats_repeater.py` | Needs GTK headless (DR#241); no dedup, loops, unbounded queues (`04` §4) | **Replace** with `aether-rfd` | Exact client-facing behaviour |
| **Sniffer / diagnostics** | Decoded frame log in the event tab; serial hex log | `sessions/sniff.py`, `SWFSerial.set_log_file` | Ad hoc | **Replace** with structured trace + `.aecap` wire capture + replay | None |
| **Configuration** | INI (`d-rats.config`) with stringly-typed port specs | `dratsconfig.py`, `config_defaults.py` | GUI can corrupt the file; delimited strings | **Replace** with typed config + DB; **import** legacy config | Import only |
| **Warm-up frame** | Type-254 frame from `!` before a burst after idle | `transport.py:359-371` | Disabled by default since 0.3.6 | **Retain** as a per-radio-profile option (data-driven) | Ignore received warm-ups; send only if the profile requires |
| **Compat / raw mode** | Treat non-DDT2 text from the radio as chat | `transport.py:448-454` | Garbage from noise becomes chat | **Retain** as an opt-in "plain text monitor" | — |

## 3. Interoperability baseline that Aether must preserve

1. DDT2 header, magic values, CRC-16/XMODEM, `~` padding, yEnc banned set (sending: the post-#302 superset; receiving: accept any escaping), and the `[SOB]/[EOB]` envelope.
2. Positional session ids: 0 control, 1 chat, 2 RPC. Control types 0–3+t. Chat types 0–5. Stateful types 1/2/4/5. RPC types 0/1 with GS/RS/US encoding.
3. File/form offer `"<I"` + name, `OK` / `RESUME:<n>`, zlib stream, end by session close.
4. Form XML schema and the `<path>` element semantics.
5. Position formats: NMEA GGA + 8/20-char suffix, and `$$CRC` DPRS.
6. The ratflector handshake, including the no-banner path.

**None of these has been verified against a running D-RATS from this environment.** Phase C acceptance criterion C1 covers it.
