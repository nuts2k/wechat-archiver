use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{ArchiverError, Result};
use crate::task_store::{TaskCreate, TaskStore};
use crate::types::{now_epoch_ms, ExtractSummary, ScanAction};

type TaskJob = Box<dyn FnOnce(TaskOptions) -> Result<ExtractSummary> + Send + 'static>;
type SharedTaskStore = Arc<dyn TaskStore + 'static>;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub task_id: String,
    pub task_name: String,
    pub status: TaskStatus,
    pub progress: TaskProgress,
    pub last_event: Option<TaskEvent>,
    pub error: Option<String>,
    pub result: Option<ExtractSummary>,
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

#[derive(Debug, Clone)]
pub struct TaskMetadata {
    pub task_kind: String,
    pub archive_dir: Option<PathBuf>,
    pub source_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub params_summary_json: Value,
}

impl TaskMetadata {
    pub fn new(task_kind: impl Into<String>) -> Self {
        Self {
            task_kind: task_kind.into(),
            archive_dir: None,
            source_dir: None,
            dry_run: false,
            params_summary_json: Value::Object(Map::new()),
        }
    }

    pub fn with_archive_dir(mut self, archive_dir: impl Into<PathBuf>) -> Self {
        self.archive_dir = Some(archive_dir.into());
        self
    }

    pub fn with_source_dir(mut self, source_dir: impl Into<PathBuf>) -> Self {
        self.source_dir = Some(source_dir.into());
        self
    }

    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn with_params_summary_json(mut self, params_summary_json: Value) -> Self {
        self.params_summary_json = params_summary_json;
        self
    }

    fn to_create(&self, task_id: &str, task_name: &str) -> TaskCreate {
        TaskCreate {
            task_id: task_id.to_string(),
            task_name: task_name.to_string(),
            task_kind: self.task_kind.clone(),
            archive_dir: self.archive_dir.clone(),
            source_dir: self.source_dir.clone(),
            dry_run: self.dry_run,
            params_summary_json: self.params_summary_json.clone(),
        }
    }
}

#[derive(Clone)]
pub struct TaskRunner {
    sequence: Arc<std::sync::atomic::AtomicU64>,
    sender: mpsc::Sender<QueuedTask>,
    store: Option<SharedTaskStore>,
}

impl TaskRunner {
    pub fn new() -> Self {
        Self::new_with_optional_store(None)
    }

    pub fn with_store<S>(store: Arc<S>) -> Self
    where
        S: TaskStore + 'static,
    {
        let store: SharedTaskStore = store;
        Self::new_with_optional_store(Some(store))
    }

    fn new_with_optional_store(store: Option<SharedTaskStore>) -> Self {
        let (sender, receiver) = mpsc::channel::<QueuedTask>();
        thread::spawn(move || {
            for task in receiver {
                run_queued_task(task);
            }
        });

        Self {
            sequence: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            sender,
            store,
        }
    }

    pub fn spawn(
        &self,
        task_name: impl Into<String>,
        job: impl FnOnce(TaskOptions) -> Result<ExtractSummary> + Send + 'static,
    ) -> TaskHandle {
        let task_name = task_name.into();
        let metadata = self
            .store
            .as_ref()
            .map(|_| TaskMetadata::new(task_name.clone()));
        self.spawn_inner(task_name, metadata, job)
    }

    pub fn spawn_with_metadata(
        &self,
        task_name: impl Into<String>,
        metadata: TaskMetadata,
        job: impl FnOnce(TaskOptions) -> Result<ExtractSummary> + Send + 'static,
    ) -> TaskHandle {
        self.spawn_inner(task_name.into(), Some(metadata), job)
    }

    fn spawn_inner(
        &self,
        task_name: String,
        metadata: Option<TaskMetadata>,
        job: impl FnOnce(TaskOptions) -> Result<ExtractSummary> + Send + 'static,
    ) -> TaskHandle {
        let task_id = self.next_task_id(self.store.is_some());
        let cancel_token = CancelToken::new();
        let (event_sender, event_receiver) = mpsc::channel::<TaskEvent>();
        let shared = Arc::new(TaskShared::new(TaskSnapshot {
            task_id: task_id.clone(),
            task_name: task_name.clone(),
            status: TaskStatus::Queued,
            progress: TaskProgress::default(),
            last_event: None,
            error: None,
            result: None,
        }));
        let task_create = metadata
            .as_ref()
            .map(|metadata| metadata.to_create(&task_id, &task_name));
        let task_store = if task_create.is_some() {
            self.store.clone()
        } else {
            None
        };
        let handle = TaskHandle {
            task_id: task_id.clone(),
            task_name: task_name.clone(),
            cancel_token: cancel_token.clone(),
            shared: Arc::clone(&shared),
            event_receiver: Arc::new(Mutex::new(event_receiver)),
        };

        if let (Some(store), Some(task_create)) = (&task_store, &task_create) {
            if let Err(error) = store.create_task(task_create) {
                handle.shared.update_terminal(
                    TaskStatus::Failed,
                    None,
                    Some(format!("任务持久化失败: {error}")),
                );
                return handle;
            }
        }

        let queued = QueuedTask {
            task_id: task_id.clone(),
            task_name: task_name.clone(),
            cancel_token: cancel_token.clone(),
            shared: Arc::clone(&shared),
            event_sender,
            store: task_store.clone(),
            job: Box::new(job),
        };

        if let Err(error) = self.sender.send(queued) {
            let error = format!("任务队列已停止: {error}");
            handle
                .shared
                .update_terminal(TaskStatus::Failed, None, Some(error.clone()));
            if let Some(store) = &task_store {
                let _ = store.mark_failed(&task_id, &error);
            }
        }

        handle
    }

    fn next_task_id(&self, persistent: bool) -> String {
        let sequence = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if persistent {
            format!("task-{}-{}-{sequence}", now_epoch_ms(), std::process::id())
        } else {
            format!("task-{sequence}")
        }
    }
}

impl Default for TaskRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TaskRunner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TaskRunner").finish_non_exhaustive()
    }
}

pub struct TaskHandle {
    task_id: String,
    task_name: String,
    cancel_token: CancelToken,
    shared: Arc<TaskShared>,
    event_receiver: Arc<Mutex<mpsc::Receiver<TaskEvent>>>,
}

impl TaskHandle {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn task_name(&self) -> &str {
        &self.task_name
    }

    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }

    pub fn snapshot(&self) -> TaskSnapshot {
        self.shared.snapshot()
    }

    pub fn status(&self) -> TaskStatus {
        self.snapshot().status
    }

    pub fn progress(&self) -> TaskProgress {
        self.snapshot().progress
    }

    pub fn try_recv_event(&self) -> Option<TaskEvent> {
        self.event_receiver.lock().unwrap().try_recv().ok()
    }

    pub fn drain_events(&self) -> Vec<TaskEvent> {
        let receiver = self.event_receiver.lock().unwrap();
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        events
    }

    pub fn join(&self) -> TaskSnapshot {
        self.shared.wait_terminal()
    }
}

impl fmt::Debug for TaskHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskHandle")
            .field("task_id", &self.task_id)
            .field("task_name", &self.task_name)
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

struct QueuedTask {
    task_id: String,
    task_name: String,
    cancel_token: CancelToken,
    shared: Arc<TaskShared>,
    event_sender: mpsc::Sender<TaskEvent>,
    store: Option<SharedTaskStore>,
    job: TaskJob,
}

struct TaskShared {
    snapshot: Mutex<TaskSnapshot>,
    finished: Condvar,
}

impl TaskShared {
    fn new(snapshot: TaskSnapshot) -> Self {
        Self {
            snapshot: Mutex::new(snapshot),
            finished: Condvar::new(),
        }
    }

    fn snapshot(&self) -> TaskSnapshot {
        self.snapshot.lock().unwrap().clone()
    }

    fn mark_running(&self) {
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.status = TaskStatus::Running;
    }

    fn record_event(&self, event: TaskEvent) {
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.progress = event.progress.clone();
        snapshot.last_event = Some(event);
    }

    fn update_terminal(
        &self,
        status: TaskStatus,
        result: Option<ExtractSummary>,
        error: Option<String>,
    ) {
        let mut snapshot = self.snapshot.lock().unwrap();
        if let Some(result) = result {
            snapshot.progress = TaskProgress::from(&result);
            snapshot.result = Some(result);
        }
        snapshot.status = status;
        snapshot.error = error;
        self.finished.notify_all();
    }

    fn wait_terminal(&self) -> TaskSnapshot {
        let mut snapshot = self.snapshot.lock().unwrap();
        while !snapshot.status.is_terminal() {
            snapshot = self.finished.wait(snapshot).unwrap();
        }
        snapshot.clone()
    }
}

fn run_queued_task(task: QueuedTask) {
    task.shared.mark_running();
    if let Some(store) = &task.store {
        let _ = store.mark_running(&task.task_id);
    }
    let reporter = TaskReporter::new({
        let shared = Arc::clone(&task.shared);
        let event_sender = task.event_sender.clone();
        let store = task.store.clone();
        let task_id = task.task_id.clone();
        move |event| {
            shared.record_event(event.clone());
            if let Some(store) = &store {
                let _ = store.update_snapshot(&task_id, &event.progress, Some(&event));
            }
            let _ = event_sender.send(event);
        }
    });
    let options = TaskOptions::new()
        .with_cancel_token(task.cancel_token)
        .with_reporter(reporter);

    let result = panic::catch_unwind(AssertUnwindSafe(|| (task.job)(options)));
    match result {
        Ok(Ok(summary)) => {
            if let Some(store) = &task.store {
                let _ = store.mark_completed(&task.task_id, &summary);
            }
            task.shared
                .update_terminal(TaskStatus::Completed, Some(summary), None);
        }
        Ok(Err(error)) => {
            let status = if matches!(error, ArchiverError::TaskCancelled { .. }) {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Failed
            };
            let error = error.to_string();
            if let Some(store) = &task.store {
                let result = if status == TaskStatus::Cancelled {
                    store.mark_cancelled(&task.task_id, &error)
                } else {
                    store.mark_failed(&task.task_id, &error)
                };
                let _ = result;
            }
            task.shared.update_terminal(status, None, Some(error));
        }
        Err(_) => {
            let error = format!("后台任务 panic: {} ({})", task.task_name, task.task_id);
            if let Some(store) = &task.store {
                let _ = store.mark_failed(&task.task_id, &error);
            }
            task.shared
                .update_terminal(TaskStatus::Failed, None, Some(error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_store::{PersistentTaskStatus, SqliteTaskStore, TaskStore};
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn summary(run_id: &str) -> ExtractSummary {
        let mut summary = ExtractSummary::new(
            run_id.to_string(),
            PathBuf::from("/tmp/source"),
            PathBuf::from("/tmp/archive"),
            false,
        );
        summary.scanned_files = 1;
        summary.candidates = 1;
        summary.archived = 1;
        summary
    }

    fn metadata() -> TaskMetadata {
        TaskMetadata::new("extract_images")
            .with_source_dir("/tmp/source")
            .with_archive_dir("/tmp/archive")
            .with_params_summary_json(json!({
                "task_kind": "extract_images",
                "media_types": ["image"],
                "image_aes_key_provided": false
            }))
    }

    fn wait_for_status(handle: &TaskHandle, status: TaskStatus) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if handle.status() == status {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "task {} did not reach {status:?}, latest={:?}",
            handle.task_id(),
            handle.snapshot()
        );
    }

    fn wait_for_persistent_status(
        store: &impl TaskStore,
        task_id: &str,
        status: PersistentTaskStatus,
    ) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(record) = store.get_task(task_id).unwrap() {
                if record.status == status {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("persistent task {task_id} did not reach {status:?}");
    }

    #[test]
    fn runner_keeps_later_task_queued_until_worker_is_available() {
        let runner = TaskRunner::new();
        let (release_sender, release_receiver) = mpsc::channel();

        let first = runner.spawn("blocking", move |_options| {
            release_receiver.recv().unwrap();
            Ok(summary("first"))
        });
        wait_for_status(&first, TaskStatus::Running);

        let second = runner.spawn("queued", |_options| Ok(summary("second")));
        assert_eq!(second.status(), TaskStatus::Queued);

        release_sender.send(()).unwrap();
        assert_eq!(first.join().status, TaskStatus::Completed);
        assert_eq!(second.join().status, TaskStatus::Completed);
    }

    #[test]
    fn runner_completes_task_and_exposes_events() {
        let runner = TaskRunner::new();
        let handle = runner.spawn("complete", |options| {
            let summary = summary("complete-run");
            options.emit(task_event(
                "complete-run",
                "complete",
                TaskEventKind::CandidateFound,
                &summary,
                None,
                None,
                None,
            ));
            Ok(summary)
        });

        let snapshot = handle.join();
        assert_eq!(snapshot.status, TaskStatus::Completed);
        assert_eq!(snapshot.result.unwrap().run_id, "complete-run");
        assert_eq!(snapshot.progress.archived, 1);

        let events = handle.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TaskEventKind::CandidateFound);
        assert_eq!(events[0].progress.candidates, 1);
    }

    #[test]
    fn runner_records_task_failure() {
        let runner = TaskRunner::new();
        let handle = runner.spawn("failure", |_options| {
            Err(ArchiverError::Other("boom".to_string()))
        });

        let snapshot = handle.join();
        assert_eq!(snapshot.status, TaskStatus::Failed);
        assert_eq!(snapshot.error.as_deref(), Some("boom"));
        assert!(snapshot.result.is_none());
    }

    #[test]
    fn runner_cancels_task_and_emits_cancelled_event() {
        let runner = TaskRunner::new();
        let (started_sender, started_receiver) = mpsc::channel();
        let handle = runner.spawn("cancel", move |options| {
            started_sender.send(()).unwrap();
            while !options.cancel_token.is_cancelled() {
                thread::sleep(Duration::from_millis(5));
            }
            options.check_cancelled("cancel-run", "cancel", TaskProgress::default())?;
            Ok(summary("unreachable"))
        });

        started_receiver.recv().unwrap();
        handle.cancel();

        let snapshot = handle.join();
        assert_eq!(snapshot.status, TaskStatus::Cancelled);
        assert_eq!(snapshot.error.as_deref(), Some("task cancelled: cancel"));

        let events = handle.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TaskEventKind::Cancelled);
        assert_eq!(events[0].run_id, "cancel-run");
    }

    #[test]
    fn runner_with_store_records_queued_task_before_worker_is_available() {
        let store = Arc::new(SqliteTaskStore::open_in_memory().unwrap());
        let runner = TaskRunner::with_store(Arc::clone(&store));
        let (release_sender, release_receiver) = mpsc::channel();

        let first = runner.spawn_with_metadata("blocking", metadata(), move |_options| {
            release_receiver.recv().unwrap();
            Ok(summary("first"))
        });
        wait_for_status(&first, TaskStatus::Running);

        let second =
            runner.spawn_with_metadata("queued", metadata(), |_options| Ok(summary("second")));
        let queued = store.get_task(second.task_id()).unwrap().unwrap();
        assert_eq!(queued.status, PersistentTaskStatus::Queued);
        assert_eq!(queued.task_kind, "extract_images");

        release_sender.send(()).unwrap();
        assert_eq!(first.join().status, TaskStatus::Completed);
        assert_eq!(second.join().status, TaskStatus::Completed);
    }

    #[test]
    fn runner_with_store_records_progress_and_completed_task() {
        let store = Arc::new(SqliteTaskStore::open_in_memory().unwrap());
        let runner = TaskRunner::with_store(Arc::clone(&store));
        let handle = runner.spawn_with_metadata("complete", metadata(), |options| {
            let summary = summary("complete-run");
            options.emit(task_event(
                "complete-run",
                "complete",
                TaskEventKind::CandidateFound,
                &summary,
                None,
                None,
                Some("发现候选媒体".to_string()),
            ));
            Ok(summary)
        });

        let snapshot = handle.join();
        assert_eq!(snapshot.status, TaskStatus::Completed);

        let record = store.get_task(handle.task_id()).unwrap().unwrap();
        assert_eq!(record.status, PersistentTaskStatus::Completed);
        assert_eq!(record.progress.archived, 1);
        assert_eq!(record.result_summary.unwrap().run_id, "complete-run");
        assert_eq!(
            record.last_event.unwrap().kind,
            TaskEventKind::CandidateFound
        );
        assert_eq!(
            record.archive_dir.as_deref(),
            Some(Path::new("/tmp/archive"))
        );
        assert_eq!(
            record.params_summary_json["media_types"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn runner_with_store_records_failed_and_cancelled_tasks() {
        let store = Arc::new(SqliteTaskStore::open_in_memory().unwrap());
        let runner = TaskRunner::with_store(Arc::clone(&store));

        let failed = runner.spawn_with_metadata("failure", metadata(), |_options| {
            Err(ArchiverError::Other("boom".to_string()))
        });
        let failed_snapshot = failed.join();
        assert_eq!(failed_snapshot.status, TaskStatus::Failed);

        let failed_record = store.get_task(failed.task_id()).unwrap().unwrap();
        assert_eq!(failed_record.status, PersistentTaskStatus::Failed);
        assert_eq!(failed_record.error.as_deref(), Some("boom"));

        let (started_sender, started_receiver) = mpsc::channel();
        let cancelled = runner.spawn_with_metadata("cancel", metadata(), move |options| {
            started_sender.send(()).unwrap();
            while !options.cancel_token.is_cancelled() {
                thread::sleep(Duration::from_millis(5));
            }
            options.check_cancelled("cancel-run", "cancel", TaskProgress::default())?;
            Ok(summary("unreachable"))
        });

        started_receiver.recv().unwrap();
        wait_for_persistent_status(
            store.as_ref(),
            cancelled.task_id(),
            PersistentTaskStatus::Running,
        );
        cancelled.cancel();
        let cancelled_snapshot = cancelled.join();
        assert_eq!(cancelled_snapshot.status, TaskStatus::Cancelled);

        let cancelled_record = store.get_task(cancelled.task_id()).unwrap().unwrap();
        assert_eq!(cancelled_record.status, PersistentTaskStatus::Cancelled);
        assert_eq!(
            cancelled_record.error.as_deref(),
            Some("task cancelled: cancel")
        );
        assert_eq!(
            cancelled_record.last_event.unwrap().kind,
            TaskEventKind::Cancelled
        );
    }
}
