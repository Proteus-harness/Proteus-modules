//! SQLite is the canonical journal here, not a cache of JSONL.
use super::*;
use anyhow::{Context, ensure};
use ring::digest::{Context as HashContext, SHA256};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::PathBuf;
mod record;

const FILE: &str = "journal.sqlite";

#[derive(Default)]
pub(crate) struct Sqlite {
    writer: Option<Connection>,
}

fn open_read(dir: &Path) -> Result<Option<Connection>> {
    ensure!(
        !dir.join(crate::codec::JOURNAL_FILE).try_exists()?,
        "session contains a JSONL journal; select its storage implementation or explicitly migrate it"
    );
    let path = dir.join(FILE);
    if !path.try_exists()? {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute_batch("BEGIN DEFERRED;")?;
    Ok(Some(conn))
}

fn validate_identity(conn: &Connection, id: SessionId) -> Result<()> {
    let (schema, session): (u32, String) = conn.query_row(
        "SELECT schema_version, session_id FROM journal_identity WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        schema == JOURNAL_SCHEMA_VERSION && session == id.to_string(),
        "SQLite journal identity/schema mismatch"
    );
    Ok(())
}

fn snapshot_source(
    dir: &Path,
    id: SessionId,
    hint: Option<&PrefixHint>,
) -> Result<Box<dyn RecordSnapshot>> {
    let conn = open_read(dir)?;
    let mut hash = HashContext::new(&SHA256);
    let end = if let Some(conn) = &conn {
        validate_identity(conn, id)?;
        let end = u64::try_from(conn.query_row(
            "SELECT coalesce(max(sequence),0) FROM journal",
            [],
            |r| r.get::<_, i64>(0),
        )?)?;
        if let Some(hint) = hint {
            ensure!(
                end >= hint.extent,
                "SQLite journal shortened below cached prefix"
            );
            hint.extent
        } else {
            let mut stmt =
                conn.prepare("SELECT payload FROM journal WHERE sequence<=?1 ORDER BY sequence")?;
            let mut rows = stmt.query([i64::try_from(end)?])?;
            while let Some(row) = rows.next()? {
                let bytes: Vec<u8> = row.get(0)?;
                crate::diagnostics::fingerprint(bytes.len());
                hash.update(&bytes);
            }
            end
        }
    } else {
        0
    };
    let fingerprint = if let Some(hint) = hint {
        hint.fingerprint.clone()
    } else {
        hash.finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    Ok(Box::new(SqliteSnapshot {
        conn,
        end,
        after: 0,
        id,
        fingerprint,
        current_hash: HashContext::new(&SHA256),
    }))
}

impl JournalBackend for Sqlite {
    fn snapshot(&mut self, dir: &Path, id: SessionId) -> Result<Box<dyn RecordSnapshot>> {
        snapshot_source(dir, id, None)
    }
    fn snapshot_warm(
        &mut self,
        dir: &Path,
        id: SessionId,
        hint: &PrefixHint,
    ) -> Result<Box<dyn RecordSnapshot>> {
        snapshot_source(dir, id, Some(hint))
    }
    fn recover(&mut self, dir: &Path, id: SessionId) -> Result<()> {
        ensure!(
            !dir.join(crate::codec::JOURNAL_FILE).try_exists()?,
            "session contains a JSONL journal; storage implementations cannot silently replace it"
        );
        if self.writer.is_none() {
            let conn = Connection::open(dir.join(FILE))?;
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
            )?;
            conn.execute_batch("CREATE TABLE IF NOT EXISTS journal_identity (
                singleton INTEGER PRIMARY KEY CHECK(singleton=1), schema_version INTEGER NOT NULL, session_id TEXT NOT NULL
            ); CREATE TABLE IF NOT EXISTS journal (
                sequence INTEGER PRIMARY KEY CHECK(sequence>0), record_id TEXT UNIQUE NOT NULL, payload BLOB NOT NULL
            );")?;
            conn.execute("INSERT OR IGNORE INTO journal_identity(singleton,schema_version,session_id) VALUES(1,?1,?2)",
                params![JOURNAL_SCHEMA_VERSION, id.to_string()])?;
            validate_identity(&conn, id)?;
            self.writer = Some(conn);
        }
        let conn = self.writer.as_ref().expect("writer opened");
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        ensure!(
            check == "ok",
            "SQLite journal integrity check failed: {check}"
        );
        Ok(())
    }
    fn append(&mut self, _dir: &Path, record: &JournalRecord) -> Result<Append> {
        let conn = self
            .writer
            .as_mut()
            .context("SQLite writer must recover before append")?;
        let payload = record::encode(record)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("INSERT OR IGNORE INTO journal_identity(singleton,schema_version,session_id) VALUES(1,?1,?2)",
            params![JOURNAL_SCHEMA_VERSION, record.session_id.to_string()])?;
        validate_identity(&tx, record.session_id)?;
        let actual = u64::try_from(tx.query_row(
            "SELECT coalesce(max(sequence),0) FROM journal",
            [],
            |r| r.get::<_, i64>(0),
        )?)?;
        ensure!(
            record.session_seq == actual + 1,
            "SQLite append sequence conflict"
        );
        tx.execute(
            "INSERT INTO journal(sequence,record_id,payload) VALUES(?1,?2,?3)",
            params![
                i64::try_from(record.session_seq)?,
                record.record_id.to_string(),
                payload
            ],
        )?;
        tx.commit()?;
        Ok(Append {
            extent: record.session_seq,
            bytes: payload,
            blobs: Vec::new(),
        })
    }
    fn description_path(&self, dir: &Path) -> PathBuf {
        dir.join(FILE)
    }
}

struct SqliteSnapshot {
    conn: Option<Connection>,
    end: u64,
    after: u64,
    id: SessionId,
    fingerprint: String,
    current_hash: HashContext,
}
impl RecordSnapshot for SqliteSnapshot {
    fn reset(&mut self) -> Result<()> {
        self.after = 0;
        self.current_hash = HashContext::new(&SHA256);
        Ok(())
    }
    fn next(&mut self) -> Result<Option<JournalRecord>> {
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        let row: Option<(i64, Vec<u8>)> = conn.query_row(
            "SELECT sequence,payload FROM journal WHERE sequence>?1 AND sequence<=?2 ORDER BY sequence LIMIT 1",
            params![i64::try_from(self.after)?, i64::try_from(self.end)?], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((sequence, bytes)) = row else {
            let actual: String = self
                .current_hash
                .clone()
                .finish()
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            ensure!(actual == self.fingerprint, "SQLite snapshot prefix changed");
            return Ok(None);
        };
        crate::diagnostics::decoded();
        self.current_hash.update(&bytes);
        let sequence = u64::try_from(sequence)?;
        ensure!(
            bytes.len() <= crate::io::MAX_PAYLOAD_BYTES * 2,
            "SQLite record exceeds maximum"
        );
        let record = record::decode(&bytes)?;
        ensure!(
            record.session_seq == sequence
                && record.session_id == self.id
                && record.schema_version == JOURNAL_SCHEMA_VERSION,
            "SQLite journal record identity mismatch"
        );
        self.after = sequence;
        Ok(Some(record))
    }
    fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    fn read_hash(&self) -> HashContext {
        self.current_hash.clone()
    }
    fn extent(&self) -> u64 {
        self.end
    }
    fn prefix_fingerprint(&mut self, sequence: u64) -> Result<Option<String>> {
        if sequence > self.end {
            return Ok(None);
        }
        let mut hash = HashContext::new(&SHA256);
        if let Some(conn) = &self.conn {
            let mut stmt =
                conn.prepare("SELECT payload FROM journal WHERE sequence<=?1 ORDER BY sequence")?;
            let mut rows = stmt.query([i64::try_from(sequence)?])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                let bytes: Vec<u8> = row.get(0)?;
                hash.update(&bytes);
                count += 1;
            }
            ensure!(
                count == sequence,
                "SQLite journal prefix has a sequence gap"
            );
        }
        Ok(Some(
            hash.finish()
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        ))
    }
}
