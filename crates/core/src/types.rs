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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported_explanation: Option<UnsupportedExplanation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UnsupportedExplanation {
    pub reasons: Vec<UnsupportedReasonSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsupportedReasonSummary {
    pub reason: String,
    pub count: u64,
    pub samples: Vec<String>,
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
            unsupported_explanation: None,
        }
    }

    pub(crate) fn enable_unsupported_explanation(&mut self) {
        self.unsupported_explanation = Some(UnsupportedExplanation::default());
    }

    pub(crate) fn record_unsupported(&mut self, reason: String, sample: Option<String>) {
        let Some(explanation) = self.unsupported_explanation.as_mut() else {
            return;
        };
        let entry = explanation
            .reasons
            .iter_mut()
            .find(|entry| entry.reason == reason);
        let entry = match entry {
            Some(entry) => entry,
            None => {
                explanation.reasons.push(UnsupportedReasonSummary {
                    reason,
                    count: 0,
                    samples: Vec::new(),
                });
                explanation
                    .reasons
                    .last_mut()
                    .expect("inserted reason must exist")
            }
        };
        entry.count += 1;
        if let Some(sample) = sample {
            if entry.samples.len() < 3 {
                entry.samples.push(sample);
            }
        }
    }

    pub(crate) fn finish_unsupported_explanation(&mut self) {
        let Some(explanation) = self.unsupported_explanation.as_mut() else {
            return;
        };
        explanation.reasons.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then(left.reason.cmp(&right.reason))
        });
    }
}

pub(crate) fn now_epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
