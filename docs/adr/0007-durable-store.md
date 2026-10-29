# ADR-0007: SQLite (WAL) durable store with append-only delivery journal

Status: Proposed · Date: 2026-10-29

## Decision
A single SQLite database per station profile. WAL mode; `synchronous=FULL` on queue and journal writes. Attachments are content-addressed files. Every delivery-state change is appended to `message_events`, which is the exportable traffic record. Schema migrations are versioned and forward-only, with an automatic backup before migrating.

## Rationale
Replaces the XML-folder + lock-file outbox (stale locks: DR#122; corrupt 0-byte forms: DR#76). Gives crash and power-loss recovery and queryable history.

## Validation
Power-cut simulation (kill -9 and fsync fault injection) shows no lost queued message and no duplicate transmission beyond the documented at-least-once semantics.
