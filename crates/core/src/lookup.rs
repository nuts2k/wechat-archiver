use std::path::{Path, PathBuf};

use rusqlite::{Connection, Row, ToSql};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::index::{index_path, open_index_readonly};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexLookup {
    pub archive_dir: PathBuf,
    pub index_path: PathBuf,
    pub query: IndexLookupQuery,
    pub matched_records: u64,
    pub records: Vec<IndexLookupRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum IndexLookupQuery {
    Sha256(String),
    SourcePath(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexLookupRecord {
    pub id: i64,
    pub source_path: String,
    pub source_relative_path: String,
    pub source_kind: String,
    pub media_type: String,
    pub original_filename: Option<String>,
    pub mime_type: Option<String>,
    pub message_talker: Option<String>,
    pub message_sender: Option<String>,
    pub message_local_id: Option<i64>,
    pub message_create_time: Option<i64>,
    pub decoder: Option<String>,
    pub archive_path: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub extension: Option<String>,
    pub decrypt_status: String,
    pub verify_status: String,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub fn lookup_index(archive_dir: &Path, query: IndexLookupQuery) -> Result<IndexLookup> {
    let conn = open_index_readonly(archive_dir)?;
    let records = match &query {
        IndexLookupQuery::Sha256(sha256) => {
            query_records(&conn, "sha256 = ?1", "source_path, id", sha256.as_str())?
        }
        IndexLookupQuery::SourcePath(source_path) => {
            query_records_by_source_path(&conn, source_path)?
        }
    };

    Ok(IndexLookup {
        archive_dir: archive_dir.to_path_buf(),
        index_path: index_path(archive_dir),
        query,
        matched_records: records.len() as u64,
        records,
    })
}

fn query_records_by_source_path(
    conn: &Connection,
    source_path: &str,
) -> Result<Vec<IndexLookupRecord>> {
    let variants = source_path_variants(source_path);
    if variants.len() == 1 {
        return query_records(
            conn,
            "source_path = ?1",
            "updated_at_ms DESC, id DESC",
            variants[0].as_str(),
        );
    }

    query_records_with_two_values(
        conn,
        "(source_path = ?1 OR source_path = ?2)",
        "updated_at_ms DESC, id DESC",
        variants[0].as_str(),
        variants[1].as_str(),
    )
}

fn source_path_variants(source_path: &str) -> Vec<String> {
    let mut variants = vec![source_path.to_string()];
    if let Ok(canonical) = Path::new(source_path).canonicalize() {
        let canonical = canonical.to_string_lossy().to_string();
        if !variants.iter().any(|variant| variant == &canonical) {
            variants.push(canonical);
        }
    }
    variants
}

fn query_records(
    conn: &Connection,
    predicate: &str,
    order_by: &str,
    value: &str,
) -> Result<Vec<IndexLookupRecord>> {
    query_records_with_params(conn, predicate, order_by, &[&value])
}

fn query_records_with_two_values(
    conn: &Connection,
    predicate: &str,
    order_by: &str,
    first_value: &str,
    second_value: &str,
) -> Result<Vec<IndexLookupRecord>> {
    query_records_with_params(conn, predicate, order_by, &[&first_value, &second_value])
}

pub(crate) fn query_all_records(conn: &Connection) -> Result<Vec<IndexLookupRecord>> {
    let values: [&dyn ToSql; 0] = [];
    query_records_with_params(conn, "1 = 1", "id", &values)
}

fn query_records_with_params(
    conn: &Connection,
    predicate: &str,
    order_by: &str,
    values: &[&dyn ToSql],
) -> Result<Vec<IndexLookupRecord>> {
    let columns = media_item_columns(conn)?;
    let sql = format!(
        r#"
        SELECT
            id,
            source_path,
            source_relative_path,
            source_kind,
            media_type,
            {original_filename},
            {mime_type},
            {message_talker},
            {message_sender},
            {message_local_id},
            {message_create_time},
            {decoder},
            archive_path,
            sha256,
            size_bytes,
            extension,
            decrypt_status,
            verify_status,
            error,
            created_at_ms,
            updated_at_ms
        FROM media_items
        WHERE {predicate}
        ORDER BY {order_by}
        "#,
        original_filename = column_or_null(&columns, "original_filename", "TEXT"),
        mime_type = column_or_null(&columns, "mime_type", "TEXT"),
        message_talker = column_or_null(&columns, "message_talker", "TEXT"),
        message_sender = column_or_null(&columns, "message_sender", "TEXT"),
        message_local_id = column_or_null(&columns, "message_local_id", "INTEGER"),
        message_create_time = column_or_null(&columns, "message_create_time", "INTEGER"),
        decoder = column_or_null(&columns, "decoder", "TEXT"),
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(values, row_to_record)?;
    let records = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(records)
}

fn media_item_columns(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(media_items)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(columns)
}

fn column_or_null(columns: &[String], column_name: &str, column_type: &str) -> String {
    if columns.iter().any(|column| column == column_name) {
        column_name.to_string()
    } else {
        format!("CAST(NULL AS {column_type}) AS {column_name}")
    }
}

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<IndexLookupRecord> {
    let size_bytes = row
        .get::<_, Option<i64>>(14)?
        .map(|value| value.max(0) as u64);
    Ok(IndexLookupRecord {
        id: row.get(0)?,
        source_path: row.get(1)?,
        source_relative_path: row.get(2)?,
        source_kind: row.get(3)?,
        media_type: row.get(4)?,
        original_filename: row.get(5)?,
        mime_type: row.get(6)?,
        message_talker: row.get(7)?,
        message_sender: row.get(8)?,
        message_local_id: row.get(9)?,
        message_create_time: row.get(10)?,
        decoder: row.get(11)?,
        archive_path: row.get(12)?,
        sha256: row.get(13)?,
        size_bytes,
        extension: row.get(15)?,
        decrypt_status: row.get(16)?,
        verify_status: row.get(17)?,
        error: row.get(18)?,
        created_at_ms: row.get(19)?,
        updated_at_ms: row.get(20)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{insert_record, open_index, MediaRecord};

    fn record(source_path: &str, sha256: &str) -> MediaRecord {
        MediaRecord {
            source_path: source_path.to_string(),
            source_relative_path: Path::new(source_path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            source_kind: "direct_image".to_string(),
            media_type: "image".to_string(),
            original_filename: Path::new(source_path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string()),
            mime_type: Some("image/jpeg".to_string()),
            message_talker: Some("chat_a".to_string()),
            message_sender: None,
            message_local_id: Some(42),
            message_create_time: Some(1_700_000_000),
            decoder: None,
            archive_path: Some(format!("objects/sha256/ab/{sha256}.jpg")),
            sha256: Some(sha256.to_string()),
            size_bytes: Some(123),
            extension: Some("jpg".to_string()),
            decrypt_status: "decoded".to_string(),
            verify_status: "ok".to_string(),
            error: None,
            timestamp_epoch_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn lookup_index_finds_records_by_sha256() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let conn = open_index(archive).unwrap();
        insert_record(&conn, &record("/tmp/source/a.jpg", "abc123")).unwrap();
        insert_record(&conn, &record("/tmp/source/b.jpg", "abc123")).unwrap();
        insert_record(&conn, &record("/tmp/source/c.jpg", "def456")).unwrap();
        drop(conn);

        let lookup = lookup_index(archive, IndexLookupQuery::Sha256("abc123".to_string())).unwrap();

        assert_eq!(lookup.matched_records, 2);
        assert_eq!(lookup.records[0].source_path, "/tmp/source/a.jpg");
        assert_eq!(lookup.records[1].source_path, "/tmp/source/b.jpg");
        assert_eq!(
            lookup.records[0].original_filename.as_deref(),
            Some("a.jpg")
        );
        assert_eq!(lookup.records[0].mime_type.as_deref(), Some("image/jpeg"));
        assert_eq!(lookup.records[0].message_talker.as_deref(), Some("chat_a"));
        assert_eq!(lookup.records[0].size_bytes, Some(123));
    }

    #[test]
    fn lookup_index_finds_record_by_source_path() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let conn = open_index(archive).unwrap();
        insert_record(&conn, &record("/tmp/source/a.jpg", "abc123")).unwrap();
        insert_record(&conn, &record("/tmp/source/b.jpg", "def456")).unwrap();
        drop(conn);

        let lookup = lookup_index(
            archive,
            IndexLookupQuery::SourcePath("/tmp/source/b.jpg".to_string()),
        )
        .unwrap();

        assert_eq!(lookup.matched_records, 1);
        assert_eq!(lookup.records[0].source_path, "/tmp/source/b.jpg");
        assert_eq!(lookup.records[0].sha256.as_deref(), Some("def456"));
    }

    #[cfg(unix)]
    #[test]
    fn lookup_index_matches_canonical_source_path_variant() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("archive");
        let real_source = tmp.path().join("real-source");
        let linked_source = tmp.path().join("linked-source");
        std::fs::create_dir(&archive).unwrap();
        std::fs::create_dir(&real_source).unwrap();
        std::os::unix::fs::symlink(&real_source, &linked_source).unwrap();
        let linked_file = linked_source.join("image.jpg");
        std::fs::write(&linked_file, b"image").unwrap();
        let canonical_file = linked_file.canonicalize().unwrap();

        let conn = open_index(&archive).unwrap();
        insert_record(&conn, &record(&canonical_file.to_string_lossy(), "abc123")).unwrap();
        drop(conn);

        let lookup = lookup_index(
            &archive,
            IndexLookupQuery::SourcePath(linked_file.to_string_lossy().to_string()),
        )
        .unwrap();

        assert_eq!(lookup.matched_records, 1);
        assert_eq!(
            lookup.records[0].source_path,
            canonical_file.to_string_lossy()
        );
    }

    #[test]
    fn lookup_index_reads_legacy_schema_without_writing_migrations() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let conn = Connection::open(index_path(archive)).unwrap();
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
                sha256,
                size_bytes,
                extension,
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
                'abc123',
                99,
                'jpg',
                'decoded',
                'ok',
                1,
                1
            );
            "#,
        )
        .unwrap();
        drop(conn);

        let lookup = lookup_index(
            archive,
            IndexLookupQuery::SourcePath("/tmp/source/legacy.dat".to_string()),
        )
        .unwrap();

        assert_eq!(lookup.matched_records, 1);
        assert_eq!(lookup.records[0].decoder, None);
        assert_eq!(lookup.records[0].message_talker, None);

        let conn = Connection::open(index_path(archive)).unwrap();
        let migrations_table_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrations_table_exists, 0);
    }
}
