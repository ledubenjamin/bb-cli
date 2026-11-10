use super::{
    BLOCK_SIZE, LogIdentity, LogLine, LogMatch, LogQuery, LogWindow, RETENTION, now, text,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug)]
pub(super) struct Database {
    pub connection: Connection,
    path: PathBuf,
    quota: u64,
}

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS entities (
 scope TEXT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL, payload TEXT NOT NULL,
 updated INTEGER NOT NULL, PRIMARY KEY(scope,kind,key)
);
CREATE TABLE IF NOT EXISTS catalog (
 scope TEXT NOT NULL, workspace TEXT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL,
 project TEXT, search_text TEXT NOT NULL, sort_name TEXT NOT NULL, payload TEXT NOT NULL,
 updated INTEGER NOT NULL, PRIMARY KEY(scope,workspace,kind,key)
);
CREATE INDEX IF NOT EXISTS catalog_listing ON catalog(scope,workspace,kind,sort_name,key);
CREATE TABLE IF NOT EXISTS logs (
 id INTEGER PRIMARY KEY, scope TEXT NOT NULL, workspace TEXT NOT NULL, repository TEXT NOT NULL,
 run_uuid TEXT NOT NULL, run_attempt INTEGER NOT NULL, step_uuid TEXT NOT NULL, attempt INTEGER NOT NULL,
 identity TEXT NOT NULL, total INTEGER, complete INTEGER NOT NULL DEFAULT 0, updated INTEGER NOT NULL,
 UNIQUE(scope,repository,run_uuid,run_attempt,step_uuid,attempt)
);
CREATE INDEX IF NOT EXISTS logs_scope ON logs(scope,workspace,repository,updated);
CREATE TABLE IF NOT EXISTS blocks (
 log_id INTEGER NOT NULL REFERENCES logs(id) ON DELETE CASCADE,
 start INTEGER NOT NULL, data BLOB NOT NULL, PRIMARY KEY(log_id,start)
);
CREATE TABLE IF NOT EXISTS coverage (
 log_id INTEGER NOT NULL REFERENCES logs(id) ON DELETE CASCADE,
 start INTEGER NOT NULL, end INTEGER NOT NULL, PRIMARY KEY(log_id,start)
);
CREATE TABLE IF NOT EXISTS lines (
 id INTEGER PRIMARY KEY, log_id INTEGER NOT NULL REFERENCES logs(id) ON DELETE CASCADE,
 start INTEGER NOT NULL, end INTEGER NOT NULL, text TEXT NOT NULL, UNIQUE(log_id,start)
);
CREATE VIRTUAL TABLE IF NOT EXISTS log_fts USING fts5(text,content='lines',content_rowid='id',tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS lines_insert AFTER INSERT ON lines BEGIN
 INSERT INTO log_fts(rowid,text) VALUES(new.id,new.text);
END;
CREATE TRIGGER IF NOT EXISTS lines_delete AFTER DELETE ON lines BEGIN
 INSERT INTO log_fts(log_fts,rowid,text) VALUES('delete',old.id,old.text);
END;
CREATE TRIGGER IF NOT EXISTS lines_update AFTER UPDATE ON lines BEGIN
 INSERT INTO log_fts(log_fts,rowid,text) VALUES('delete',old.id,old.text);
 INSERT INTO log_fts(rowid,text) VALUES(new.id,new.text);
END;
PRAGMA user_version=2;
";

impl Database {
    pub fn open(path: &Path, quota: u64) -> Result<Self> {
        let directory = path.parent().context("cache path has no parent")?;
        fs::create_dir_all(directory).context("cannot create cache directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        };
        #[cfg(not(unix))]
        OpenOptions::new().create(true).append(true).open(path)?;
        let _writer = writer_lock(path)?;
        let connection = Connection::open(path).context("cannot open log cache")?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(version <= 2, "log cache belongs to a newer bb version");
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA wal_autocheckpoint=128; PRAGMA journal_size_limit=0; PRAGMA secure_delete=ON;")?;
        // Reserve half the disk budget for WAL and maintenance while another process reads.
        let page_size = connection.pragma_query_value(None, "page_size", |r| number(r, 0))?;
        connection.pragma_update(
            None,
            "max_page_count",
            Integer((quota / 2 / page_size).max(128)),
        )?;
        connection.execute_batch(SCHEMA)?;
        let mut db = Self {
            connection,
            path: path.to_owned(),
            quota,
        };
        db.maintain(0, None)?;
        Ok(db)
    }

    pub fn save(&mut self, scope: &str, kind: &str, key: &str, payload: &str) -> Result<()> {
        ensure!(
            payload.len() <= 16 * 1024 * 1024,
            "cached resource is too large"
        );
        let _writer = writer_lock(&self.path)?;
        self.maintain(payload.len() as u64 * 2, None)?;
        self.connection.execute("INSERT INTO entities VALUES(?1,?2,?3,?4,?5) ON CONFLICT(scope,kind,key) DO UPDATE SET payload=excluded.payload,updated=excluded.updated",params![scope,kind,key,payload,Integer(now())])?;
        Ok(())
    }

    pub fn load(&self, scope: &str, kind: &str, key: &str) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT payload FROM entities WHERE scope=?1 AND kind=?2 AND key=?3",
                params![scope, kind, key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn index_catalog(
        &mut self,
        scope: &str,
        workspace: &str,
        items: &[crate::discovery::CatalogItem],
        updated: u64,
    ) -> Result<()> {
        let records: Vec<_> = items
            .iter()
            .map(|item| Ok((item, serde_json::to_string(item)?, item.search_text())))
            .collect::<Result<_>>()?;
        let size: usize = records
            .iter()
            .map(|(_, payload, text)| payload.len() + text.len())
            .sum();
        ensure!(size <= 16 * 1024 * 1024, "catalog page is too large");
        let _writer = writer_lock(&self.path)?;
        self.maintain(size as u64 * 2, None)?;
        let transaction = self.connection.transaction()?;
        {
            let mut insert = transaction.prepare_cached("INSERT INTO catalog VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                ON CONFLICT(scope,workspace,kind,key) DO UPDATE SET project=excluded.project,search_text=excluded.search_text,sort_name=excluded.sort_name,payload=excluded.payload,updated=excluded.updated
                WHERE excluded.updated >= catalog.updated")?;
            for (item, payload, text) in records {
                insert.execute(params![
                    scope,
                    workspace,
                    item.kind().name(),
                    item.key(),
                    item.project(),
                    text,
                    item.name().to_lowercase(),
                    payload,
                    Integer(updated)
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn find_catalog(
        &self,
        scope: &str,
        query: &crate::discovery::CatalogQuery,
    ) -> Result<super::CachedPage<crate::discovery::CatalogItem>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT payload,updated FROM catalog
            WHERE scope=?1 AND workspace=?2 AND kind=?3 AND (?4 IS NULL OR project=?4)
            AND instr(search_text,?5)>0 AND updated>=?6 ORDER BY sort_name,key LIMIT 10001",
        )?;
        let mut rows = statement.query(params![
            scope,
            query.workspace,
            query.kind.name(),
            query.project,
            query.text.trim().to_lowercase(),
            Integer(now().saturating_sub(RETENTION))
        ])?;
        let mut values = Vec::new();
        let mut updated_at = 0;
        while let Some(row) = rows.next()? {
            values.push(serde_json::from_str(&row.get::<_, String>(0)?)?);
            updated_at = updated_at.max(number(row, 1)?);
        }
        ensure!(
            values.len() <= 10_000,
            "more than 10,000 cached results; refine the search"
        );
        Ok(super::CachedPage {
            values,
            next: None,
            updated_at,
        })
    }

    pub fn seed_catalog(&mut self, scope: &str, workspace: &str) -> Result<()> {
        if self.load(scope, "catalog_seed", workspace)?.is_some() {
            return Ok(());
        }
        // Upgrade existing navigation snapshots once per account and workspace.
        let snapshots = self.connection.prepare("SELECT kind,payload,updated FROM entities WHERE scope=?1
            AND ((kind=?3 AND key=?2) OR (kind=?4 AND substr(key,1,length(?2)+1)=?2||':')) ORDER BY updated")?
            .query_map(params![scope,workspace,super::EntityKind::Projects.name(),super::EntityKind::Repositories.name()], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,number(row,2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (kind, payload, updated) in snapshots {
            let items = if kind == "repositories" {
                serde_json::from_str::<super::CachedPage<bb_api::models::Repository>>(&payload)?
                    .values
                    .into_iter()
                    .map(|repository| {
                        crate::discovery::CatalogItem::Repository(Box::new(repository))
                    })
                    .collect::<Vec<_>>()
            } else {
                serde_json::from_str::<super::CachedPage<bb_api::models::Project>>(&payload)?
                    .values
                    .into_iter()
                    .map(crate::discovery::CatalogItem::Project)
                    .collect()
            };
            self.index_catalog(scope, workspace, &items, updated)?;
        }
        self.save(scope, "catalog_seed", workspace, "{}")
    }

    pub fn restart(&mut self, scope: &str, identity: &LogIdentity) -> Result<()> {
        let _writer = writer_lock(&self.path)?;
        if let Some(id) = log_id(&self.connection, scope, identity)? {
            self.connection
                .execute("DELETE FROM logs WHERE id=?1", [id])?;
        }
        Ok(())
    }

    pub fn put(
        &mut self,
        scope: &str,
        identity: &LogIdentity,
        offset: u64,
        bytes: &[u8],
        total: Option<u64>,
    ) -> Result<()> {
        ensure!(
            bytes.len() <= BLOCK_SIZE,
            "log chunks must not exceed 64 KiB"
        );
        let end = offset
            .checked_add(bytes.len() as u64)
            .context("log offset overflow")?;
        ensure!(
            i64::try_from(end).is_ok() && total.is_none_or(|n| n >= end),
            "invalid remote log range"
        );
        // Maintenance and the following write form one cross-process operation.
        // SQLite serializes transactions, but quota checks precede the transaction.
        let _writer = writer_lock(&self.path)?;
        let protected = log_id(&self.connection, scope, identity)?;
        self.maintain((bytes.len() as u64).saturating_mul(24), protected)?;
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let identity_json = serde_json::to_string(identity)?;
        tx.execute("INSERT INTO logs(scope,workspace,repository,run_uuid,run_attempt,step_uuid,attempt,identity,updated) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(scope,repository,run_uuid,run_attempt,step_uuid,attempt) DO UPDATE SET identity=excluded.identity,updated=excluded.updated",params![scope,identity.workspace,identity.repository,identity.run_uuid,Integer(identity.run_attempt),identity.step_uuid,Integer(identity.step_attempt),identity_json,Integer(now())])?;
        let id = log_id(&tx, scope, identity)?.context("log disappeared during transaction")?;
        let previous: Option<u64> =
            tx.query_row("SELECT total FROM logs WHERE id=?1", [id], |r| {
                optional_number(r, 0)
            })?;
        let mut rewritten = previous.zip(total).is_some_and(|(old, new)| new < old);
        let existing = ranges(&tx, id)?;
        // The first known total can reveal truncation of previously unbounded data.
        rewritten |= total.is_some_and(|size| existing.iter().any(|(_, end)| *end > size));
        for (start, stop) in &existing {
            let left = (*start).max(offset);
            let right = (*stop).min(end);
            if left < right
                && read_bytes(&tx, id, left, right)?
                    != bytes[usize::try_from(left - offset)?..usize::try_from(right - offset)?]
            {
                rewritten = true;
                break;
            }
        }
        if rewritten {
            clear_content(&tx, id)?;
            tx.execute("UPDATE logs SET total=NULL,complete=0 WHERE id=?1", [id])?;
        }
        if !bytes.is_empty() {
            write_blocks(&tx, id, offset, bytes)?;
            let mut left = offset;
            let mut right = end;
            for (start, stop) in ranges(&tx, id)? {
                if stop >= left && start <= right {
                    left = left.min(start);
                    right = right.max(stop);
                }
            }
            tx.execute(
                "DELETE FROM coverage WHERE log_id=?1 AND end>=?2 AND start<=?3",
                params![id, Integer(left), Integer(right)],
            )?;
            tx.execute(
                "INSERT INTO coverage VALUES(?1,?2,?3)",
                params![id, Integer(left), Integer(right)],
            )?;
            let old_start: Option<u64> = tx.query_row(
                "SELECT min(start) FROM lines WHERE log_id=?1 AND start<=?2 AND end>=?2",
                params![id, Integer(offset)],
                |r| optional_number(r, 0),
            )?;
            let old_end: Option<u64> = tx.query_row(
                "SELECT max(end) FROM lines WHERE log_id=?1 AND start<=?2 AND end>=?2",
                params![id, Integer(end)],
                |r| optional_number(r, 0),
            )?;
            let index_start = old_start.unwrap_or(offset).max(left);
            let index_end = old_end.unwrap_or(end).min(right);
            let raw = read_bytes(&tx, id, index_start, index_end)?;
            tx.execute(
                "DELETE FROM lines WHERE log_id=?1 AND start>=?2 AND start<?3",
                params![id, Integer(index_start), Integer(index_end)],
            )?;
            let mut insert = tx.prepare("INSERT INTO lines(log_id,start,end,text) VALUES(?1,?2,?3,?4) ON CONFLICT(log_id,start) DO UPDATE SET end=excluded.end,text=excluded.text")?;
            for line in text::lines(&raw, index_start) {
                insert.execute(params![
                    id,
                    Integer(line.start),
                    Integer(line.end),
                    line.text
                ])?;
            }
        }
        tx.execute(
            "UPDATE logs SET total=coalesce(?2,total),complete=0 WHERE id=?1",
            params![id, total.map(Integer)],
        )?;
        tx.execute("UPDATE logs SET complete=1 WHERE id=?1 AND total IS NOT NULL AND (total=0 OR EXISTS(SELECT 1 FROM coverage WHERE log_id=?1 AND start=0 AND end>=logs.total))",[id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn window(
        &mut self,
        scope: &str,
        identity: &LogIdentity,
        from: Option<u64>,
        query: Option<&LogQuery>,
    ) -> Result<LogWindow> {
        // One read snapshot keeps indexed text, source bytes, and coverage coherent
        // even when another bb process rewrites this log between statements.
        let snapshot = self.connection.transaction()?;
        let Some(id) = log_id(&snapshot, scope, identity)? else {
            return Ok(LogWindow::default());
        };
        let (total, complete, updated_at) = snapshot.query_row(
            "SELECT total,complete,updated FROM logs WHERE id=?1",
            [id],
            |r| Ok((optional_number(r, 0)?, r.get(1)?, number(r, 2)?)),
        )?;
        let sql = if from.is_some() {
            "SELECT start,end,text,id FROM lines WHERE log_id=?1 AND end>=?2 ORDER BY start LIMIT 2000"
        } else {
            "SELECT start,end,text,id FROM lines WHERE log_id=?1 AND start>=?2 ORDER BY start DESC LIMIT 2000"
        };
        let mut statement = snapshot.prepare(sql)?;
        let rows = statement.query_map(params![id, Integer(from.unwrap_or(0))], |row| {
            Ok((
                row.get::<_, i64>(3)?,
                LogLine {
                    start: number(row, 0)?,
                    end: number(row, 1)?,
                    text: row.get(2)?,
                    highlights: Vec::new(),
                },
            ))
        })?;
        let mut lines = Vec::new();
        let mut line_ids = Vec::new();
        let mut text_bytes = 0;
        for row in rows {
            let (line_id, row) = row?;
            text_bytes += row.text.len();
            if text_bytes > 2 * 1024 * 1024 {
                break;
            }
            line_ids.push(line_id);
            lines.push(row);
        }
        if let Some(query) = query {
            highlight_lines(&snapshot, id, &line_ids, &mut lines, query)?;
        }
        if from.is_none() {
            lines.reverse();
        }
        // Long-line search fragments overlap; rendering displays each source byte once.
        let mut prior_end = 0;
        let mut rendered: Vec<LogLine> = Vec::with_capacity(lines.len());
        for mut line in lines {
            if line.end <= prior_end {
                continue;
            }
            if line.start < prior_end {
                let raw = read_bytes(&snapshot, id, line.start, line.end)?;
                let cleaned = text::after(&raw, usize::try_from(prior_end - line.start)?);
                let cleaned = cleaned.trim_end_matches('\n');
                let removed = line.text.len().saturating_sub(cleaned.len());
                // A match crossing a fragment boundary is indexed only in the
                // overlapping fragment. Carry its prefix back to the preceding row.
                if let Some(previous) = rendered.last_mut()
                    && previous.text.ends_with(&line.text[..removed])
                {
                    let base = previous.text.len() - removed;
                    previous.highlights.extend(
                        line.highlights
                            .iter()
                            .filter(|span| span.start < removed)
                            .map(|span| base + span.start..base + span.end.min(removed)),
                    );
                    previous.highlights.sort_by_key(|span| span.start);
                    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
                    for span in previous.highlights.drain(..) {
                        if let Some(last) = merged.last_mut()
                            && last.end >= span.start
                        {
                            last.end = last.end.max(span.end);
                        } else {
                            merged.push(span);
                        }
                    }
                    previous.highlights = merged;
                }
                line.highlights = line
                    .highlights
                    .into_iter()
                    .filter_map(|span| {
                        (span.end > removed)
                            .then(|| span.start.saturating_sub(removed)..span.end - removed)
                    })
                    .collect();
                cleaned.clone_into(&mut line.text);
                line.start = prior_end;
            }
            prior_end = line.end;
            rendered.push(line);
        }
        Ok(LogWindow {
            lines: rendered,
            ranges: ranges(&snapshot, id)?,
            total,
            complete,
            updated_at,
        })
    }

    pub fn search(
        &mut self,
        scope: &str,
        workspace: &str,
        repository: Option<&str>,
        step: Option<&LogIdentity>,
        query: &LogQuery,
        limit: u32,
    ) -> Result<Vec<LogMatch>> {
        // One read snapshot keeps indexed text, source bytes, and coverage coherent
        // even when another bb process rewrites this log between statements.
        let snapshot = self.connection.transaction()?;
        let mut statement = snapshot.prepare("SELECT l.identity,n.start,n.text,l.complete,l.updated,l.id,n.end,highlight(log_fts,0,char(30),char(31)) FROM log_fts JOIN lines n ON n.id=log_fts.rowid JOIN logs l ON l.id=n.log_id WHERE log_fts MATCH ?1 AND l.scope=?2 AND l.workspace=?3 AND (?4 IS NULL OR l.repository=?4) AND (?6 IS NULL OR (l.run_uuid=?6 AND l.run_attempt=?7 AND l.step_uuid=?8 AND l.attempt=?9)) ORDER BY l.updated DESC,l.id DESC,n.start LIMIT ?5")?;
        let rows = statement.query_map(
            params![
                query.literal,
                scope,
                workspace,
                repository,
                limit.saturating_mul(2),
                step.map(|s| &s.run_uuid),
                step.map(|s| Integer(s.run_attempt)),
                step.map(|s| &s.step_uuid),
                step.map(|s| Integer(s.step_attempt))
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    number(r, 1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                    number(r, 4)?,
                    r.get::<_, i64>(5)?,
                    number(r, 6)?,
                    r.get::<_, String>(7)?,
                ))
            },
        )?;
        let mut result = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for row in rows {
            let (identity, start, line, log_complete, updated_at, id, end, marked) = row?;
            let Some(span) = text::highlights(&marked).into_iter().next() else {
                continue;
            };
            let raw = read_bytes(&snapshot, id, start, end)?;
            let Some(position) = text::raw_offset(&raw, span.start) else {
                continue;
            };
            let byte_offset = start + position as u64;
            if !seen.insert((id, byte_offset)) {
                continue;
            }
            let character = line[..span.start].chars().count().saturating_sub(60);
            let text: String = line.chars().skip(character).take(1600).collect();
            result.push(LogMatch {
                identity: serde_json::from_str(&identity)?,
                byte_offset,
                text,
                log_complete,
                updated_at,
            });
            if result.len() >= limit as usize {
                break;
            }
        }
        Ok(result)
    }

    pub fn clear(&mut self, scope: &str, workspace: &str) -> Result<usize> {
        let _writer = writer_lock(&self.path)?;
        let removed = self.connection.execute(
            "DELETE FROM logs WHERE scope=?1 AND workspace=?2",
            params![scope, workspace],
        )?;
        self.connection
            .execute_batch("PRAGMA incremental_vacuum; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(removed)
    }

    fn maintain(&mut self, reserve: u64, protected: Option<i64>) -> Result<()> {
        let cutoff = now().saturating_sub(RETENTION);
        self.connection
            .execute("DELETE FROM logs WHERE updated<?1", [Integer(cutoff)])?;
        self.connection
            .execute("DELETE FROM entities WHERE updated<?1", [Integer(cutoff)])?;
        self.connection
            .execute("DELETE FROM catalog WHERE updated<?1", [Integer(cutoff)])?;
        loop {
            let pages = self
                .connection
                .pragma_query_value(None, "page_count", |r| number(r, 0))?;
            let free = self
                .connection
                .pragma_query_value(None, "freelist_count", |r| number(r, 0))?;
            let size = self
                .connection
                .pragma_query_value(None, "page_size", |r| number(r, 0))?;
            if pages
                .saturating_sub(free)
                .saturating_mul(size)
                .saturating_add(reserve)
                <= self.quota / 2
            {
                break;
            }
            if self.connection.execute(
                "DELETE FROM logs WHERE id=(SELECT id FROM logs WHERE id<>?1 ORDER BY updated,id LIMIT 1)",
                [protected.unwrap_or(-1)],
            )? == 0
                && self.connection.execute("DELETE FROM entities WHERE rowid=(SELECT rowid FROM entities ORDER BY updated LIMIT 1)",[])? == 0
                && self.connection.execute("DELETE FROM catalog WHERE rowid=(SELECT rowid FROM catalog ORDER BY updated LIMIT 1)",[])? == 0 { bail!("log cache quota reached"); }
        }
        if self.disk_size() > self.quota.saturating_sub(reserve) {
            self.connection
                .execute_batch("PRAGMA incremental_vacuum; PRAGMA wal_checkpoint(TRUNCATE);")?;
            ensure!(
                self.disk_size().saturating_add(reserve) <= self.quota,
                "log cache quota reached; another process may be holding a read transaction"
            );
        }
        Ok(())
    }

    fn disk_size(&self) -> u64 {
        let mut wal = self.path.as_os_str().to_owned();
        wal.push("-wal");
        fs::metadata(&self.path).map_or(0, |m| m.len())
            + fs::metadata(PathBuf::from(wal)).map_or(0, |m| m.len())
    }
}

/// One FTS cursor annotates only the selected rows. Rowid bounds avoid scanning
/// unrelated index history, while the join confines results to this log window.
fn highlight_lines(
    connection: &Connection,
    log_id: i64,
    ids: &[i64],
    lines: &mut [LogLine],
    query: &LogQuery,
) -> Result<()> {
    let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
        return Ok(());
    };
    let first_start = first.start.min(last.start);
    let last_start = first.start.max(last.start);
    let indices: std::collections::HashMap<_, _> = ids
        .iter()
        .copied()
        .enumerate()
        .map(|(index, id)| (id, index))
        .collect();
    let mut statement = connection.prepare_cached(
        "SELECT log_fts.rowid,highlight(log_fts,0,char(30),char(31))
         FROM log_fts CROSS JOIN lines n ON n.id=log_fts.rowid
         WHERE log_fts MATCH ?1 AND log_fts.rowid BETWEEN ?2 AND ?3
         AND n.log_id=?4 AND n.start BETWEEN ?5 AND ?6",
    )?;
    let mut rows = statement.query(params![
        query.literal,
        ids.iter().min(),
        ids.iter().max(),
        log_id,
        Integer(first_start),
        Integer(last_start)
    ])?;
    while let Some(row) = rows.next()? {
        let id = row.get::<_, i64>(0)?;
        if let Some(index) = indices.get(&id) {
            lines[*index].highlights = text::highlights(&row.get::<_, String>(1)?);
        }
    }
    Ok(())
}

fn writer_lock(path: &Path) -> Result<fs::File> {
    // FTS updates and quota maintenance can hold the lock for more than 500 ms
    // on slower disks. Waiting happens on the dedicated cache worker thread.
    writer_lock_for(path, Duration::from_secs(5))
}

fn writer_lock_for(path: &Path, timeout: Duration) -> Result<fs::File> {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600)
    };
    let file = options
        .open(PathBuf::from(name))
        .context("cannot open cache writer lock")?;
    let started = std::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(fs::TryLockError::WouldBlock) => bail!("log cache is busy; retry shortly"),
            Err(fs::TryLockError::Error(error)) => {
                return Err(error).context("cannot lock log cache");
            }
        }
    }
}

fn log_id(connection: &Connection, scope: &str, id: &LogIdentity) -> Result<Option<i64>> {
    Ok(connection.query_row("SELECT id FROM logs WHERE scope=?1 AND repository=?2 AND run_uuid=?3 AND run_attempt=?4 AND step_uuid=?5 AND attempt=?6",params![scope,id.repository,id.run_uuid,Integer(id.run_attempt),id.step_uuid,Integer(id.step_attempt)],|r|r.get(0)).optional()?)
}
fn ranges(connection: &Connection, id: i64) -> Result<Vec<(u64, u64)>> {
    Ok(connection
        .prepare("SELECT start,end FROM coverage WHERE log_id=?1 ORDER BY start")?
        .query_map([id], |r| Ok((number(r, 0)?, number(r, 1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}
fn clear_content(tx: &Transaction<'_>, id: i64) -> Result<()> {
    for table in ["blocks", "coverage", "lines"] {
        tx.execute(&format!("DELETE FROM {table} WHERE log_id=?1"), [id])?;
    }
    Ok(())
}
fn write_blocks(tx: &Transaction<'_>, id: i64, offset: u64, bytes: &[u8]) -> Result<()> {
    let mut position = offset;
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let block_start = position / BLOCK_SIZE as u64 * BLOCK_SIZE as u64;
        let within = usize::try_from(position - block_start)?;
        let length = remaining.len().min(BLOCK_SIZE - within);
        let mut block = tx
            .query_row(
                "SELECT data FROM blocks WHERE log_id=?1 AND start=?2",
                params![id, Integer(block_start)],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .unwrap_or_else(|| vec![0; BLOCK_SIZE]);
        block[within..within + length].copy_from_slice(&remaining[..length]);
        tx.execute("INSERT INTO blocks VALUES(?1,?2,?3) ON CONFLICT(log_id,start) DO UPDATE SET data=excluded.data",params![id,Integer(block_start),block])?;
        position += length as u64;
        remaining = &remaining[length..];
    }
    Ok(())
}
fn read_bytes(connection: &Connection, id: i64, start: u64, end: u64) -> Result<Vec<u8>> {
    let length = usize::try_from(end.saturating_sub(start))?;
    ensure!(
        length <= 4 * BLOCK_SIZE,
        "log index window exceeds its memory budget"
    );
    let mut bytes = Vec::with_capacity(length);
    let mut position = start;
    while position < end {
        let block_start = position / BLOCK_SIZE as u64 * BLOCK_SIZE as u64;
        let within = usize::try_from(position - block_start)?;
        let block: Vec<u8> = connection.query_row(
            "SELECT data FROM blocks WHERE log_id=?1 AND start=?2",
            params![id, Integer(block_start)],
            |r| r.get(0),
        )?;
        let length = usize::try_from(end - position)?.min(BLOCK_SIZE - within);
        bytes.extend_from_slice(&block[within..within + length]);
        position += length as u64;
    }
    Ok(bytes)
}

// SQLite integers are signed. All remote offsets cross this checked boundary.
#[derive(Debug, Clone, Copy)]
struct Integer(u64);
impl rusqlite::types::ToSql for Integer {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        let value = i64::try_from(self.0)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(rusqlite::types::ToSqlOutput::Owned(
            rusqlite::types::Value::Integer(value),
        ))
    }
}
impl rusqlite::types::FromSql for Integer {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let integer = value.as_i64()?;
        u64::try_from(integer)
            .map(Self)
            .map_err(|_negative| rusqlite::types::FromSqlError::OutOfRange(integer))
    }
}
fn number(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    row.get::<_, Integer>(index).map(|value| value.0)
}
fn optional_number(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<Integer>>(index)
        .map(|value| value.map(|value| value.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_lock_waits_for_a_slow_writer() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("logs.db");
        let held = writer_lock(&path)?;
        let (started, waiting) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            started.send(())?;
            writer_lock(&path)
        });
        waiting.recv()?;
        std::thread::sleep(Duration::from_millis(750));
        drop(held);
        let acquired = worker
            .join()
            .map_err(|_panic| anyhow::anyhow!("writer thread panicked"))??;
        drop(acquired);
        Ok(())
    }

    #[test]
    fn writer_lock_times_out_and_can_be_acquired_after_release() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("logs.db");
        let held = writer_lock(&path)?;
        let error = writer_lock_for(&path, Duration::from_millis(30))
            .err()
            .context("a second writer acquired the held lock")?;
        assert_eq!(error.to_string(), "log cache is busy; retry shortly");
        drop(held);
        let acquired = writer_lock_for(&path, Duration::ZERO)?;
        drop(acquired);
        Ok(())
    }
}
