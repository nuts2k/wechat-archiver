use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{ArchiverError, Result};
use crate::types::{ExtractSummary, ScanAction};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    Started,
    ScanSourceStarted,
    FileScanned,
    CandidateFound,
    ItemFinished,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskProgress {
    pub scanned_files: u64,
    pub candidates: u64,
    pub would_archive: u64,
    pub archived: u64,
    pub already_archived: u64,
    pub reused_records: u64,
    pub decoded_dat: u64,
    pub metadata_backfilled: u64,
    pub new_objects: u64,
    pub existing_objects: u64,
    pub unsupported: u64,
    pub failed: u64,
}

impl From<&ExtractSummary> for TaskProgress {
    fn from(summary: &ExtractSummary) -> Self {
        Self {
            scanned_files: summary.scanned_files,
            candidates: summary.candidates,
            would_archive: summary.would_archive,
            archived: summary.archived,
            already_archived: summary.already_archived,
            reused_records: summary.reused_records,
            decoded_dat: summary.decoded_dat,
            metadata_backfilled: summary.metadata_backfilled,
            new_objects: summary.new_objects,
            existing_objects: summary.existing_objects,
            unsupported: summary.unsupported,
            failed: summary.failed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskEvent {
    pub run_id: String,
    pub task_name: String,
    pub kind: TaskEventKind,
    pub progress: TaskProgress,
    pub source_path: Option<PathBuf>,
    pub action: Option<ScanAction>,
    pub message: Option<String>,
}

#[derive(Clone)]
pub struct TaskReporter {
    handler: Arc<dyn Fn(TaskEvent) + Send + Sync + 'static>,
}

impl TaskReporter {
    pub fn new(handler: impl Fn(TaskEvent) + Send + Sync + 'static) -> Self {
        Self {
            handler: Arc::new(handler),
        }
    }

    fn emit(&self, event: TaskEvent) {
        (self.handler)(event);
    }
}

impl fmt::Debug for TaskReporter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskReporter")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Default)]
pub struct TaskOptions {
    pub cancel_token: CancelToken,
    pub reporter: Option<TaskReporter>,
}

impl TaskOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cancel_token(mut self, cancel_token: CancelToken) -> Self {
        self.cancel_token = cancel_token;
        self
    }

    pub fn with_reporter(mut self, reporter: TaskReporter) -> Self {
        self.reporter = Some(reporter);
        self
    }

    pub(crate) fn check_cancelled(
        &self,
        run_id: &str,
        task_name: &str,
        progress: TaskProgress,
    ) -> Result<()> {
        if !self.cancel_token.is_cancelled() {
            return Ok(());
        }

        self.emit(TaskEvent {
            run_id: run_id.to_string(),
            task_name: task_name.to_string(),
            kind: TaskEventKind::Cancelled,
            progress,
            source_path: None,
            action: None,
            message: Some("任务已取消".to_string()),
        });
        Err(ArchiverError::TaskCancelled {
            task_name: task_name.to_string(),
        })
    }

    pub(crate) fn emit(&self, event: TaskEvent) {
        if let Some(reporter) = &self.reporter {
            reporter.emit(event);
        }
    }
}

pub(crate) fn task_event(
    run_id: &str,
    task_name: &str,
    kind: TaskEventKind,
    summary: &ExtractSummary,
    source_path: Option<&Path>,
    action: Option<ScanAction>,
    message: Option<String>,
) -> TaskEvent {
    TaskEvent {
        run_id: run_id.to_string(),
        task_name: task_name.to_string(),
        kind,
        progress: TaskProgress::from(summary),
        source_path: source_path.map(Path::to_path_buf),
        action,
        message,
    }
}
