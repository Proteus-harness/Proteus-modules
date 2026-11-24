//! SQLite is the canonical journal here, not a cache of JSONL.
use super::*;
use anyhow::{Context, ensure};
use ring::digest::{Context as HashContext, SHA256};
use rusqlite::{Connection, OpenFlags, params};
use std::path::PathBuf;
mod record;

const FILE: &str = "journal.sqlite";
// Exact schema text of the built-in writer (SQLite drops IF NOT EXISTS).
// Anything else, including hand-built compatible tables, uses the seek reader.
const WRITER_JOURNAL_SCHEMA: &str = "CREATE TABLE journal (
                sequence INTEGER PRIMARY KEY CHECK(sequence>0), record_id TEXT UNIQUE NOT NULL, payload BLOB NOT NULL
            )";

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

// The payload is only needed while the row is live, for hashing or decoding.
fn payload_bytes<'r>(row: &'r rusqlite::Row<'_>, column: usize) -> rusqlite::Result<&'r [u8]> {
    let value = row.get_ref(column)?;
    value.as_blob().map_err(|_| {
        rusqlite::Error::InvalidColumnType(column, "payload".into(), value.data_type())
    })
}

fn open_source(dir: &Path, id: SessionId) -> Result<(Option<Connection>, u64)> {
    let conn = open_read(dir)?;
    let end = if let Some(conn) = &conn {
        validate_identity(conn, id)?;
        u64::try_from(conn.query_row(
            "SELECT coalesce(max(sequence),0) FROM journal",
            [],
            |r| r.get::<_, i64>(0),
        )?)?
    } else {
        0
    };
    Ok((conn, end))
}

fn snapshot_source(
    conn: Option<Connection>,
    id: SessionId,
    mut end: u64,
    hint: Option<&PrefixHint>,
) -> Result<SqliteSnapshot> {
    let mut hash = HashContext::new(&SHA256);
    if let Some(conn) = &conn {
        if let Some(hint) = hint {
            ensure!(
                end >= hint.extent,
                "SQLite journal shortened below cached prefix"
            );
            end = hint.extent;
        } else {
            let mut stmt =
                conn.prepare("SELECT payload FROM journal WHERE sequence<=?1 ORDER BY sequence")?;
            let mut rows = stmt.query([i64::try_from(end)?])?;
            while let Some(row) = rows.next()? {
                let bytes = payload_bytes(row, 0)?;
                crate::diagnostics::fingerprint(bytes.len());
                hash.update(bytes);
            }
        }
    }
    let fingerprint = if let Some(hint) = hint {
        hint.fingerprint.clone()
    } else {
        crate::io::digest_hex(hash.finish())
    };
    Ok(SqliteSnapshot {
        conn,
        end,
        after: 0,
        id,
        fingerprint,
        current_hash: HashContext::new(&SHA256),
    })
}

impl Sqlite {
    /// Cold catalog only. Hand back proof after every ordered row has passed
    /// the visitor and EOF is reached, without exposing an unverified snapshot.
    pub(crate) fn replay_catalog<T: Default>(
        dir: &Path,
        id: SessionId,
        apply: impl Fn(&mut T, &JournalRecord) -> Result<()>,
    ) -> Result<(T, PrefixHint)> {
        let (conn, end) = open_source(dir, id)?;
        if let Some(replayed) = replay_catalog_rows(conn.as_ref(), id, end, &apply)? {
            return Ok(replayed);
        }

        // Unexpected ordering falls back on the same pinned transaction.
        // Start a new fold: no partial result can escape the cursor path.
        let mut source = snapshot_source(conn, id, end, None)?;
        let mut folded = T::default();
        while let Some(record) = source.next()? {
            apply(&mut folded, &record)?;
        }
        Ok((
            folded,
            PrefixHint {
                extent: source.end,
                fingerprint: source.fingerprint,
            },
        ))
    }
}

fn replay_catalog_rows<T: Default>(
    conn: Option<&Connection>,
    id: SessionId,
    end: u64,
    apply: impl Fn(&mut T, &JournalRecord) -> Result<()>,
) -> Result<Option<(T, PrefixHint)>> {
    let mut folded = T::default();
    let mut hash = HashContext::new(&SHA256);
    if let Some(conn) = conn {
        let ordinary_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema
                           WHERE type='table' AND name='journal' AND rootpage>0)",
            [],
            |row| row.get(0),
        )?;
        if !ordinary_table {
            return Ok(None);
        }
        let mut stmt = conn
            .prepare("SELECT sequence,payload FROM journal WHERE sequence<=?1 ORDER BY sequence")?;
        let mut rows = stmt.query([i64::try_from(end)?])?;
        let mut seen = 0;
        while let Some(row) = rows.next()? {
            seen += 1;
            // The legacy seek cursor visits positive, distinct sequences.
            // Do not assume an arbitrary existing table enforces the writer DDL.
            let sequence = match row.get::<_, i64>(0) {
                Ok(sequence) if u64::try_from(sequence) == Ok(seen) => seen,
                Ok(_) | Err(rusqlite::Error::InvalidColumnType(..)) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let bytes = payload_bytes(row, 1)?;
            hash.update(bytes);
            apply(&mut folded, &decode_record(id, sequence, bytes)?)?;
        }
        if seen != end {
            return Ok(None);
        }
    }
    Ok(Some((
        folded,
        PrefixHint {
            extent: end,
            fingerprint: crate::io::digest_hex(hash.finish()),
        },
    )))
}

impl JournalBackend for Sqlite {
    fn snapshot(&mut self, dir: &Path, id: SessionId) -> Result<Box<dyn RecordSnapshot>> {
        let (conn, end) = open_source(dir, id)?;
        Ok(Box::new(snapshot_source(conn, id, end, None)?))
    }
    fn snapshot_warm(
        &mut self,
        dir: &Path,
        id: SessionId,
        hint: &PrefixHint,
    ) -> Result<Box<dyn RecordSnapshot>> {
        let (conn, end) = open_source(dir, id)?;
        Ok(Box::new(snapshot_source(conn, id, end, Some(hint))?))
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
        let mut stmt = conn.prepare_cached(
            "SELECT sequence,payload FROM journal WHERE sequence>?1 AND sequence<=?2 ORDER BY sequence LIMIT 1",
        )?;
        let mut rows = stmt.query(params![
            i64::try_from(self.after)?,
            i64::try_from(self.end)?
        ])?;
        let Some(row) = rows.next()? else {
            let actual: String = crate::io::digest_hex(self.current_hash.clone().finish());
            ensure!(actual == self.fingerprint, "SQLite snapshot prefix changed");
            return Ok(None);
        };
        let sequence: i64 = row.get(0)?;
        let bytes = payload_bytes(row, 1)?;
        self.current_hash.update(bytes);
        let sequence = u64::try_from(sequence)?;
        let record = decode_record(self.id, sequence, bytes)?;
        self.after = sequence;
        Ok(Some(record))
    }
    fn try_visit_display_rows(
        &mut self,
        cut: Option<u64>,
        visit: &mut DisplayRowVisitor<'_>,
    ) -> Result<bool> {
        let Some(conn) = &self.conn else {
            return Ok(false);
        };
        if conn.is_autocommit() || self.after != 0 || !writer_journal_for_display(conn) {
            return Ok(false);
        }

        // The exact writer table has one integral row per sequence. Match the
        // seek reader's positive range; sequence gaps still fail in the fold.
        let mut stmt = conn.prepare(
            "SELECT sequence,payload FROM journal WHERE sequence>0 AND sequence<=?1 ORDER BY sequence",
        )?;
        let mut rows = stmt.query([i64::try_from(self.end)?])?;
        while let Some(row) = rows.next()? {
            let sequence = u64::try_from(row.get::<_, i64>(0)?)?;
            let bytes = payload_bytes(row, 1)?;
            self.current_hash.update(bytes);
            let record = decode_record(self.id, sequence, bytes)?;
            self.after = record.session_seq;
            let boundary = (cut == Some(self.after))
                .then(|| crate::io::digest_hex(self.current_hash.clone().finish()));
            visit(&record, boundary)?;
        }
        let actual = crate::io::digest_hex(self.current_hash.clone().finish());
        ensure!(actual == self.fingerprint, "SQLite snapshot prefix changed");
        Ok(true)
    }
    fn consumed_fingerprint(&self) -> String {
        crate::io::digest_hex(self.current_hash.clone().finish())
    }
    fn confirm_verified_data(&self) -> Result<bool> {
        // The open read transaction pins both the payloads and their order.
        Ok(true)
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
                hash.update(payload_bytes(row, 0)?);
                count += 1;
            }
            ensure!(
                count == sequence,
                "SQLite journal prefix has a sequence gap"
            );
        }
        Ok(Some(crate::io::digest_hex(hash.finish())))
    }
}

fn writer_journal_for_display(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema
                       WHERE type='table' AND name='journal' AND rootpage>0 AND sql=?1)
             AND NOT EXISTS(SELECT 1 FROM temp.sqlite_schema WHERE name='journal')",
        [WRITER_JOURNAL_SCHEMA],
        |row| row.get(0),
    )
    .unwrap_or(false)
}

fn decode_record(id: SessionId, sequence: u64, bytes: &[u8]) -> Result<JournalRecord> {
    crate::diagnostics::decoded();
    ensure!(
        bytes.len() <= crate::io::MAX_PAYLOAD_BYTES * 2,
        "SQLite record exceeds maximum"
    );
    let record = record::decode(bytes)?;
    ensure!(
        record.session_seq == sequence
            && record.session_id == id
            && record.schema_version == JOURNAL_SCHEMA_VERSION,
        "SQLite journal record identity mismatch"
    );
    Ok(record)
}

#[cfg(test)]
#[test]
fn payload_bytes_matches_owned_blob_reads_and_errors_for_both_projections() {
    let conn = Connection::open_in_memory().unwrap();
    for (columns, index) in [("payload", 0), ("1 AS sequence, payload", 1)] {
        let sql = format!(
            "SELECT {columns} FROM (SELECT x'00ff' AS payload UNION ALL SELECT x'' \
             UNION ALL SELECT 'text' UNION ALL SELECT 1 UNION ALL SELECT 1.5 UNION ALL SELECT NULL)"
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            assert_eq!(
                payload_bytes(row, index).map(<[u8]>::to_vec),
                row.get::<_, Vec<u8>>(index)
            );
        }
    }
}
