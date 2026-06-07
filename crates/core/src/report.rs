use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::index::{index_path, open_index_readonly};
use crate::lookup::{query_all_records, IndexLookupRecord};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveReport {
    pub archive_dir: PathBuf,
    pub index_path: PathBuf,
    pub total_records: u64,
    pub archived_records: u64,
    pub failed_records: u64,
    pub unsupported_records: u64,
    pub records: Vec<IndexLookupRecord>,
}

pub fn archive_report(archive_dir: &Path) -> Result<ArchiveReport> {
    let conn = open_index_readonly(archive_dir)?;
    let records = query_all_records(&conn)?;
    let archived_records = records
        .iter()
        .filter(|record| record.verify_status == "ok")
        .count() as u64;
    let failed_records = records
        .iter()
        .filter(|record| record.verify_status == "failed" || record.decrypt_status == "failed")
        .count() as u64;
    let unsupported_records = records
        .iter()
        .filter(|record| record.decrypt_status == "unsupported")
        .count() as u64;

    Ok(ArchiveReport {
        archive_dir: archive_dir.to_path_buf(),
        index_path: index_path(archive_dir),
        total_records: records.len() as u64,
        archived_records,
        failed_records,
        unsupported_records,
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{insert_record, open_index, MediaRecord};

    fn record(
        source_path: &str,
        sha256: Option<&str>,
        decrypt_status: &str,
        verify_status: &str,
    ) -> MediaRecord {
        MediaRecord {
            source_path: source_path.to_string(),
            source_relative_path: Path::new(source_path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            source_kind: "direct_image".to_string(),
            media_type: "image".to_string(),
            message_talker: Some("chat_a".to_string()),
            message_sender: None,
            message_local_id: Some(42),
            message_create_time: Some(1_700_000_000),
            decoder: None,
            archive_path: sha256.map(|sha256| format!("objects/sha256/ab/{sha256}.jpg")),
            sha256: sha256.map(str::to_string),
            size_bytes: Some(123),
            extension: Some("jpg".to_string()),
            decrypt_status: decrypt_status.to_string(),
            verify_status: verify_status.to_string(),
            error: if verify_status == "failed" {
                Some("copy_failed".to_string())
            } else {
                None
            },
            timestamp_epoch_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn archive_report_reads_all_records_in_id_order() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let conn = open_index(archive).unwrap();
        insert_record(
            &conn,
            &record("/tmp/source/a.jpg", Some("abc123"), "not_needed", "ok"),
        )
        .unwrap();
        insert_record(
            &conn,
            &record("/tmp/source/b.jpg", None, "failed", "failed"),
        )
        .unwrap();
        insert_record(
            &conn,
            &record("/tmp/source/c.dat", None, "unsupported", "skipped"),
        )
        .unwrap();
        drop(conn);

        let report = archive_report(archive).unwrap();

        assert_eq!(report.total_records, 3);
        assert_eq!(report.archived_records, 1);
        assert_eq!(report.failed_records, 1);
        assert_eq!(report.unsupported_records, 1);
        assert_eq!(report.records[0].source_path, "/tmp/source/a.jpg");
        assert_eq!(report.records[1].source_path, "/tmp/source/b.jpg");
        assert_eq!(report.records[2].source_path, "/tmp/source/c.dat");
        assert_eq!(report.records[0].message_talker.as_deref(), Some("chat_a"));
        assert_eq!(report.records[1].error.as_deref(), Some("copy_failed"));
    }
}
