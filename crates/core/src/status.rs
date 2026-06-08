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
    pub media_type_counts: Vec<StatusCount>,
    pub source_kind_counts: Vec<StatusCount>,
    pub decrypt_status_counts: Vec<StatusCount>,
    pub verify_status_counts: Vec<StatusCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusCount {
    pub value: String,
    pub count: u64,
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
                media_type_counts: Vec::new(),
                source_kind_counts: Vec::new(),
                decrypt_status_counts: Vec::new(),
                verify_status_counts: Vec::new(),
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
    let media_type_counts = group_counts(&conn, "media_type")?;
    let source_kind_counts = group_counts(&conn, "source_kind")?;
    let decrypt_status_counts = group_counts(&conn, "decrypt_status")?;
    let verify_status_counts = group_counts(&conn, "verify_status")?;

    Ok(ArchiveStatus {
        archive_dir: archive_dir.to_path_buf(),
        index_path: index_path(archive_dir),
        total_records,
        archived_records,
        unsupported_records,
        failed_records,
        unique_objects,
        unique_bytes,
        media_type_counts,
        source_kind_counts,
        decrypt_status_counts,
        verify_status_counts,
    })
}

fn count(conn: &rusqlite::Connection, sql: &str) -> Result<u64> {
    let value: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    Ok(value.max(0) as u64)
}

fn group_counts(conn: &rusqlite::Connection, column_name: &str) -> Result<Vec<StatusCount>> {
    let sql = format!(
        r#"
        SELECT {column_name}, COUNT(*)
        FROM media_items
        GROUP BY {column_name}
        ORDER BY COUNT(*) DESC, {column_name}
        "#
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        let count: i64 = row.get(1)?;
        Ok(StatusCount {
            value: row.get(0)?,
            count: count.max(0) as u64,
        })
    })?;
    let counts = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{insert_record, open_index, MediaRecord};

    fn record(source_path: &str, media_type: &str, source_kind: &str) -> MediaRecord {
        MediaRecord {
            source_path: source_path.to_string(),
            source_relative_path: source_path
                .rsplit('/')
                .next()
                .unwrap_or(source_path)
                .to_string(),
            source_kind: source_kind.to_string(),
            media_type: media_type.to_string(),
            original_filename: source_path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            mime_type: Some("image/jpeg".to_string()),
            width_px: Some(640),
            height_px: Some(480),
            duration_ms: None,
            message_talker: None,
            message_sender: None,
            message_local_id: None,
            message_create_time: None,
            decoder: None,
            archive_path: Some(format!("objects/sha256/ab/{source_path}.jpg")),
            sha256: Some(source_path.to_string()),
            size_bytes: Some(10),
            source_size_bytes: Some(10),
            source_modified_ms: Some(1),
            extension: Some("jpg".to_string()),
            decrypt_status: "not_needed".to_string(),
            verify_status: "ok".to_string(),
            error: None,
            timestamp_epoch_ms: 1,
        }
    }

    fn count_for(counts: &[StatusCount], value: &str) -> u64 {
        counts
            .iter()
            .find(|count| count.value == value)
            .map(|count| count.count)
            .unwrap_or(0)
    }

    #[test]
    fn archive_status_reports_group_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let conn = open_index(archive).unwrap();
        insert_record(&conn, &record("image-a", "image", "direct_image")).unwrap();
        insert_record(&conn, &record("image-b", "image", "direct_image")).unwrap();
        insert_record(&conn, &record("video-a", "video", "direct_video")).unwrap();
        drop(conn);

        let status = archive_status(archive).unwrap();

        assert_eq!(status.total_records, 3);
        assert_eq!(count_for(&status.media_type_counts, "image"), 2);
        assert_eq!(count_for(&status.media_type_counts, "video"), 1);
        assert_eq!(count_for(&status.source_kind_counts, "direct_image"), 2);
        assert_eq!(count_for(&status.source_kind_counts, "direct_video"), 1);
        assert_eq!(count_for(&status.decrypt_status_counts, "not_needed"), 3);
        assert_eq!(count_for(&status.verify_status_counts, "ok"), 3);
    }
}
