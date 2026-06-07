use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};

use crate::error::{ArchiverError, Result};

#[derive(Debug, Clone)]
pub(crate) struct MediaRecord {
    pub source_path: String,
    pub source_relative_path: String,
    pub source_kind: String,
    pub media_type: String,
    pub decoder: Option<String>,
    pub archive_path: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub extension: Option<String>,
    pub decrypt_status: String,
    pub verify_status: String,
    pub error: Option<String>,
    pub timestamp_epoch_ms: u128,
}

pub(crate) fn index_path(archive_dir: &Path) -> PathBuf {
    archive_dir.join("index.sqlite")
}

pub(crate) fn open_index(archive_dir: &Path) -> Result<Connection> {
    let path = index_path(archive_dir);
    let conn = Connection::open(path)?;
    initialize(&conn)?;
    Ok(conn)
}

pub(crate) fn open_index_readonly(archive_dir: &Path) -> Result<Connection> {
    let path = index_path(archive_dir);
    if !path.exists() {
        return Err(ArchiverError::MissingIndex(path));
    }
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

fn initialize(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS media_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_path TEXT NOT NULL,
            source_relative_path TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            media_type TEXT NOT NULL,
            decoder TEXT,
            archive_path TEXT,
            sha256 TEXT,
            size_bytes INTEGER,
            extension TEXT,
            decrypt_status TEXT NOT NULL,
            verify_status TEXT NOT NULL,
            error TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_media_items_sha256
            ON media_items(sha256);

        DELETE FROM media_items
        WHERE id NOT IN (
            SELECT MAX(id)
            FROM media_items
            GROUP BY source_path
        );

        DROP INDEX IF EXISTS idx_media_items_source_path;
        CREATE UNIQUE INDEX idx_media_items_source_path
            ON media_items(source_path);
        "#,
    )?;
    ensure_decoder_column(conn)?;
    Ok(())
}

fn ensure_decoder_column(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(media_items)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !columns.iter().any(|column| column == "decoder") {
        conn.execute("ALTER TABLE media_items ADD COLUMN decoder TEXT", [])?;
    }
    conn.execute(
        r#"
        UPDATE media_items
        SET decoder = source_kind,
            source_kind = 'dat_image'
        WHERE decoder IS NULL
          AND source_kind IN (
            'legacy_xor',
            'v1_aes',
            'v2_aes',
            'wxgf_jpg',
            'wxgf_raw',
            'wxgf_mp4'
          )
        "#,
        [],
    )?;
    Ok(())
}

pub(crate) fn insert_record(conn: &Connection, record: &MediaRecord) -> Result<()> {
    let timestamp_epoch_ms = record.timestamp_epoch_ms.min(i64::MAX as u128) as i64;
    conn.execute(
        r#"
        INSERT INTO media_items (
            source_path,
            source_relative_path,
            source_kind,
            media_type,
            decoder,
            archive_path,
            sha256,
            size_bytes,
            extension,
            decrypt_status,
            verify_status,
            error,
            created_at_ms,
            updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
        ON CONFLICT(source_path)
        DO UPDATE SET
            archive_path = excluded.archive_path,
            sha256 = excluded.sha256,
            size_bytes = excluded.size_bytes,
            source_relative_path = excluded.source_relative_path,
            source_kind = excluded.source_kind,
            media_type = excluded.media_type,
            decoder = excluded.decoder,
            extension = excluded.extension,
            decrypt_status = excluded.decrypt_status,
            verify_status = excluded.verify_status,
            error = excluded.error,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![
            record.source_path,
            record.source_relative_path,
            record.source_kind,
            record.media_type,
            record.decoder,
            record.archive_path,
            record.sha256,
            record.size_bytes,
            record.extension,
            record.decrypt_status,
            record.verify_status,
            record.error,
            timestamp_epoch_ms,
        ],
    )?;
    Ok(())
}
