use crate::{MAX_LIST_PAGE_SIZE, MediaMetadata, MetadataFilter, MetadataPage, StoreError};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Durable query index; JSON sidecars remain the authoritative metadata records.
pub(crate) struct MetadataIndex {
    database_path: PathBuf,
    metadata_dir: PathBuf,
}

impl MetadataIndex {
    pub(crate) fn new(database_path: PathBuf, metadata_dir: PathBuf) -> Self {
        Self {
            database_path,
            metadata_dir,
        }
    }

    pub(crate) fn begin_mutation(&self) -> Result<(), StoreError> {
        self.open()?.execute(
            "INSERT OR REPLACE INTO media_index_state(singleton, initialized) VALUES (1, 0)",
            [],
        )?;
        Ok(())
    }

    pub(crate) fn finish_mutation(&self) -> Result<(), StoreError> {
        self.open()?.execute(
            "INSERT OR REPLACE INTO media_index_state(singleton, initialized) VALUES (1, 1)",
            [],
        )?;
        Ok(())
    }

    fn open(&self) -> Result<Connection, StoreError> {
        open_private_database(&self.database_path)?;
        let connection = Connection::open(&self.database_path)?;
        restrict_file(&self.database_path)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS media_metadata (
                id TEXT PRIMARY KEY NOT NULL,
                sha256 TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER,
                prompt TEXT,
                prompt_lower TEXT,
                model TEXT,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS media_metadata_hash_id
                ON media_metadata(sha256, id);
            CREATE INDEX IF NOT EXISTS media_metadata_created_id
                ON media_metadata(created_at DESC, id ASC);
            CREATE INDEX IF NOT EXISTS media_metadata_expiry
                ON media_metadata(expires_at);
            CREATE TABLE IF NOT EXISTS media_index_state (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                initialized INTEGER NOT NULL
            );",
        )?;
        Ok(connection)
    }

    /// Open the index, rebuilding it from sidecars once if it has not been
    /// initialized. A corrupt SQLite file is discarded and rebuilt.
    pub(crate) fn ensure_ready(&self) -> Result<(), StoreError> {
        let connection = match self.open() {
            Ok(connection) => connection,
            Err(StoreError::Database(_)) => {
                self.remove_database_files()?;
                self.open()?
            }
            Err(error) => return Err(error),
        };

        let initialized: Option<i64> = connection
            .query_row(
                "SELECT initialized FROM media_index_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if initialized == Some(1) {
            return Ok(());
        }

        let transaction = connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM media_metadata", [])?;
        let entries = match fs::read_dir(&self.metadata_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                transaction.execute(
                    "INSERT OR REPLACE INTO media_index_state(singleton, initialized) VALUES (1, 1)",
                    [],
                )?;
                transaction.commit()?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file()
                || entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    != Some("json")
            {
                continue;
            }
            let record: MediaMetadata = serde_json::from_slice(&fs::read(entry.path())?)?;
            insert_record(&transaction, &record)?;
        }

        transaction.execute(
            "INSERT OR REPLACE INTO media_index_state(singleton, initialized) VALUES (1, 1)",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn remove_database_files(&self) -> Result<(), StoreError> {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.database_path.as_os_str().to_os_string();
            path.push(suffix);
            match fs::remove_file(PathBuf::from(path)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(crate) fn upsert(&self, metadata: &MediaMetadata) -> Result<(), StoreError> {
        insert_record(&self.open()?, metadata)
    }

    pub(crate) fn remove(&self, id: &str) -> Result<(), StoreError> {
        self.open()?
            .execute("DELETE FROM media_metadata WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub(crate) fn reference_count(&self, sha256: &str) -> Result<u64, StoreError> {
        let count: i64 = self.open()?.query_row(
            "SELECT COUNT(*) FROM media_metadata WHERE sha256 = ?1",
            params![sha256],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    pub(crate) fn find_by_hash(&self, sha256: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .open()?
            .query_row(
                "SELECT id FROM media_metadata WHERE sha256 = ?1 ORDER BY id ASC LIMIT 1",
                params![sha256],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(crate) fn list(
        &self,
        filter: &MetadataFilter,
        limit: usize,
        offset: usize,
    ) -> Result<MetadataPage, StoreError> {
        let connection = self.open()?;
        let prompt = filter
            .prompt_contains
            .as_ref()
            .map(|value| value.to_lowercase());
        let model = filter.model.as_deref();
        let from = filter.created_at_from;
        let to = filter.created_at_to;
        let total: i64 = connection.query_row(
            "SELECT COUNT(*) FROM media_metadata
             WHERE (?1 IS NULL OR instr(COALESCE(prompt_lower, ''), ?1) > 0)
               AND (?2 IS NULL OR model = ?2)
               AND (?3 IS NULL OR created_at >= ?3)
               AND (?4 IS NULL OR created_at <= ?4)",
            params![prompt, model, from, to],
            |row| row.get(0),
        )?;
        let bounded_limit = limit.min(MAX_LIST_PAGE_SIZE);
        if bounded_limit == 0 {
            return Ok(MetadataPage {
                items: Vec::new(),
                total: total as usize,
            });
        }
        let mut statement = connection.prepare(
            "SELECT record_json FROM media_metadata
             WHERE (?1 IS NULL OR instr(COALESCE(prompt_lower, ''), ?1) > 0)
               AND (?2 IS NULL OR model = ?2)
               AND (?3 IS NULL OR created_at >= ?3)
               AND (?4 IS NULL OR created_at <= ?4)
             ORDER BY created_at DESC, id ASC LIMIT ?5 OFFSET ?6",
        )?;
        let rows = statement.query_map(
            params![prompt, model, from, to, bounded_limit as i64, offset as i64],
            |row| row.get::<_, String>(0),
        )?;
        let mut items = Vec::with_capacity(bounded_limit.min(total as usize));
        for row in rows {
            items.push(serde_json::from_str(&row?)?);
        }
        Ok(MetadataPage {
            items,
            total: total as usize,
        })
    }

    pub(crate) fn expired(&self, now: i64) -> Result<Vec<MediaMetadata>, StoreError> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT record_json FROM media_metadata WHERE expires_at < ?1 ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![now], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        Ok(records)
    }
}

fn insert_record(connection: &Connection, metadata: &MediaMetadata) -> Result<(), StoreError> {
    let record_json = serde_json::to_string(metadata)?;
    let prompt_lower = metadata.prompt.as_deref().map(str::to_lowercase);
    connection.execute(
        "INSERT INTO media_metadata(id, sha256, created_at, expires_at, prompt, prompt_lower, model, record_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            sha256 = excluded.sha256,
            created_at = excluded.created_at,
            expires_at = excluded.expires_at,
            prompt = excluded.prompt,
            prompt_lower = excluded.prompt_lower,
            model = excluded.model,
            record_json = excluded.record_json",
        params![
            metadata.id,
            metadata.sha256,
            metadata.created_at,
            metadata.expires_at,
            metadata.prompt,
            prompt_lower,
            metadata.model,
            record_json,
        ],
    )?;
    Ok(())
}

#[cfg(unix)]
fn open_private_database(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    Ok(())
}

#[cfg(not(unix))]
fn open_private_database(path: &Path) -> Result<(), StoreError> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    Ok(())
}
#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}
