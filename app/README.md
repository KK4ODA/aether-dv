# Aether DV desktop app

A Tauri 2 shell around a plain HTML/CSS/JS interface (`ui/`). The interface talks only to the engine, `aetherd`, over its local API (JSON-RPC over WebSocket, token required). It never opens radios, files or the database itself (ADR-0001).

## Run it

1. Start the engine with your configuration: `aetherd --config my.toml`. It writes `aetherd.token` next to its database.
2. Start the app: `cargo run -p aether-desktop` (or the packaged build).
3. If the app doesn't find the token automatically (`AETHER_TOKEN_FILE`, or `aetherd.token` in the temp directory), enter the token file's path on the connect screen. Only the path is remembered, never the token.

## What works (Phase C minimal UI)

| View | |
|---|---|
| Chat (launch screen) | Per-port channels plus "All traffic"; live lines; send to everyone or one station; honest send states (Queued, Transmitted, Not sent; never "delivered" for chat); position reports shown as positions; Heard and Waiting-to-send panels |
| Messages | Inbox, outbox, sent and drafts; ICS-213 rendered as the paper form; delivery ladder; compose and send a new ICS-213; retry a failed message |
| Transfers | All transfers with progress and outcome; cancel |
| Stations | Last heard, software, last position |
| Diagnostics | Engine log, wire capture start/stop per port, support bundle |

Not yet: the command palette, map, file picker for sending files (use `aether-cli send-file`), settings and port setup (edit the TOML).

## Browser preview and smoke test

The same `ui/` runs in a browser for development: serve it and pass the engine address and token in the URL.

```bash
cd app/ui && python3 -m http.server 18080 --bind 127.0.0.1
# open http://127.0.0.1:18080/index.html?api=127.0.0.1:7373&token=<contents of aetherd.token>
node app/tools/ui-smoke.js 127.0.0.1:7373 /path/to/aetherd.token /tmp/shots
```

`ui-smoke.js` (Playwright) drives every view, sends a chat line, and fails on console errors or if any element was created from received text.

## Security notes

- Everything received from the air or a ratflector is untrusted. The UI inserts it only as text (`textContent`), never as HTML. The smoke test checks this with a hostile chat line.
- CSP: scripts from the app only (no inline script); WebSocket connections only to loopback addresses.
- The Tauri command `engine_endpoint` only accepts a loopback engine address. It only reads small `*.token` files.
