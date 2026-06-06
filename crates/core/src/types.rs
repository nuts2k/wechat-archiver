use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanAction {
    Archived,
    AlreadyArchived,
    Unsupported,
    Failed,
    WouldArchive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEvent {
    pub event: String,
    pub run_id: String,
    pub timestamp_epoch_ms: u128,
    pub source_path: String,
    pub source_relative_path: String,
    pub source_kind: String,
    pub media_type: String,
    pub action: ScanAction,
    pub archive_path: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub extension: Option<String>,
    pub decrypt_status: String,
    pub verify_status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractSummary {
    pub run_id: String,
    pub source_dir: PathBuf,
    pub archive_dir: PathBuf,
    pub dry_run: bool,
    pub scanned_files: u64,
    pub candidates: u64,
    pub would_archive: u64,
    pub archived: u64,
    pub already_archived: u64,
    pub unsupported: u64,
    pub failed: u64,
    pub manifest_path: Option<PathBuf>,
    pub index_path: Option<PathBuf>,
}

impl ExtractSummary {
    pub(crate) fn new(
        run_id: String,
        source_dir: PathBuf,
        archive_dir: PathBuf,
        dry_run: bool,
    ) -> Self {
        Self {
            run_id,
            source_dir,
            archive_dir,
            dry_run,
            scanned_files: 0,
            candidates: 0,
            would_archive: 0,
            archived: 0,
            already_archived: 0,
            unsupported: 0,
            failed: 0,
            manifest_path: None,
            index_path: None,
        }
    }
}

pub(crate) fn now_epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
