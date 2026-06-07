use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::hash::sha256_file;
use crate::index::open_index_readonly;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifySummary {
    pub archive_dir: PathBuf,
    pub checked: u64,
    pub ok: u64,
    pub missing: u64,
    pub unreadable: u64,
    pub mismatched: u64,
    pub failures: Vec<VerifyFailure>,
    pub index_checked: u64,
    pub index_ok: u64,
    pub index_failed: u64,
    pub index_failures: Vec<IndexVerifyFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyFailure {
    pub archive_path: String,
    pub expected_sha256: String,
    pub actual_sha256: Option<String>,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexVerifyFailure {
    pub media_item_id: i64,
    pub source_path: String,
    pub archive_path: Option<String>,
    pub sha256: Option<String>,
    pub error: String,
}

pub fn verify_archive(archive_dir: &Path) -> Result<VerifySummary> {
    let conn = open_index_readonly(archive_dir)?;
    let mut stmt = conn.prepare(
        r#"
        SELECT DISTINCT archive_path, sha256
        FROM media_items
        WHERE archive_path IS NOT NULL
          AND sha256 IS NOT NULL
          AND verify_status = 'ok'
        ORDER BY archive_path
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut summary = VerifySummary {
        archive_dir: archive_dir.to_path_buf(),
        checked: 0,
        ok: 0,
        missing: 0,
        unreadable: 0,
        mismatched: 0,
        failures: Vec::new(),
        index_checked: 0,
        index_ok: 0,
        index_failed: 0,
        index_failures: Vec::new(),
    };

    for row in rows {
        let (archive_path, expected_sha256) = row?;
        summary.checked += 1;
        if !is_safe_relative_archive_path(&archive_path) {
            summary.unreadable += 1;
            summary.failures.push(VerifyFailure {
                archive_path,
                expected_sha256,
                actual_sha256: None,
                error: "invalid_archive_path".to_string(),
            });
            continue;
        }

        let full_path = archive_dir.join(&archive_path);
        if !full_path.exists() {
            summary.missing += 1;
            summary.failures.push(VerifyFailure {
                archive_path,
                expected_sha256,
                actual_sha256: None,
                error: "missing_archive_object".to_string(),
            });
            continue;
        }

        let (actual_sha256, _) = match sha256_file(&full_path) {
            Ok(result) => result,
            Err(_) => {
                summary.unreadable += 1;
                summary.failures.push(VerifyFailure {
                    archive_path,
                    expected_sha256,
                    actual_sha256: None,
                    error: "archive_object_unreadable".to_string(),
                });
                continue;
            }
        };
        if actual_sha256 == expected_sha256 {
            summary.ok += 1;
        } else {
            summary.mismatched += 1;
            summary.failures.push(VerifyFailure {
                archive_path,
                expected_sha256,
                actual_sha256: Some(actual_sha256),
                error: "sha256_mismatch".to_string(),
            });
        }
    }

    verify_index_references(&conn, archive_dir, &mut summary)?;
    Ok(summary)
}

fn verify_index_references(
    conn: &rusqlite::Connection,
    archive_dir: &Path,
    summary: &mut VerifySummary,
) -> Result<()> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, source_path, archive_path, sha256
        FROM media_items
        WHERE verify_status = 'ok'
        ORDER BY id
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    for row in rows {
        let (media_item_id, source_path, archive_path, sha256) = row?;
        summary.index_checked += 1;
        match validate_index_reference(archive_dir, archive_path.as_deref(), sha256.as_deref()) {
            Ok(()) => summary.index_ok += 1,
            Err(error) => {
                summary.index_failed += 1;
                summary.index_failures.push(IndexVerifyFailure {
                    media_item_id,
                    source_path,
                    archive_path,
                    sha256,
                    error,
                });
            }
        }
    }

    Ok(())
}

fn validate_index_reference(
    archive_dir: &Path,
    archive_path: Option<&str>,
    sha256: Option<&str>,
) -> std::result::Result<(), String> {
    let archive_path = archive_path.ok_or_else(|| "missing_archive_path".to_string())?;
    let sha256 = sha256.ok_or_else(|| "missing_sha256".to_string())?;
    if !is_safe_relative_archive_path(archive_path) {
        return Err("invalid_archive_path".to_string());
    }
    if archive_path_sha256(archive_path).as_deref() != Some(sha256) {
        return Err("archive_path_sha256_mismatch".to_string());
    }

    let full_path = archive_dir.join(archive_path);
    if !full_path.exists() {
        return Err("missing_archive_object".to_string());
    }
    let metadata =
        std::fs::metadata(&full_path).map_err(|_| "archive_object_unreadable".to_string())?;
    if !metadata.is_file() {
        return Err("archive_object_not_file".to_string());
    }
    std::fs::File::open(&full_path).map_err(|_| "archive_object_unreadable".to_string())?;
    Ok(())
}

fn is_safe_relative_archive_path(archive_path: &str) -> bool {
    let path = Path::new(archive_path);
    let mut components = path.components().peekable();
    !path.is_absolute()
        && components.peek().is_some()
        && components.all(|component| matches!(component, Component::Normal(_)))
}

fn archive_path_sha256(archive_path: &str) -> Option<String> {
    Path::new(archive_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::object_rel_path;
    use crate::hash::sha256_bytes;
    use crate::index::{insert_record, open_index, MediaRecord};

    fn record(
        source_path: &str,
        archive_path: Option<String>,
        sha256: Option<String>,
    ) -> MediaRecord {
        MediaRecord {
            source_path: source_path.to_string(),
            source_relative_path: source_path
                .rsplit('/')
                .next()
                .unwrap_or(source_path)
                .to_string(),
            source_kind: "direct_file".to_string(),
            media_type: "file".to_string(),
            original_filename: source_path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            mime_type: None,
            message_talker: None,
            message_sender: None,
            message_local_id: None,
            message_create_time: None,
            decoder: None,
            archive_path,
            sha256,
            size_bytes: Some(4),
            extension: Some("bin".to_string()),
            decrypt_status: "not_needed".to_string(),
            verify_status: "ok".to_string(),
            error: None,
            timestamp_epoch_ms: 1,
        }
    }

    fn write_object(archive_dir: &Path, bytes: &[u8], extension: &str) -> (String, String) {
        let (sha256, _) = sha256_bytes(bytes);
        let rel_path = object_rel_path(&sha256, extension);
        let full_path = archive_dir.join(&rel_path);
        std::fs::create_dir_all(full_path.parent().unwrap()).unwrap();
        std::fs::write(&full_path, bytes).unwrap();
        (rel_path.to_string_lossy().to_string(), sha256)
    }

    #[test]
    fn verify_archive_checks_index_references() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let conn = open_index(archive).unwrap();
        let (archive_path, sha256) = write_object(archive, b"good", "bin");
        insert_record(
            &conn,
            &record("/tmp/source/good.bin", Some(archive_path), Some(sha256)),
        )
        .unwrap();
        insert_record(
            &conn,
            &record(
                "/tmp/source/missing-path.bin",
                None,
                Some("abc123".to_string()),
            ),
        )
        .unwrap();
        insert_record(
            &conn,
            &record(
                "/tmp/source/path-mismatch.bin",
                Some("objects/sha256/ab/not-the-index-hash.bin".to_string()),
                Some("abc123".to_string()),
            ),
        )
        .unwrap();
        drop(conn);

        let summary = verify_archive(archive).unwrap();

        assert_eq!(summary.checked, 2);
        assert_eq!(summary.ok, 1);
        assert_eq!(summary.missing, 1);
        assert_eq!(summary.unreadable, 0);
        assert_eq!(summary.index_checked, 3);
        assert_eq!(summary.index_ok, 1);
        assert_eq!(summary.index_failed, 2);
        let errors = summary
            .index_failures
            .iter()
            .map(|failure| failure.error.as_str())
            .collect::<Vec<_>>();
        assert!(errors.contains(&"missing_archive_path"));
        assert!(errors.contains(&"archive_path_sha256_mismatch"));
    }

    #[test]
    fn verify_index_reference_rejects_unsafe_archive_paths() {
        let tmp = tempfile::tempdir().unwrap();

        assert_eq!(
            validate_index_reference(tmp.path(), Some("../outside.bin"), Some("outside")),
            Err("invalid_archive_path".to_string())
        );
        assert_eq!(
            validate_index_reference(tmp.path(), Some("/tmp/outside.bin"), Some("outside")),
            Err("invalid_archive_path".to_string())
        );
    }

    #[test]
    fn verify_archive_rejects_unsafe_object_paths_before_reading() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("archive");
        let outside_path = tmp.path().join("outside.bin");
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::write(&outside_path, b"evil").unwrap();
        let (outside_sha256, _) = sha256_bytes(b"evil");

        let conn = open_index(&archive).unwrap();
        insert_record(
            &conn,
            &record(
                "/tmp/source/absolute.bin",
                Some(outside_path.to_string_lossy().to_string()),
                Some(outside_sha256.clone()),
            ),
        )
        .unwrap();
        insert_record(
            &conn,
            &record(
                "/tmp/source/traversal.bin",
                Some("../outside.bin".to_string()),
                Some(outside_sha256),
            ),
        )
        .unwrap();
        drop(conn);

        let summary = verify_archive(&archive).unwrap();

        assert_eq!(summary.checked, 2);
        assert_eq!(summary.ok, 0);
        assert_eq!(summary.missing, 0);
        assert_eq!(summary.unreadable, 2);
        assert_eq!(summary.mismatched, 0);
        assert_eq!(summary.index_checked, 2);
        assert_eq!(summary.index_ok, 0);
        assert_eq!(summary.index_failed, 2);
        assert!(summary
            .failures
            .iter()
            .all(|failure| failure.error == "invalid_archive_path"));
        assert!(summary
            .index_failures
            .iter()
            .all(|failure| failure.error == "invalid_archive_path"));
    }
}
