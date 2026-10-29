-- SPDX-License-Identifier: GPL-3.0-or-later
-- Aether DV storage schema v0, migration 0001 (Phase B draft, ADR-0007).
-- Target: SQLite >= 3.37 (STRICT tables). The engine opens with:
--   PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;
-- Conventions: ids are TEXT UUIDv7 (time-ordered) unless noted; timestamps are
-- INTEGER milliseconds since the Unix epoch, UTC; JSON columns are TEXT
-- validated with json_valid(); enums are TEXT with CHECK constraints.

CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY,
    applied_at  INTEGER NOT NULL,
    description TEXT    NOT NULL
) STRICT;

CREATE TABLE settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL CHECK (json_valid(value)),
    updated_at  INTEGER NOT NULL
) STRICT;

-- Local station identity (one row per configured identity; normally one).
CREATE TABLE station_identity (
    id          TEXT PRIMARY KEY,
    callsign    TEXT NOT NULL CHECK (length(callsign) BETWEEN 3 AND 8),  -- DDT2 station field is 8 bytes
    full_ident  TEXT NOT NULL,      -- may exceed 8 chars (e.g. with SSID); carried in ALP payloads
    is_default  INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0,1)),
    created_at  INTEGER NOT NULL
) STRICT;

-- Physical/virtual links: radios and ratflector connections.
CREATE TABLE ports (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('radio_serial','radio_bt','ratflector','sim')),
    name          TEXT NOT NULL UNIQUE,
    enabled       INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    profile_id    TEXT,               -- hardware profile key (profiles/*.toml), radio kinds only
    device_ident  TEXT,               -- stable identity: USB serial, by-id path, BT address
    config        TEXT NOT NULL CHECK (json_valid(config)),  -- typed per kind (baud, flow, host, port...)
    mode_policy   TEXT NOT NULL DEFAULT 'slow' CHECK (mode_policy IN ('slow','fast','auto')),
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    CHECK ((kind IN ('radio_serial','radio_bt')) = (profile_id IS NOT NULL))
) STRICT;

CREATE TABLE ratflector_servers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    host        TEXT NOT NULL,
    tcp_port    INTEGER NOT NULL DEFAULT 9000 CHECK (tcp_port BETWEEN 1 AND 65535),
    source      TEXT NOT NULL CHECK (source IN ('directory','user')),
    favourite   INTEGER NOT NULL DEFAULT 0 CHECK (favourite IN (0,1)),
    needs_auth  INTEGER NOT NULL DEFAULT 0 CHECK (needs_auth IN (0,1)),
    -- passwords are stored in the OS keychain, never here
    description TEXT,
    last_seen_active INTEGER,
    UNIQUE (host, tcp_port)
) STRICT;

-- Remote stations and how/where they were heard.
CREATE TABLE stations (
    callsign      TEXT PRIMARY KEY,   -- as seen in DDT2 (<= 8 chars, '~' stripped)
    first_heard   INTEGER NOT NULL,
    last_heard    INTEGER NOT NULL,
    software      TEXT,               -- from ping/version replies
    status_code   INTEGER,            -- legacy chat type 5 status
    status_text   TEXT,
    notes         TEXT
) STRICT;

CREATE TABLE station_paths (
    callsign    TEXT NOT NULL REFERENCES stations(callsign) ON DELETE CASCADE,
    port_id     TEXT NOT NULL REFERENCES ports(id) ON DELETE CASCADE,
    path_kind   TEXT NOT NULL CHECK (path_kind IN ('rf_slow','rf_fast','rf_unknown','internet')),
    last_heard  INTEGER NOT NULL,
    frames      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (callsign, port_id, path_kind)
) STRICT;

CREATE TABLE peer_capabilities (
    callsign      TEXT NOT NULL REFERENCES stations(callsign) ON DELETE CASCADE,
    port_id       TEXT NOT NULL REFERENCES ports(id) ON DELETE CASCADE,
    software      TEXT NOT NULL CHECK (software IN ('unknown','legacy_drats','aether')),
    alp_version   TEXT,
    fd_state      TEXT NOT NULL DEFAULT 'unknown'
                  CHECK (fd_state IN ('unknown','declared','declared_required','proven','stale','disproven')),
    fd_state_at   INTEGER,
    fd_source     TEXT,               -- 'operator' | 'capability_record' | 'profile' | 'evidence'
    record        TEXT CHECK (record IS NULL OR json_valid(record)),  -- raw RPCGetVersion dict
    updated_at    INTEGER NOT NULL,
    PRIMARY KEY (callsign, port_id)
) STRICT;

CREATE TABLE positions (
    id          INTEGER PRIMARY KEY,  -- rowid; high volume
    callsign    TEXT NOT NULL,
    port_id     TEXT REFERENCES ports(id) ON DELETE SET NULL,
    received_at INTEGER NOT NULL,
    fix_time    INTEGER,
    lat         REAL NOT NULL CHECK (lat BETWEEN -90 AND 90),
    lon         REAL NOT NULL CHECK (lon BETWEEN -180 AND 180),
    alt_m       REAL,
    course      REAL,
    speed_kmh   REAL,
    symbol      TEXT,
    comment     TEXT,
    source_fmt  TEXT NOT NULL CHECK (source_fmt IN ('nmea','dprs','manual','alp')),
    raw         TEXT
) STRICT;
CREATE INDEX positions_by_call_time ON positions(callsign, received_at DESC);

-- Chat lines (lightweight, high volume). Broadcast chat has no delivery evidence.
CREATE TABLE chat_lines (
    id          INTEGER PRIMARY KEY,
    port_id     TEXT REFERENCES ports(id) ON DELETE SET NULL,
    direction   TEXT NOT NULL CHECK (direction IN ('in','out')),
    src         TEXT NOT NULL,
    dst         TEXT NOT NULL,        -- 'CQCQCQ' for broadcast
    kind        TEXT NOT NULL CHECK (kind IN ('text','ping_req','ping_rsp','echo_req','echo_rsp','status','raw')),
    body        TEXT NOT NULL,
    at          INTEGER NOT NULL,
    tx_state    TEXT CHECK (tx_state IN ('queued','transmitted','failed')),  -- 'out' only; never 'delivered'
    receipt_at  INTEGER,              -- ALP receipt (Aether peers only)
    frame_crc   INTEGER,              -- DDT2 checksum, for cross-port dedup
    CHECK ((direction = 'out') = (tx_state IS NOT NULL))
) STRICT;
CREATE INDEX chat_by_port_time ON chat_lines(port_id, at);

-- Content-addressed attachments; bytes live in the attachment store directory.
CREATE TABLE blobs (
    sha256      TEXT PRIMARY KEY CHECK (length(sha256) = 64),
    size        INTEGER NOT NULL CHECK (size >= 0),
    mime        TEXT,
    stored_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE form_templates (
    id          TEXT PRIMARY KEY,     -- legacy form id, e.g. 'ICS213_US_OS'
    title       TEXT NOT NULL,
    version     TEXT NOT NULL,
    xml         TEXT NOT NULL,        -- legacy-compatible form XML (template)
    builtin     INTEGER NOT NULL DEFAULT 0 CHECK (builtin IN (0,1))
) STRICT;

-- Store-and-forward messages (forms). One row per message object.
CREATE TABLE messages (
    id            TEXT PRIMARY KEY,
    mid           TEXT NOT NULL UNIQUE,   -- legacy message id CALL.hex.hex (path/mid)
    folder        TEXT NOT NULL CHECK (folder IN ('inbox','outbox','sent','drafts','archive','trash')),
    form_id       TEXT,
    subject       TEXT,
    src           TEXT NOT NULL,
    dst           TEXT NOT NULL,          -- callsign, 'A;B;C' gratuitous route, email, 'WL2K:...'
    priority      TEXT NOT NULL DEFAULT 'routine' CHECK (priority IN ('routine','priority','immediate','flash')),
    xml           TEXT NOT NULL,          -- canonical legacy XML incl. <path>
    state         TEXT NOT NULL CHECK (state IN ('draft','queued','sending','handed_off','delivered','unconfirmed','failed','expired','received')),
    state_reason  TEXT,
    attempts      INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER,
    expires_at    INTEGER,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    CHECK (dst <> src OR state IN ('draft','received'))   -- self-addressed sends refused (P17)
) STRICT;
CREATE INDEX messages_queue ON messages(state, next_attempt_at) WHERE state IN ('queued','sending');

CREATE TABLE message_attachments (
    message_id  TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    sha256      TEXT NOT NULL REFERENCES blobs(sha256),
    filename    TEXT NOT NULL,
    PRIMARY KEY (message_id, sha256)
) STRICT;

-- Append-only delivery/traffic journal (exportable record of traffic).
CREATE TABLE message_events (
    id          INTEGER PRIMARY KEY,
    message_id  TEXT NOT NULL REFERENCES messages(id) ON DELETE RESTRICT,
    at          INTEGER NOT NULL,
    state       TEXT NOT NULL,
    hop         TEXT,                     -- next-hop callsign or ratflector name
    port_id     TEXT,
    path_kind   TEXT CHECK (path_kind IS NULL OR path_kind IN ('rf_slow','rf_fast','internet')),
    evidence    TEXT CHECK (evidence IS NULL OR evidence IN ('tcp_accepted','arq_ack','alp_complete','receipt','operator','none')),
    detail      TEXT CHECK (detail IS NULL OR json_valid(detail))
) STRICT;
CREATE INDEX message_events_by_msg ON message_events(message_id, at);
CREATE TRIGGER message_events_no_update BEFORE UPDATE ON message_events
BEGIN SELECT RAISE(ABORT, 'message_events is append-only'); END;
CREATE TRIGGER message_events_no_delete BEFORE DELETE ON message_events
BEGIN SELECT RAISE(ABORT, 'message_events is append-only'); END;

-- 'delivered' requires end-to-end evidence (SM-7); enforce at the storage layer too.
CREATE TRIGGER messages_delivered_needs_evidence BEFORE UPDATE OF state ON messages
WHEN NEW.state = 'delivered' AND NOT EXISTS (
    SELECT 1 FROM message_events e
    WHERE e.message_id = NEW.id AND e.evidence IN ('alp_complete','receipt'))
BEGIN SELECT RAISE(ABORT, 'delivered requires alp_complete or receipt evidence'); END;

-- File/form transfers (one per session attempt chain).
CREATE TABLE transfers (
    id            TEXT PRIMARY KEY,
    direction     TEXT NOT NULL CHECK (direction IN ('send','receive')),
    protocol      TEXT NOT NULL CHECK (protocol IN ('legacy_file','legacy_form','alp')),
    peer          TEXT NOT NULL,
    port_id       TEXT REFERENCES ports(id) ON DELETE SET NULL,
    message_id    TEXT REFERENCES messages(id) ON DELETE SET NULL,
    filename      TEXT NOT NULL,
    sha256        TEXT CHECK (sha256 IS NULL OR length(sha256) = 64),  -- of the uncompressed object
    total_bytes   INTEGER NOT NULL CHECK (total_bytes >= 0),
    wire_bytes    INTEGER,                -- legacy: compressed stream length from the offer
    block_size    INTEGER NOT NULL CHECK (block_size BETWEEN 16 AND 4096),
    done_bytes    INTEGER NOT NULL DEFAULT 0,
    chunk_bitmap  BLOB,                   -- received/acked blocks (ALP); NULL for legacy
    state         TEXT NOT NULL CHECK (state IN ('queued','opening','offering','sending','polling','suspended','done','failed','cancelled')),
    state_reason  TEXT,
    priority      INTEGER NOT NULL DEFAULT 50 CHECK (priority BETWEEN 0 AND 100),
    retries       INTEGER NOT NULL DEFAULT 0,
    wrap_warning  INTEGER NOT NULL DEFAULT 0 CHECK (wrap_warning IN (0,1)),  -- legacy >256 blocks (P19)
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    CHECK (done_bytes <= total_bytes OR protocol <> 'alp')
) STRICT;
CREATE INDEX transfers_active ON transfers(state) WHERE state NOT IN ('done','failed','cancelled');

-- Engine event log shown in the activity drawer (ring-trimmed by the engine).
CREATE TABLE engine_events (
    id        INTEGER PRIMARY KEY,
    at        INTEGER NOT NULL,
    level     TEXT NOT NULL CHECK (level IN ('debug','info','warn','error')),
    category  TEXT NOT NULL,
    port_id   TEXT,
    message   TEXT NOT NULL,
    detail    TEXT CHECK (detail IS NULL OR json_valid(detail))
) STRICT;
CREATE INDEX engine_events_at ON engine_events(at);

-- Measured link statistics per port and mode (feeds ModeProfile tuning).
CREATE TABLE link_stats (
    id            INTEGER PRIMARY KEY,
    port_id       TEXT NOT NULL REFERENCES ports(id) ON DELETE CASCADE,
    mode          TEXT NOT NULL CHECK (mode IN ('slow','fast','internet')),
    window_start  INTEGER NOT NULL,
    window_end    INTEGER NOT NULL,
    tx_bytes      INTEGER NOT NULL DEFAULT 0,
    rx_bytes      INTEGER NOT NULL DEFAULT 0,
    goodput_bps   REAL,
    retx_ratio    REAL,
    rtt_ms_p50    REAL,
    rtt_ms_p95    REAL,
    keyup_ms_p50  REAL,
    tail_ms_p50   REAL
) STRICT;

INSERT INTO schema_migrations(version, applied_at, description)
VALUES (1, CAST(strftime('%s','now') AS INTEGER) * 1000, 'initial schema v0');
