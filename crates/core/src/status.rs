use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ArchiverError, Result};
use crate::index::{index_path, open_index_readonly};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveStatus {
    pub archive_dir: PathBuf,
    pub index_path: PathBuf,
    pub total_records: u64,
    pub archived_records: u64,
    pub unsupported_records: u64,
    pub failed_records: u64,
    pub unique_objects: u64,
    pub unique_bytes: u64,
}

pub fn archive_status(archive_dir: &Path) -> Result<ArchiveStatus> {
    let conn = match open_index_readonly(archive_dir) {
        Ok(conn) => conn,
        Err(ArchiverError::MissingIndex(_)) => {
            return Ok(ArchiveStatus {
                archive_dir: archive_dir.to_path_buf(),
                index_path: index_path(archive_dir),
                total_records: 0,
                archived_records: 0,
                unsupported_records: 0,
                failed_records: 0,
                unique_objects: 0,
                unique_bytes: 0,
            });
        }
        Err(err) => return Err(err),
    };

    let total_records = count(&conn, "SELECT COUNT(*) FROM media_items")?;
    let archived_records = count(
        &conn,
        "SELECT COUNT(*) FROM media_items WHERE verify_status = 'ok'",
    )?;
    let unsupported_records = count(
        &conn,
        "SELECT COUNT(*) FROM media_items WHERE decrypt_status = 'unsupported'",
    )?;
    let failed_records = count(
        &conn,
        "SELECT COUNT(*) FROM media_items WHERE verify_status = 'failed' OR decrypt_status = 'failed'",
    )?;
    let unique_objects = count(
        &conn,
        "SELECT COUNT(DISTINCT sha256) FROM media_items WHERE sha256 IS NOT NULL AND verify_status = 'ok'",
    )?;
    let unique_bytes = count(
        &conn,
        r#"
        SELECT COALESCE(SUM(size_bytes), 0)
        FROM (
            SELECT sha256, MAX(size_bytes) AS size_bytes
            FROM media_items
            WHERE sha256 IS NOT NULL AND verify_status = 'ok'
            GROUP BY sha256
        )
        "#,
    )?;

    Ok(ArchiveStatus {
        archive_dir: archive_dir.to_path_buf(),
        index_path: index_path(archive_dir),
        total_records,
        archived_records,
        unsupported_records,
        failed_records,
        unique_objects,
        unique_bytes,
    })
}

fn count(conn: &rusqlite::Connection, sql: &str) -> Result<u64> {
    let value: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    Ok(value.max(0) as u64)
}
