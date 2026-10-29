//! `aether-store`: durable SQLite storage (ADR-0007).
//!
//! The schema is `spec/storage/*.sql` (validated by `check_schema.py`); this
//! crate embeds those migrations and applies them forward-only, backing the
//! database up before upgrading an existing file. Opened with WAL and
//! `synchronous=FULL`, so a returned insert has reached the disk.
//!
//! Timestamps are Unix milliseconds supplied by the caller.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

pub use rusqlite::Error;

/// Forward-only migrations, in order. The SQL files insert their own row
/// into `schema_migrations`.
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../../../spec/storage/0001_initial.sql"))];

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub id: i64,
    pub port_id: Option<String>,
    pub direction: Direction,
    pub src: String,
    pub dst: String,
    /// `text`, `ping_req`, `ping_rsp`, `echo_req`, `echo_rsp`, `status`, `raw`
    pub kind: String,
    pub body: String,
    pub at: i64,
    /// Outgoing only: `queued`, `transmitted`, `failed`. Never "delivered".
    pub tx_state: Option<String>,
    pub frame_crc: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRow {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    pub profile_id: Option<String>,
    pub device_ident: Option<String>,
    pub config_json: String,
    pub mode_policy: String,
}

/// One file or form transfer (`transfers` table). For the legacy protocols
/// `total_bytes`/`done_bytes` count the zlib stream, as D-RATS does, and
/// `sha256` is of the uncompressed object once it is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRow {
    pub id: String,
    pub message_id: Option<String>,
    /// `send` or `receive`
    pub direction: String,
    /// `legacy_file`, `legacy_form` or `alp`
    pub protocol: String,
    pub peer: String,
    pub port_id: Option<String>,
    pub filename: String,
    pub sha256: Option<String>,
    pub total_bytes: i64,
    pub wire_bytes: Option<i64>,
    pub block_size: i64,
    pub done_bytes: i64,
    pub state: String,
    pub state_reason: Option<String>,
    pub retries: i64,
    pub wrap_warning: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

fn chat_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatLine> {
    let dir: String = r.get(2)?;
    Ok(ChatLine {
        id: r.get(0)?,
        port_id: r.get(1)?,
        direction: if dir == "in" {
            Direction::In
        } else {
            Direction::Out
        },
        src: r.get(3)?,
        dst: r.get(4)?,
        kind: r.get(5)?,
        body: r.get(6)?,
        at: r.get(7)?,
        tx_state: r.get(8)?,
        frame_crc: r.get(9)?,
    })
}

/// A message (a D-RATS form document) and its delivery state (SM-7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub id: String,
    /// Legacy message id (`<path><mid>`).
    pub mid: String,
    /// `inbox`, `outbox`, `sent`, `drafts`, `archive`, `trash`
    pub folder: String,
    pub form_id: Option<String>,
    pub subject: Option<String>,
    pub src: String,
    pub dst: String,
    pub priority: String,
    pub xml: String,
    /// `draft`, `queued`, `sending`, `handed_off`, `delivered`,
    /// `unconfirmed`, `failed`, `expired`, `received`
    pub state: String,
    pub state_reason: Option<String>,
    pub attempts: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One append-only delivery event of a message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageEvent {
    pub id: i64,
    pub message_id: String,
    pub at: i64,
    pub state: String,
    pub hop: Option<String>,
    pub port_id: Option<String>,
    /// `rf_slow`, `rf_fast`, `internet`
    pub path_kind: Option<String>,
    /// `tcp_accepted`, `arq_ack`, `alp_complete`, `receipt`, `operator`, `none`
    pub evidence: Option<String>,
    /// JSON text
    pub detail: Option<String>,
}

const MESSAGE_COLS: &str = "id,mid,folder,form_id,subject,src,dst,priority,xml,state,state_reason,attempts,created_at,updated_at";

fn message_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: r.get(0)?,
        mid: r.get(1)?,
        folder: r.get(2)?,
        form_id: r.get(3)?,
        subject: r.get(4)?,
        src: r.get(5)?,
        dst: r.get(6)?,
        priority: r.get(7)?,
        xml: r.get(8)?,
        state: r.get(9)?,
        state_reason: r.get(10)?,
        attempts: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

/// One received or sent position report.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionRow {
    pub callsign: String,
    pub port_id: Option<String>,
    pub received_at: i64,
    pub fix_time: Option<i64>,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: Option<f64>,
    pub course: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub symbol: Option<String>,
    pub comment: Option<String>,
    /// `nmea`, `dprs`, `manual`, `alp`
    pub source_fmt: String,
    pub raw: Option<String>,
}

pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
}

impl Store {
    /// Open (creating if needed) and migrate. `None` path = in-memory (tests).
    pub fn open(path: Option<&Path>) -> Result<Store> {
        let conn = match path {
            Some(p) => Connection::open(p)?,
            None => Connection::open_in_memory()?,
        };
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        if path.is_some() {
            let mode: String =
                conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
            debug_assert_eq!(mode.to_lowercase(), "wal");
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        let mut s = Store {
            conn,
            path: path.map(Path::to_path_buf),
        };
        s.migrate()?;
        Ok(s)
    }

    pub fn schema_version(&self) -> Result<i64> {
        let exists: bool = self.conn.query_row(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(0);
        }
        self.conn.query_row(
            "SELECT coalesce(max(version), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )
    }

    fn migrate(&mut self) -> Result<()> {
        let current = self.schema_version()?;
        let pending: Vec<_> = MIGRATIONS.iter().filter(|(v, _)| *v > current).collect();
        if pending.is_empty() {
            return Ok(());
        }
        if current > 0 {
            if let Some(p) = &self.path {
                let backup = p.with_extension(format!("pre-v{}.bak", pending[0].0));
                self.conn
                    .execute("VACUUM INTO ?1", params![backup.to_string_lossy()])?;
            }
        }
        for (_, sql) in pending {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.commit()?;
        }
        Ok(())
    }

    // ---- ports ----------------------------------------------------------

    pub fn upsert_port(&self, p: &PortRow, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ports(id,kind,name,enabled,profile_id,device_ident,config,mode_policy,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)
             ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, name=excluded.name, enabled=excluded.enabled,
               profile_id=excluded.profile_id, device_ident=excluded.device_ident, config=excluded.config,
               mode_policy=excluded.mode_policy, updated_at=excluded.updated_at",
            params![p.id, p.kind, p.name, p.enabled, p.profile_id, p.device_ident, p.config_json, p.mode_policy, now],
        )?;
        Ok(())
    }

    pub fn ports(&self) -> Result<Vec<PortRow>> {
        let mut st = self.conn.prepare(
            "SELECT id,kind,name,enabled,profile_id,device_ident,config,mode_policy FROM ports ORDER BY name",
        )?;
        let rows = st.query_map([], |r| {
            Ok(PortRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                name: r.get(2)?,
                enabled: r.get(3)?,
                profile_id: r.get(4)?,
                device_ident: r.get(5)?,
                config_json: r.get(6)?,
                mode_policy: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    // ---- chat -------------------------------------------------------------

    pub fn insert_chat(&self, c: &ChatLine) -> Result<i64> {
        let dir = match c.direction {
            Direction::In => "in",
            Direction::Out => "out",
        };
        self.conn.execute(
            "INSERT INTO chat_lines(port_id,direction,src,dst,kind,body,at,tx_state,frame_crc)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                c.port_id,
                dir,
                c.src,
                c.dst,
                c.kind,
                c.body,
                c.at,
                c.tx_state,
                c.frame_crc
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_chat_tx_state(&self, id: i64, state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chat_lines SET tx_state=?2 WHERE id=?1",
            params![id, state],
        )?;
        Ok(())
    }

    pub fn chat_line(&self, id: i64) -> Result<Option<ChatLine>> {
        let mut st = self.conn.prepare(
            "SELECT id,port_id,direction,src,dst,kind,body,at,tx_state,frame_crc FROM chat_lines WHERE id=?1",
        )?;
        st.query_row(params![id], chat_row).optional()
    }

    /// Newest first. `port_id = None` gives the merged "All traffic" view.
    pub fn chat_history(
        &self,
        port_id: Option<&str>,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<ChatLine>> {
        let mut st = self.conn.prepare(
            "SELECT id,port_id,direction,src,dst,kind,body,at,tx_state,frame_crc FROM chat_lines
             WHERE (?1 IS NULL OR port_id = ?1) AND (?2 IS NULL OR id < ?2)
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = st.query_map(params![port_id, before_id, limit], chat_row)?;
        rows.collect()
    }

    // ---- stations -----------------------------------------------------------

    pub fn station_heard(
        &self,
        callsign: &str,
        port_id: &str,
        path_kind: &str,
        now: i64,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO stations(callsign, first_heard, last_heard) VALUES (?1, ?2, ?2)
             ON CONFLICT(callsign) DO UPDATE SET last_heard = excluded.last_heard",
            params![callsign, now],
        )?;
        tx.execute(
            "INSERT INTO station_paths(callsign, port_id, path_kind, last_heard, frames) VALUES (?1,?2,?3,?4,1)
             ON CONFLICT(callsign, port_id, path_kind) DO UPDATE SET last_heard = excluded.last_heard, frames = frames + 1",
            params![callsign, port_id, path_kind, now],
        )?;
        tx.commit()
    }

    /// The port `callsign` was most recently heard on, if any.
    pub fn station_last_port(&self, callsign: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT port_id FROM station_paths WHERE callsign=?1 ORDER BY last_heard DESC LIMIT 1",
                params![callsign],
                |r| r.get(0),
            )
            .optional()
    }

    pub fn station_software(&self, callsign: &str, software: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE stations SET software=?2 WHERE callsign=?1",
            params![callsign, software],
        )?;
        Ok(())
    }

    /// (callsign, last_heard, software)
    pub fn stations(&self) -> Result<Vec<(String, i64, Option<String>)>> {
        let mut st = self.conn.prepare(
            "SELECT callsign,last_heard,software FROM stations ORDER BY last_heard DESC",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    }

    // ---- messages -----------------------------------------------------------

    /// Insert a message together with its first event (same state).
    pub fn insert_message(&self, m: &MessageRow, ev: &MessageEvent) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            &format!("INSERT INTO messages({MESSAGE_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"),
            params![
                m.id,
                m.mid,
                m.folder,
                m.form_id,
                m.subject,
                m.src,
                m.dst,
                m.priority,
                m.xml,
                m.state,
                m.state_reason,
                m.attempts,
                m.created_at,
                m.updated_at
            ],
        )?;
        Self::insert_event(&tx, &m.id, ev)?;
        tx.commit()
    }

    fn insert_event(conn: &Connection, message_id: &str, e: &MessageEvent) -> Result<()> {
        conn.execute(
            "INSERT INTO message_events(message_id,at,state,hop,port_id,path_kind,evidence,detail)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                message_id,
                e.at,
                e.state,
                e.hop,
                e.port_id,
                e.path_kind,
                e.evidence,
                e.detail
            ],
        )?;
        Ok(())
    }

    /// Record a state change: appends `ev` and updates the message (and its
    /// folder, if given) in one transaction. `ev.state` is the new state.
    pub fn set_message_state(
        &self,
        id: &str,
        folder: Option<&str>,
        reason: Option<&str>,
        bump_attempts: bool,
        ev: &MessageEvent,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        Self::insert_event(&tx, id, ev)?;
        let n = tx.execute(
            "UPDATE messages SET state=?2, state_reason=?3, folder=COALESCE(?4, folder),
                attempts = attempts + ?5, updated_at=?6 WHERE id=?1",
            params![id, ev.state, reason, folder, bump_attempts as i64, ev.at],
        )?;
        if n != 1 {
            return Err(Error::QueryReturnedNoRows);
        }
        tx.commit()
    }

    /// Replace a draft's content.
    pub fn update_draft(
        &self,
        id: &str,
        xml: &str,
        subject: Option<&str>,
        dst: &str,
        now: i64,
    ) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE messages SET xml=?2, subject=?3, dst=?4, updated_at=?5 WHERE id=?1 AND state='draft'",
            params![id, xml, subject, dst, now],
        )?;
        Ok(n == 1)
    }

    pub fn message(&self, id: &str) -> Result<Option<MessageRow>> {
        self.conn
            .query_row(
                &format!("SELECT {MESSAGE_COLS} FROM messages WHERE id=?1"),
                params![id],
                message_row,
            )
            .optional()
    }

    pub fn message_by_mid(&self, mid: &str) -> Result<Option<MessageRow>> {
        self.conn
            .query_row(
                &format!("SELECT {MESSAGE_COLS} FROM messages WHERE mid=?1"),
                params![mid],
                message_row,
            )
            .optional()
    }

    /// Newest first; all folders if `folder` is `None`.
    pub fn messages(
        &self,
        folder: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<MessageRow>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages WHERE (?1 IS NULL OR folder=?1)
             ORDER BY created_at DESC, id DESC LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = st.query_map(params![folder, limit, offset], message_row)?;
        rows.collect()
    }

    pub fn message_events(&self, id: &str) -> Result<Vec<MessageEvent>> {
        let mut st = self.conn.prepare(
            "SELECT id,message_id,at,state,hop,port_id,path_kind,evidence,detail FROM message_events
             WHERE message_id=?1 ORDER BY id",
        )?;
        let rows = st.query_map(params![id], |r| {
            Ok(MessageEvent {
                id: r.get(0)?,
                message_id: r.get(1)?,
                at: r.get(2)?,
                state: r.get(3)?,
                hop: r.get(4)?,
                port_id: r.get(5)?,
                path_kind: r.get(6)?,
                evidence: r.get(7)?,
                detail: r.get(8)?,
            })
        })?;
        rows.collect()
    }

    // ---- positions ----------------------------------------------------------

    pub fn insert_position(&self, p: &PositionRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO positions(callsign,port_id,received_at,fix_time,lat,lon,alt_m,course,speed_kmh,symbol,comment,source_fmt,raw)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                p.callsign,
                p.port_id,
                p.received_at,
                p.fix_time,
                p.lat,
                p.lon,
                p.alt_m,
                p.course,
                p.speed_kmh,
                p.symbol,
                p.comment,
                p.source_fmt,
                p.raw
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Oldest first. `callsign = None`: the latest report of every station.
    pub fn positions(
        &self,
        callsign: Option<&str>,
        since: Option<i64>,
    ) -> Result<Vec<PositionRow>> {
        let sql = match callsign {
            Some(_) => "SELECT callsign,port_id,received_at,fix_time,lat,lon,alt_m,course,speed_kmh,symbol,comment,source_fmt,raw
                 FROM positions WHERE callsign=?1 AND received_at >= ?2 ORDER BY received_at, id",
            None => "SELECT callsign,port_id,received_at,fix_time,lat,lon,alt_m,course,speed_kmh,symbol,comment,source_fmt,raw
                 FROM positions p WHERE received_at >= ?2 AND id = (SELECT MAX(id) FROM positions q WHERE q.callsign = p.callsign)
                 AND (?1 IS NULL) ORDER BY callsign",
        };
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map(params![callsign, since.unwrap_or(0)], |r| {
            Ok(PositionRow {
                callsign: r.get(0)?,
                port_id: r.get(1)?,
                received_at: r.get(2)?,
                fix_time: r.get(3)?,
                lat: r.get(4)?,
                lon: r.get(5)?,
                alt_m: r.get(6)?,
                course: r.get(7)?,
                speed_kmh: r.get(8)?,
                symbol: r.get(9)?,
                comment: r.get(10)?,
                source_fmt: r.get(11)?,
                raw: r.get(12)?,
            })
        })?;
        rows.collect()
    }

    // ---- transfers ----------------------------------------------------------

    /// Insert or update by id. `created_at` is kept from the first insert and
    /// `sha256` is never cleared once set.
    pub fn upsert_transfer(&self, t: &TransferRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO transfers(id,direction,protocol,peer,port_id,filename,sha256,total_bytes,wire_bytes,
                block_size,done_bytes,state,state_reason,retries,wrap_warning,created_at,updated_at,message_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
             ON CONFLICT(id) DO UPDATE SET filename=excluded.filename,
                sha256=COALESCE(excluded.sha256, transfers.sha256), total_bytes=excluded.total_bytes,
                wire_bytes=excluded.wire_bytes, done_bytes=excluded.done_bytes, state=excluded.state,
                state_reason=excluded.state_reason, retries=excluded.retries,
                wrap_warning=excluded.wrap_warning, updated_at=excluded.updated_at,
                message_id=COALESCE(excluded.message_id, transfers.message_id)",
            params![
                t.id,
                t.direction,
                t.protocol,
                t.peer,
                t.port_id,
                t.filename,
                t.sha256,
                t.total_bytes,
                t.wire_bytes,
                t.block_size,
                t.done_bytes,
                t.state,
                t.state_reason,
                t.retries,
                t.wrap_warning as i64,
                t.created_at,
                t.updated_at,
                t.message_id
            ],
        )?;
        Ok(())
    }

    /// Newest first; finished ones (done/failed/cancelled) only if asked.
    pub fn transfers(&self, include_finished: bool, limit: u32) -> Result<Vec<TransferRow>> {
        let mut st = self.conn.prepare(
            "SELECT id,direction,protocol,peer,port_id,filename,sha256,total_bytes,wire_bytes,block_size,
                done_bytes,state,state_reason,retries,wrap_warning,created_at,updated_at,message_id FROM transfers
             WHERE ?1 OR state NOT IN ('done','failed','cancelled')
             ORDER BY created_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![include_finished, limit], |r| {
            Ok(TransferRow {
                id: r.get(0)?,
                direction: r.get(1)?,
                protocol: r.get(2)?,
                peer: r.get(3)?,
                port_id: r.get(4)?,
                filename: r.get(5)?,
                sha256: r.get(6)?,
                total_bytes: r.get(7)?,
                wire_bytes: r.get(8)?,
                block_size: r.get(9)?,
                done_bytes: r.get(10)?,
                state: r.get(11)?,
                state_reason: r.get(12)?,
                retries: r.get(13)?,
                wrap_warning: r.get::<_, i64>(14)? != 0,
                created_at: r.get(15)?,
                updated_at: r.get(16)?,
                message_id: r.get(17)?,
            })
        })?;
        rows.collect()
    }

    // ---- engine events ------------------------------------------------------

    pub fn log_event(
        &self,
        at: i64,
        level: &str,
        category: &str,
        port_id: Option<&str>,
        message: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO engine_events(at,level,category,port_id,message) VALUES (?1,?2,?3,?4,?5)",
            params![at, level, category, port_id, message],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Engine log, newest first: (id, at, level, category, port_id, message).
    #[allow(clippy::type_complexity)]
    pub fn events(
        &self,
        before_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<(i64, i64, String, String, Option<String>, String)>> {
        let mut st = self.conn.prepare(
            "SELECT id,at,level,category,port_id,message FROM engine_events
             WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![before_id, limit], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        rows.collect()
    }

    pub fn last_event(&self) -> Result<Option<(i64, String)>> {
        self.conn
            .query_row(
                "SELECT id, message FROM engine_events ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(body: &str, dir: Direction, port: &str) -> ChatLine {
        ChatLine {
            id: 0,
            port_id: Some(port.into()),
            direction: dir.clone(),
            src: "KK4ODA".into(),
            dst: "CQCQCQ".into(),
            kind: "text".into(),
            body: body.into(),
            at: 1,
            tx_state: (dir == Direction::Out).then(|| "queued".into()),
            frame_crc: None,
        }
    }

    fn port(id: &str, kind: &str) -> PortRow {
        PortRow {
            id: id.into(),
            kind: kind.into(),
            name: id.into(),
            enabled: true,
            profile_id: (kind == "radio_serial").then(|| "icom-ic9700".into()),
            device_ident: None,
            config_json: "{}".into(),
            mode_policy: "slow".into(),
        }
    }

    #[test]
    fn migrates_fresh_db() {
        let s = Store::open(None).unwrap();
        assert_eq!(s.schema_version().unwrap(), 1);
    }

    #[test]
    fn chat_roundtrip_and_states() {
        let s = Store::open(None).unwrap();
        s.upsert_port(&port("p1", "ratflector"), 0).unwrap();
        let id = s.insert_chat(&line("hello", Direction::Out, "p1")).unwrap();
        s.insert_chat(&line("hi", Direction::In, "p1")).unwrap();
        s.set_chat_tx_state(id, "transmitted").unwrap();
        let h = s.chat_history(Some("p1"), None, 10).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[1].tx_state.as_deref(), Some("transmitted"));
        assert!(
            s.set_chat_tx_state(id, "delivered").is_err(),
            "schema forbids 'delivered' chat"
        );
    }

    #[test]
    fn persists_across_reopen_and_reopen_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aether.db");
        {
            let s = Store::open(Some(&path)).unwrap();
            s.upsert_port(&port("p1", "radio_serial"), 0).unwrap();
            s.insert_chat(&line("durable", Direction::Out, "p1"))
                .unwrap();
        }
        let s = Store::open(Some(&path)).unwrap();
        assert_eq!(s.schema_version().unwrap(), 1);
        assert_eq!(s.chat_history(None, None, 10).unwrap()[0].body, "durable");
        assert_eq!(
            s.ports().unwrap()[0].profile_id.as_deref(),
            Some("icom-ic9700")
        );
    }

    #[test]
    fn station_paths_accumulate() {
        let s = Store::open(None).unwrap();
        s.upsert_port(&port("p1", "ratflector"), 0).unwrap();
        s.station_heard("W4DRS", "p1", "internet", 10).unwrap();
        s.station_heard("W4DRS", "p1", "internet", 20).unwrap();
        s.station_software("W4DRS", "D-RATS 0.4.2").unwrap();
        assert_eq!(
            s.stations().unwrap(),
            vec![("W4DRS".into(), 20, Some("D-RATS 0.4.2".into()))]
        );
    }

    #[test]
    fn transfer_upsert_keeps_hash_and_filters_finished() {
        let s = Store::open(None).unwrap();
        s.upsert_port(&port("p1", "ratflector"), 0).unwrap();
        let mut t = TransferRow {
            id: "0190f0a0-0000-7000-8000-000000000001".into(),
            message_id: None,
            direction: "receive".into(),
            protocol: "legacy_file".into(),
            peer: "W4DRS".into(),
            port_id: Some("p1".into()),
            filename: "report.txt".into(),
            sha256: None,
            total_bytes: 900,
            wire_bytes: Some(900),
            block_size: 512,
            done_bytes: 0,
            state: "sending".into(),
            state_reason: None,
            retries: 0,
            wrap_warning: false,
            created_at: 5,
            updated_at: 5,
        };
        s.upsert_transfer(&t).unwrap();
        assert_eq!(s.transfers(false, 10).unwrap().len(), 1);
        t.sha256 = Some("ab".repeat(32));
        t.done_bytes = 900;
        t.state = "done".into();
        t.updated_at = 9;
        s.upsert_transfer(&t).unwrap();
        t.sha256 = None;
        s.upsert_transfer(&t).unwrap();
        assert!(s.transfers(false, 10).unwrap().is_empty());
        let all = s.transfers(true, 10).unwrap();
        assert_eq!(all[0].sha256.as_deref(), Some(&*"ab".repeat(32)));
        assert_eq!((all[0].created_at, all[0].updated_at), (5, 9));
        t.state = "receiving".into();
        assert!(s.upsert_transfer(&t).is_err(), "schema state enum");
    }

    #[test]
    fn message_lifecycle_and_delivery_guard() {
        let s = Store::open(None).unwrap();
        let m = MessageRow {
            id: "m1".into(),
            mid: "KK4ODA.1.2".into(),
            folder: "outbox".into(),
            form_id: Some("ICS213_US_OS".into()),
            subject: Some("cots".into()),
            src: "KK4ODA".into(),
            dst: "W4DRS".into(),
            priority: "routine".into(),
            xml: "<xml/>".into(),
            state: "queued".into(),
            state_reason: None,
            attempts: 0,
            created_at: 1,
            updated_at: 1,
        };
        let ev = |state: &str, at: i64, evidence: Option<&str>| MessageEvent {
            at,
            state: state.into(),
            evidence: evidence.map(str::to_string),
            ..MessageEvent::default()
        };
        s.insert_message(&m, &ev("queued", 1, None)).unwrap();
        assert!(
            s.insert_message(&m, &ev("queued", 1, None)).is_err(),
            "unique id/mid"
        );
        s.set_message_state("m1", None, None, true, &ev("sending", 2, None))
            .unwrap();
        s.set_message_state(
            "m1",
            Some("sent"),
            None,
            false,
            &ev("handed_off", 3, Some("arq_ack")),
        )
        .unwrap();
        // A legacy hand-off is not end-to-end evidence: 'delivered' is refused.
        assert!(s
            .set_message_state(
                "m1",
                None,
                None,
                false,
                &ev("delivered", 4, Some("arq_ack"))
            )
            .is_err());
        let got = s.message("m1").unwrap().unwrap();
        assert_eq!(
            (got.state.as_str(), got.folder.as_str(), got.attempts),
            ("handed_off", "sent", 1)
        );
        assert_eq!(s.message_by_mid("KK4ODA.1.2").unwrap().unwrap().id, "m1");
        let evs = s.message_events("m1").unwrap();
        assert_eq!(
            evs.iter().map(|e| e.state.as_str()).collect::<Vec<_>>(),
            vec!["queued", "sending", "handed_off"]
        );
        assert_eq!(s.messages(Some("sent"), 10, 0).unwrap().len(), 1);
        assert!(s.messages(Some("inbox"), 10, 0).unwrap().is_empty());
        assert!(s
            .set_message_state("nope", None, None, false, &ev("failed", 5, None))
            .is_err());
        // Self-addressed sends are refused by the schema (P17).
        let mut own = m.clone();
        own.id = "m2".into();
        own.mid = "KK4ODA.1.3".into();
        own.dst = "KK4ODA".into();
        assert!(s.insert_message(&own, &ev("queued", 1, None)).is_err());
    }

    #[test]
    fn positions_history_and_latest() {
        let s = Store::open(None).unwrap();
        let pos = |call: &str, at: i64, lat: f64| PositionRow {
            callsign: call.into(),
            port_id: None,
            received_at: at,
            fix_time: None,
            lat,
            lon: -84.0,
            alt_m: None,
            course: None,
            speed_kmh: None,
            symbol: Some("/>".into()),
            comment: None,
            source_fmt: "dprs".into(),
            raw: None,
        };
        s.insert_position(&pos("W4DRS", 1, 34.0)).unwrap();
        s.insert_position(&pos("W4DRS", 5, 34.5)).unwrap();
        s.insert_position(&pos("K7TAY", 3, 45.0)).unwrap();
        assert!(
            s.insert_position(&pos("BAD", 3, 95.0)).is_err(),
            "schema range check"
        );
        let h = s.positions(Some("W4DRS"), None).unwrap();
        assert_eq!(
            h.iter().map(|p| p.lat).collect::<Vec<_>>(),
            vec![34.0, 34.5]
        );
        assert_eq!(s.positions(Some("W4DRS"), Some(2)).unwrap().len(), 1);
        let latest = s.positions(None, None).unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(
            (latest[1].callsign.as_str(), latest[1].lat),
            ("W4DRS", 34.5)
        );
    }
}
