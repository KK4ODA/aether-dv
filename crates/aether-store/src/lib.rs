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
        let rows = st.query_map(params![port_id, before_id, limit], |r| {
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
        })?;
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
}
