# Aether DV desktop app

A Tauri 2 shell around a plain HTML/CSS/JS interface (`ui/`). The interface talks only to the engine, `aetherdv`, over its local API (JSON-RPC over WebSocket, token required). It never opens radios, files or the database itself (ADR-0001).

## Run it

Installed builds include the engine (`aetherdv`) and `aetherdv-cli` next to the app. On the connect screen, enter the path of your configuration file (see `examples/`) and choose **Start the engine**.
- **Token:** the engine's token goes to the app's data directory.
- **Console output:** goes to `aetherdv-console.log` there.
- **Relative paths** in the configuration are relative to the configuration file.
- **When the app closes,** it stops the engine it started.

To use an engine you started yourself (`aetherdv --config my.toml`), enter its `aetherdv.token` path and choose Connect. Only the path is remembered, never the token.

## Updates

Diagnostics shows the version and has **Check for updates**. The app also checks once, a few seconds after it starts.
- **Prompt:** when an update exists, a prompt shows its notes with **Install and restart**. Nothing installs without that click.
- **Engine:** an engine started by the app stops during the update; start it again afterwards.
- **Signing:** every package is signature-checked against the key built into the app. The signature must name the announced version, so replayed or downgraded packages are refused.
- **Local builds** carry no key and say they can't update.

Release process and key setup: `docs/releasing.md`. End-to-end test: `app/tools/updater-e2e.sh`.

## What works (Phase C minimal UI)

| View | |
|---|---|
| Chat (launch screen) | Per-port channels plus "All traffic"; live lines; send to everyone or one station; honest send states (Queued, Transmitted, Not sent; never "delivered" for chat); position reports shown as positions; Heard and Waiting-to-send panels |
| Messages | Inbox, outbox, sent and drafts; ICS-213 rendered as the paper form; delivery ladder; compose and send a new ICS-213; retry a failed message |
| Transfers | All transfers with progress and outcome; cancel |
| Stations | Last heard, software, last position |
| Radio Setup | Brand → model tree with setup steps, links and operators' field reports, from `ui/radio-guides.json`; works without the engine. How reports are added: `docs/radio-guides.md` |
| Sounds | Optional sound per event (chat to me, chat to everyone, message, file, failures, port lost/back, positions, update), each switchable, plus volume; saved on this computer |
| Diagnostics | Version and update check, engine log, wire capture start/stop per port, support bundle |

Not yet: the command palette, map, file picker for sending files (use `aetherdv-cli send-file`), settings and port setup (edit the TOML).

## Browser preview and smoke test

The same `ui/` runs in a browser for development: serve it and pass the engine address and token in the URL.

```bash
cd app/ui && python3 -m http.server 18080 --bind 127.0.0.1
# open http://127.0.0.1:18080/index.html?api=127.0.0.1:7373&token=<contents of aetherdv.token>
node app/tools/ui-smoke.js 127.0.0.1:7373 /path/to/aetherdv.token /tmp/shots
```

`ui-smoke.js` (Playwright) drives every view, sends a chat line, and fails on console errors or if any element was created from received text.

## Security notes

- Everything received from the air or a ratflector is untrusted. The UI inserts it only as text (`textContent`), never as HTML. The smoke test checks this with a hostile chat line.
- CSP: scripts from the app only (no inline script); WebSocket connections only to loopback addresses.
- The Tauri command `engine_endpoint` only accepts a loopback engine address. It only reads small `*.token` files.
