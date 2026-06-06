use std::path::{Path, PathBuf};

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
    pub mismatched: u64,
    pub failures: Vec<VerifyFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyFailure {
    pub archive_path: String,
    pub expected_sha256: String,
    pub actual_sha256: Option<String>,
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
        mismatched: 0,
        failures: Vec::new(),
    };

    for row in rows {
        let (archive_path, expected_sha256) = row?;
        summary.checked += 1;
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

        let (actual_sha256, _) = sha256_file(&full_path)?;
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

    Ok(summary)
}
