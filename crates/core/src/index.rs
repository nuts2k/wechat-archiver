use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::error::{ArchiverError, Result};

const CURRENT_SCHEMA_VERSION: i64 = 7;

type MigrationFn = fn(&Connection) -> Result<()>;

struct Migration {
    version: i64,
    name: &'static str,
    apply: MigrationFn,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "create_media_items",
        apply: migration_1_create_media_items,
    },
    Migration {
        version: 2,
        name: "add_decoder_column",
        apply: migration_2_add_decoder_column,
    },
    Migration {
        version: 3,
        name: "add_message_metadata_columns",
        apply: migration_3_add_message_metadata_columns,
    },
    Migration {
        version: 4,
        name: "add_file_metadata_columns",
        apply: migration_4_add_file_metadata_columns,
    },
    Migration {
        version: 5,
        name: "add_media_metadata_columns",
        apply: migration_5_add_media_metadata_columns,
    },
    Migration {
        version: 6,
        name: "add_source_fingerprint_columns",
        apply: migration_6_add_source_fingerprint_columns,
    },
    Migration {
        version: 7,
        name: "add_decode_fingerprint_column",
        apply: migration_7_add_decode_fingerprint_column,
    },
];

#[derive(Debug, Clone)]
pub(crate) struct MediaRecord {
    pub source_path: String,
    pub source_relative_path: String,
    pub source_kind: String,
    pub media_type: String,
    pub original_filename: Option<String>,
    pub mime_type: Option<String>,
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
    pub duration_ms: Option<u64>,
    pub message_talker: Option<String>,
    pub message_sender: Option<String>,
    pub message_local_id: Option<i64>,
    pub message_create_time: Option<i64>,
    pub decoder: Option<String>,
    pub decode_fingerprint: Option<String>,
    pub archive_path: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub source_size_bytes: Option<u64>,
    pub source_modified_ms: Option<i64>,
    pub extension: Option<String>,
    pub decrypt_status: String,
    pub verify_status: String,
    pub error: Option<String>,
    pub timestamp_epoch_ms: u128,
}

#[derive(Debug, Clone)]
pub(crate) struct ReusableMediaRecord {
    pub archive_path: String,
    pub sha256: String,
    pub size_bytes: Option<u64>,
    pub extension: Option<String>,
    pub decoder: Option<String>,
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
    pub duration_ms: Option<u64>,
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
    debug_assert_eq!(
        MIGRATIONS.last().map(|migration| migration.version),
        Some(CURRENT_SCHEMA_VERSION)
    );
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        "#,
    )?;
    ensure_schema_migrations_table(conn)?;
    for migration in MIGRATIONS {
        apply_migration(conn, migration)?;
    }
    Ok(())
}

fn ensure_schema_migrations_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        );
        "#,
    )?;
    Ok(())
}

fn apply_migration(conn: &Connection, migration: &Migration) -> Result<()> {
    if migration_applied(conn, migration.version)? {
        return Ok(());
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        if migration_applied(conn, migration.version)? {
            return Ok(());
        }
        (migration.apply)(conn)?;
        conn.execute(
            r#"
            INSERT INTO schema_migrations (version, name, applied_at_ms)
            VALUES (?1, ?2, CAST(strftime('%s', 'now') AS INTEGER) * 1000)
            "#,
            params![migration.version, migration.name],
        )?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

fn migration_applied(conn: &Connection, version: i64) -> Result<bool> {
    let applied = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
        params![version],
        |row| row.get::<_, i64>(0),
    )? != 0;
    Ok(applied)
}

fn migration_1_create_media_items(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS media_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_path TEXT NOT NULL,
            source_relative_path TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            media_type TEXT NOT NULL,
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
    Ok(())
}

fn migration_2_add_decoder_column(conn: &Connection) -> Result<()> {
    let columns = media_item_columns(conn)?;
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

fn migration_3_add_message_metadata_columns(conn: &Connection) -> Result<()> {
    ensure_column(conn, "message_talker", "TEXT")?;
    ensure_column(conn, "message_sender", "TEXT")?;
    ensure_column(conn, "message_local_id", "INTEGER")?;
    ensure_column(conn, "message_create_time", "INTEGER")?;
    conn.execute(
        r#"
        CREATE INDEX IF NOT EXISTS idx_media_items_message
            ON media_items(message_talker, message_create_time, message_local_id)
        "#,
        [],
    )?;
    Ok(())
}

fn migration_4_add_file_metadata_columns(conn: &Connection) -> Result<()> {
    ensure_column(conn, "original_filename", "TEXT")?;
    ensure_column(conn, "mime_type", "TEXT")?;
    Ok(())
}

fn migration_5_add_media_metadata_columns(conn: &Connection) -> Result<()> {
    ensure_column(conn, "width_px", "INTEGER")?;
    ensure_column(conn, "height_px", "INTEGER")?;
    ensure_column(conn, "duration_ms", "INTEGER")?;
    Ok(())
}

fn migration_6_add_source_fingerprint_columns(conn: &Connection) -> Result<()> {
    ensure_column(conn, "source_size_bytes", "INTEGER")?;
    ensure_column(conn, "source_modified_ms", "INTEGER")?;
    conn.execute(
        r#"
        CREATE INDEX IF NOT EXISTS idx_media_items_source_fingerprint
            ON media_items(source_path, source_size_bytes, source_modified_ms)
        "#,
        [],
    )?;
    Ok(())
}

fn migration_7_add_decode_fingerprint_column(conn: &Connection) -> Result<()> {
    ensure_column(conn, "decode_fingerprint", "TEXT")?;
    conn.execute(
        r#"
        CREATE INDEX IF NOT EXISTS idx_media_items_decode_fingerprint
            ON media_items(source_path, source_size_bytes, source_modified_ms, decode_fingerprint)
        "#,
        [],
    )?;
    Ok(())
}

fn media_item_columns(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(media_items)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(columns)
}

fn ensure_column(conn: &Connection, column_name: &str, column_type: &str) -> Result<()> {
    let columns = media_item_columns(conn)?;
    if !columns.iter().any(|column| column == column_name) {
        conn.execute(
            &format!("ALTER TABLE media_items ADD COLUMN {column_name} {column_type}"),
            [],
        )?;
    }
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
            original_filename,
            mime_type,
            width_px,
            height_px,
            duration_ms,
            message_talker,
            message_sender,
            message_local_id,
            message_create_time,
            decoder,
            decode_fingerprint,
            archive_path,
            sha256,
            size_bytes,
            source_size_bytes,
            source_modified_ms,
            extension,
            decrypt_status,
            verify_status,
            error,
            created_at_ms,
            updated_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?25)
        ON CONFLICT(source_path)
        DO UPDATE SET
            archive_path = excluded.archive_path,
            sha256 = excluded.sha256,
            size_bytes = excluded.size_bytes,
            source_size_bytes = excluded.source_size_bytes,
            source_modified_ms = excluded.source_modified_ms,
            source_relative_path = excluded.source_relative_path,
            source_kind = excluded.source_kind,
            media_type = excluded.media_type,
            original_filename = excluded.original_filename,
            mime_type = excluded.mime_type,
            width_px = excluded.width_px,
            height_px = excluded.height_px,
            duration_ms = excluded.duration_ms,
            message_talker = excluded.message_talker,
            message_sender = excluded.message_sender,
            message_local_id = excluded.message_local_id,
            message_create_time = excluded.message_create_time,
            decoder = excluded.decoder,
            decode_fingerprint = excluded.decode_fingerprint,
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
            record.original_filename,
            record.mime_type,
            record.width_px,
            record.height_px,
            record.duration_ms,
            record.message_talker,
            record.message_sender,
            record.message_local_id,
            record.message_create_time,
            record.decoder,
            record.decode_fingerprint,
            record.archive_path,
            record.sha256,
            record.size_bytes,
            record.source_size_bytes,
            record.source_modified_ms,
            record.extension,
            record.decrypt_status,
            record.verify_status,
            record.error,
            timestamp_epoch_ms,
        ],
    )?;
    Ok(())
}

pub(crate) fn reusable_media_record(
    conn: &Connection,
    source_path: &str,
    source_kind: &str,
    media_type: &str,
    extension: &str,
    source_size_bytes: u64,
    source_modified_ms: i64,
) -> Result<Option<ReusableMediaRecord>> {
    let record = conn
        .query_row(
            r#"
            SELECT
                archive_path,
                sha256,
                size_bytes,
                extension,
                decoder,
                width_px,
                height_px,
                duration_ms
            FROM media_items
            WHERE source_path = ?1
              AND source_kind = ?2
              AND media_type = ?3
              AND extension = ?4
              AND source_size_bytes = ?5
              AND source_modified_ms = ?6
              AND verify_status = 'ok'
              AND archive_path IS NOT NULL
              AND sha256 IS NOT NULL
            LIMIT 1
            "#,
            params![
                source_path,
                source_kind,
                media_type,
                extension,
                source_size_bytes,
                source_modified_ms,
            ],
            |row| {
                let size_bytes = row
                    .get::<_, Option<i64>>(2)?
                    .map(|value| value.max(0) as u64);
                Ok(ReusableMediaRecord {
                    archive_path: row.get(0)?,
                    sha256: row.get(1)?,
                    size_bytes,
                    extension: row.get(3)?,
                    decoder: row.get(4)?,
                    width_px: optional_u32(row, 5)?,
                    height_px: optional_u32(row, 6)?,
                    duration_ms: optional_u64(row, 7)?,
                })
            },
        )
        .optional()?;
    Ok(record)
}

pub(crate) fn reusable_decoded_media_record(
    conn: &Connection,
    source_path: &str,
    source_kind: &str,
    media_type: &str,
    source_size_bytes: u64,
    source_modified_ms: i64,
    decode_fingerprint: &str,
) -> Result<Option<ReusableMediaRecord>> {
    let record = conn
        .query_row(
            r#"
            SELECT
                archive_path,
                sha256,
                size_bytes,
                extension,
                decoder,
                width_px,
                height_px,
                duration_ms
            FROM media_items
            WHERE source_path = ?1
              AND source_kind = ?2
              AND media_type = ?3
              AND source_size_bytes = ?4
              AND source_modified_ms = ?5
              AND decode_fingerprint = ?6
              AND verify_status = 'ok'
              AND archive_path IS NOT NULL
              AND sha256 IS NOT NULL
            LIMIT 1
            "#,
            params![
                source_path,
                source_kind,
                media_type,
                source_size_bytes,
                source_modified_ms,
                decode_fingerprint,
            ],
            |row| {
                let size_bytes = row
                    .get::<_, Option<i64>>(2)?
                    .map(|value| value.max(0) as u64);
                Ok(ReusableMediaRecord {
                    archive_path: row.get(0)?,
                    sha256: row.get(1)?,
                    size_bytes,
                    extension: row.get(3)?,
                    decoder: row.get(4)?,
                    width_px: optional_u32(row, 5)?,
                    height_px: optional_u32(row, 6)?,
                    duration_ms: optional_u64(row, 7)?,
                })
            },
        )
        .optional()?;
    Ok(record)
}

fn optional_u32(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u32>> {
    Ok(row.get::<_, Option<i64>>(index)?.and_then(|value| {
        if (0..=u32::MAX as i64).contains(&value) {
            Some(value as u32)
        } else {
            None
        }
    }))
}

fn optional_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    Ok(row
        .get::<_, Option<i64>>(index)?
        .and_then(|value| (value >= 0).then_some(value as u64)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migration_versions(conn: &Connection) -> Vec<i64> {
        conn.prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    fn migration_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn assert_current_migrations(conn: &Connection) {
        assert_eq!(
            migration_versions(conn),
            (1..=CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(
            migration_count(conn),
            CURRENT_SCHEMA_VERSION,
            "每个 schema 版本应只登记一次"
        );
    }

    fn assert_media_item_columns(conn: &Connection, expected_columns: &[&str]) {
        let columns = media_item_columns(conn).unwrap();
        for column in expected_columns {
            assert!(
                columns.iter().any(|existing| existing == column),
                "缺少 media_items.{column}"
            );
        }
    }

    #[test]
    fn open_index_initializes_schema_migrations_for_new_index() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();

        let conn = open_index(archive).unwrap();

        assert_current_migrations(&conn);
        assert_media_item_columns(
            &conn,
            &[
                "source_path",
                "source_relative_path",
                "source_kind",
                "media_type",
                "original_filename",
                "mime_type",
                "width_px",
                "height_px",
                "duration_ms",
                "source_size_bytes",
                "source_modified_ms",
                "decode_fingerprint",
                "decoder",
                "message_talker",
                "message_sender",
                "message_local_id",
                "message_create_time",
            ],
        );
        let latest_migration_name: String = conn
            .query_row(
                "SELECT name FROM schema_migrations WHERE version = ?1",
                params![CURRENT_SCHEMA_VERSION],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(latest_migration_name, "add_decode_fingerprint_column");
    }

    #[test]
    fn open_index_migrates_message_metadata_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let index = index_path(archive);
        let conn = Connection::open(&index).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE media_items (
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

            INSERT INTO media_items (
                source_path,
                source_relative_path,
                source_kind,
                media_type,
                decoder,
                decrypt_status,
                verify_status,
                created_at_ms,
                updated_at_ms
            )
            VALUES (
                '/tmp/source/image.dat',
                'image.dat',
                'message_db_image',
                'image',
                'legacy_xor',
                'decoded',
                'ok',
                1,
                1
            );
            "#,
        )
        .unwrap();
        drop(conn);

        let migrated = open_index(archive).unwrap();
        assert_current_migrations(&migrated);
        assert_media_item_columns(
            &migrated,
            &[
                "message_talker",
                "message_sender",
                "message_local_id",
                "message_create_time",
                "original_filename",
                "mime_type",
                "width_px",
                "height_px",
                "duration_ms",
                "source_size_bytes",
                "source_modified_ms",
            ],
        );

        let source_path: String = migrated
            .query_row("SELECT source_path FROM media_items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(source_path, "/tmp/source/image.dat");
    }

    #[test]
    fn open_index_migrates_legacy_decoder_from_source_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let index = index_path(archive);
        let conn = Connection::open(&index).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE media_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_path TEXT NOT NULL,
                source_relative_path TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                media_type TEXT NOT NULL,
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

            INSERT INTO media_items (
                source_path,
                source_relative_path,
                source_kind,
                media_type,
                decrypt_status,
                verify_status,
                created_at_ms,
                updated_at_ms
            )
            VALUES (
                '/tmp/source/legacy.dat',
                'legacy.dat',
                'legacy_xor',
                'image',
                'decoded',
                'ok',
                1,
                1
            );
            "#,
        )
        .unwrap();
        drop(conn);

        let migrated = open_index(archive).unwrap();

        assert_current_migrations(&migrated);
        assert_media_item_columns(&migrated, &["decoder"]);
        let (source_kind, decoder): (String, Option<String>) = migrated
            .query_row(
                "SELECT source_kind, decoder FROM media_items WHERE source_path = '/tmp/source/legacy.dat'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(source_kind, "dat_image");
        assert_eq!(decoder.as_deref(), Some("legacy_xor"));
    }

    #[test]
    fn open_index_schema_migrations_are_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();

        let first = open_index(archive).unwrap();
        let first_count = migration_count(&first);
        drop(first);

        let second = open_index(archive).unwrap();

        assert_current_migrations(&second);
        assert_eq!(migration_count(&second), first_count);
    }
}
