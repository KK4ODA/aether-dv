# spec/: machine-checkable specifications (Phase B)

| Path | What | Check |
|---|---|---|
| `api/aether-api-v0.openrpc.json` | Engine ↔ UI API v0 (OpenRPC 1.3, JSON-RPC 2.0 + event stream) | `python3 spec/api/check_api.py` |
| `storage/0001_initial.sql` | SQLite schema v0, migration 0001 | `python3 spec/storage/check_schema.py` (applies the migration and exercises 20 constraint/trigger checks) |
| `models/legacy_arq_check.py` | Exhaustive model checker for the legacy ARQ and the ALP span rule | see `docs/phase-b/arq-model-findings.md` |

Design rules encoded in these specs (traceable to `docs/phase-b/state-machines.md`):

- **Honest delivery.** The API has no "delivered" chat state. A message can only be marked `delivered` if its append-only journal holds `alp_complete` or `receipt` evidence; a SQLite trigger enforces this.
- **Durability.** `messages.send` returns only after the queue entry is committed (WAL, `synchronous=FULL`). Traffic journals (`message_events`) are append-only, and messages with a journal cannot be hard-deleted. Retention works by archiving and exporting (`messages.export`), never by silent deletion.
- **No blind probing.** `devices.list` never opens a port. CI-V reads and writes (`ports.readRadioMode`/`setRadioMode`) are gated by the profile's `fdControl` evidence level and operator confirmation (ADR-0005).
- **Reconnect without loss.** `events.subscribe(sinceCursor)` replays events missed while the UI was disconnected.
- **Self-addressed traffic is refused** (legacy P17) by both the API (error 1422) and a schema CHECK.
- **Legacy wrap risk is visible.** Transfers to legacy peers longer than 256 blocks carry `wrapWarning` (legacy defect P19).
- Passwords never touch the database (OS keychain only).

Versioning: the API follows semver. `system.hello` rejects a major mismatch (error 1426). Storage migrations are forward-only, numbered `NNNN_*.sql`, and the engine backs up the DB before migrating.
