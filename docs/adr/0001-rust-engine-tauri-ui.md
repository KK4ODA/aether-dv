# ADR-0001: Rust engine as a separate process + Tauri 2 desktop UI

Status: **Accepted** 2026-10-29 (owner decision) · Proposed: 2026-10-29

## Context
The RF engine must survive UI failure, run headless on a Raspberry Pi, and share code with the ratflector server. It needs reliable serial I/O, explicit concurrency, memory safety, and painless installers on Windows, macOS and Linux. The legacy Python/GTK stack failed on packaging (no native Windows build, MSYS2/MobaXterm workarounds), on accessibility on macOS (DR#315), and on headless operation (DR#241). drats2 stalled on Python packaging boilerplate.

## Decision
- A Rust workspace provides every non-UI component (codec, link, radio, store, ratflector client/server, sim).
- The engine `aetherdv` is its own OS process. The UI talks to it over a versioned local JSON-RPC + event API.
- The desktop shell is Tauri 2 with a TypeScript UI. Maps use MapLibre GL with offline tiles.

## Alternatives
- PySide6 (largest contributor pool, good a11y; but packaging, GIL timing, two runtimes).
- Native Rust GUI (Slint/egui/iced: no mature offline map; Slint licence terms).
- C++/Qt (no memory safety).

## Consequences
+ One language for engine, server and tests; single static binaries; deterministic sans-I/O cores.
+ UI replaceable (TUI, web) without touching RF.
− Rust raises the contributor bar. Mitigated with a language-neutral protocol spec and test vectors.
− WebKitGTK variance on Linux. Mitigated with an AppImage pinned runtime and a minimum-distro policy.

## Validation
Phase C spike: a serial loopback at 9600/38400/115200 on all three OSes with USB unplug/replug, plus Tauri packaging of an empty shell for all targets in CI, before committing.

## Validation record (2026-10-29, Phase B)
- Serial: `serialport` exclusive open, binary transparency, unplug detection and stable-path reopen verified on Linux PTYs. macOS PTYs reject the baud ioctl, so macOS needs a real device (open item).
- Tauri 2 shell built and launched on Linux, and packaged on Linux, macOS and Windows in GitHub Actions (all green, unsigned).
- Full test matrix green on Linux, macOS 14 and Windows 2022.
