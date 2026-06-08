use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::error::{ArchiverError, Result};
use crate::task::{TaskEvent, TaskProgress};
use crate::types::{now_epoch_ms, ExtractSummary};

const CURRENT_SCHEMA_VERSION: i64 = 1;

type MigrationFn = fn(&Connection) -> Result<()>;

struct Migration {
    version: i64,
    name: &'static str,
    apply: MigrationFn,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "create_task_runs",
    apply: migration_1_create_task_runs,
}];

const TASK_RECORD_COLUMNS: &str = r#"
    task_id,
    task_name,
    task_kind,
    archive_dir,
    source_dir,
    status,
    created_at_ms,
    started_at_ms,
    finished_at_ms,
    dry_run,
    params_summary_json,
    progress_json,
    last_event_json,
    result_summary_json,
    error
"#;

pub trait TaskStore: Send + Sync {
    fn create_task(&self, task: &TaskCreate) -> Result<TaskRecord>;
    fn mark_running(&self, task_id: &str) -> Result<()>;
    fn update_snapshot(
        &self,
        task_id: &str,
        progress: &TaskProgress,
        last_event: Option<&TaskEvent>,
    ) -> Result<()>;
    fn finish_task(
        &self,
        task_id: &str,
        status: PersistentTaskStatus,
        result_summary: Option<&ExtractSummary>,
        error: Option<&str>,
    ) -> Result<()>;
    fn mark_unfinished_interrupted(&self, error: &str) -> Result<usize>;
    fn get_task(&self, task_id: &str) -> Result<Option<TaskRecord>>;
    fn list_tasks(&self, query: &TaskListQuery) -> Result<Vec<TaskRecord>>;

    fn mark_completed(&self, task_id: &str, result_summary: &ExtractSummary) -> Result<()> {
        self.finish_task(
            task_id,
            PersistentTaskStatus::Completed,
            Some(result_summary),
            None,
        )
    }

    fn mark_failed(&self, task_id: &str, error: &str) -> Result<()> {
        self.finish_task(task_id, PersistentTaskStatus::Failed, None, Some(error))
    }

    fn mark_cancelled(&self, task_id: &str, error: &str) -> Result<()> {
        self.finish_task(task_id, PersistentTaskStatus::Cancelled, None, Some(error))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistentTaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl PersistentTaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(ArchiverError::Other(format!("未知任务持久化状态: {value}"))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskCreate {
    pub task_id: String,
    pub task_name: String,
    pub task_kind: String,
    pub archive_dir: Option<PathBuf>,
    pub source_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub params_summary_json: JsonValue,
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub task_id: String,
    pub task_name: String,
    pub task_kind: String,
    pub archive_dir: Option<PathBuf>,
    pub source_dir: Option<PathBuf>,
    pub status: PersistentTaskStatus,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub dry_run: bool,
    pub params_summary_json: JsonValue,
    pub progress: TaskProgress,
    pub last_event: Option<TaskEvent>,
    pub result_summary: Option<ExtractSummary>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskListQuery {
    pub statuses: Vec<PersistentTaskStatus>,
    pub task_kind: Option<String>,
    pub created_at_from_ms: Option<i64>,
    pub created_at_to_ms: Option<i64>,
    pub limit: Option<usize>,
}

impl TaskListQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_status(mut self, status: PersistentTaskStatus) -> Self {
        self.statuses.push(status);
        self
    }

    pub fn with_statuses(
        mut self,
        statuses: impl IntoIterator<Item = PersistentTaskStatus>,
    ) -> Self {
        self.statuses.extend(statuses);
        self
    }

    pub fn with_task_kind(mut self, task_kind: impl Into<String>) -> Self {
        self.task_kind = Some(task_kind.into());
        self
    }

    pub fn with_created_at_from_ms(mut self, created_at_from_ms: i64) -> Self {
        self.created_at_from_ms = Some(created_at_from_ms);
        self
    }

    pub fn with_created_at_to_ms(mut self, created_at_to_ms: i64) -> Self {
        self.created_at_to_ms = Some(created_at_to_ms);
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

#[derive(Debug)]
pub struct SqliteTaskStore {
    conn: Mutex<Connection>,
}

impl SqliteTaskStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        initialize(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        initialize(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| ArchiverError::Other("任务历史数据库锁已损坏".to_string()))
    }
}

impl TaskStore for SqliteTaskStore {
    fn create_task(&self, task: &TaskCreate) -> Result<TaskRecord> {
        let created_at_ms = now_ms();
        let progress = TaskProgress::default();
        let params_summary_json = serde_json::to_string(&task.params_summary_json)?;
        let progress_json = serde_json::to_string(&progress)?;

        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO task_runs (
                task_id,
                task_name,
                task_kind,
                archive_dir,
                source_dir,
                status,
                created_at_ms,
                dry_run,
                params_summary_json,
                progress_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                task.task_id,
                task.task_name,
                task.task_kind,
                path_to_string(task.archive_dir.as_deref()),
                path_to_string(task.source_dir.as_deref()),
                PersistentTaskStatus::Queued.as_str(),
                created_at_ms,
                bool_to_int(task.dry_run),
                params_summary_json,
                progress_json,
            ],
        )?;

        Ok(TaskRecord {
            task_id: task.task_id.clone(),
            task_name: task.task_name.clone(),
            task_kind: task.task_kind.clone(),
            archive_dir: task.archive_dir.clone(),
            source_dir: task.source_dir.clone(),
            status: PersistentTaskStatus::Queued,
            created_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
            dry_run: task.dry_run,
            params_summary_json: task.params_summary_json.clone(),
            progress,
            last_event: None,
            result_summary: None,
            error: None,
        })
    }

    fn mark_running(&self, task_id: &str) -> Result<()> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE task_runs
            SET status = ?2,
                started_at_ms = COALESCE(started_at_ms, ?3)
            WHERE task_id = ?1
            "#,
            params![task_id, PersistentTaskStatus::Running.as_str(), now_ms()],
        )?;
        ensure_task_updated(task_id, updated)
    }

    fn update_snapshot(
        &self,
        task_id: &str,
        progress: &TaskProgress,
        last_event: Option<&TaskEvent>,
    ) -> Result<()> {
        let progress_json = serde_json::to_string(progress)?;
        let last_event_json = last_event.map(serde_json::to_string).transpose()?;
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE task_runs
            SET progress_json = ?2,
                last_event_json = ?3
            WHERE task_id = ?1
            "#,
            params![task_id, progress_json, last_event_json],
        )?;
        ensure_task_updated(task_id, updated)
    }

    fn finish_task(
        &self,
        task_id: &str,
        status: PersistentTaskStatus,
        result_summary: Option<&ExtractSummary>,
        error: Option<&str>,
    ) -> Result<()> {
        if !status.is_terminal() || status == PersistentTaskStatus::Interrupted {
            return Err(ArchiverError::Other(format!(
                "finish_task 不接受非业务终态: {}",
                status.as_str()
            )));
        }

        let result_summary_json = result_summary.map(serde_json::to_string).transpose()?;
        let progress_json = match result_summary {
            Some(summary) => serde_json::to_string(&TaskProgress::from(summary))?,
            None => self.current_progress_json(task_id)?,
        };
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE task_runs
            SET status = ?2,
                finished_at_ms = COALESCE(finished_at_ms, ?3),
                progress_json = ?4,
                result_summary_json = ?5,
                error = ?6
            WHERE task_id = ?1
            "#,
            params![
                task_id,
                status.as_str(),
                now_ms(),
                progress_json,
                result_summary_json,
                error,
            ],
        )?;
        ensure_task_updated(task_id, updated)
    }

    fn mark_unfinished_interrupted(&self, error: &str) -> Result<usize> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE task_runs
            SET status = ?1,
                finished_at_ms = COALESCE(finished_at_ms, ?2),
                error = ?3
            WHERE status IN (?4, ?5)
            "#,
            params![
                PersistentTaskStatus::Interrupted.as_str(),
                now_ms(),
                error,
                PersistentTaskStatus::Queued.as_str(),
                PersistentTaskStatus::Running.as_str(),
            ],
        )?;
        Ok(updated)
    }

    fn get_task(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        let conn = self.connection()?;
        let sql = format!(
            r#"
            SELECT {TASK_RECORD_COLUMNS}
            FROM task_runs
            WHERE task_id = ?1
            "#
        );
        conn.query_row(&sql, params![task_id], task_record_from_row)
            .optional()
            .map_err(ArchiverError::from)
            .and_then(|record| record.transpose())
    }

    fn list_tasks(&self, query: &TaskListQuery) -> Result<Vec<TaskRecord>> {
        let conn = self.connection()?;
        let (sql, params) = build_list_tasks_query(query);
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(params), task_record_from_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(ArchiverError::from)??);
        }
        Ok(records)
    }
}

impl SqliteTaskStore {
    fn current_progress_json(&self, task_id: &str) -> Result<String> {
        let conn = self.connection()?;
        let progress_json = conn
            .query_row(
                "SELECT progress_json FROM task_runs WHERE task_id = ?1",
                params![task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| ArchiverError::Other(format!("任务不存在: {task_id}")))?;
        Ok(progress_json)
    }
}

fn initialize(conn: &Connection) -> Result<()> {
    debug_assert_eq!(
        MIGRATIONS.last().map(|migration| migration.version),
        Some(CURRENT_SCHEMA_VERSION)
    );
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        "#,
    )?;
    ensure_schema_migrations_table(conn)?;
    for migration in MIGRATIONS {
        apply_migration(conn, migration)?;
    }
    Ok(())
}

fn ensure_schema_migrations_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        );
        "#,
    )?;
    Ok(())
}

fn apply_migration(conn: &Connection, migration: &Migration) -> Result<()> {
    if migration_applied(conn, migration.version)? {
        return Ok(());
    }

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        if migration_applied(conn, migration.version)? {
            return Ok(());
        }
        (migration.apply)(conn)?;
        conn.execute(
            r#"
            INSERT INTO schema_migrations (version, name, applied_at_ms)
            VALUES (?1, ?2, ?3)
            "#,
            params![migration.version, migration.name, now_ms()],
        )?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

fn migration_applied(conn: &Connection, version: i64) -> Result<bool> {
    let applied = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
        params![version],
        |row| row.get::<_, i64>(0),
    )? != 0;
    Ok(applied)
}

fn migration_1_create_task_runs(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS task_runs (
            task_id TEXT PRIMARY KEY,
            task_name TEXT NOT NULL,
            task_kind TEXT NOT NULL,
            archive_dir TEXT,
            source_dir TEXT,
            status TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            started_at_ms INTEGER,
            finished_at_ms INTEGER,
            dry_run INTEGER NOT NULL DEFAULT 0,
            params_summary_json TEXT NOT NULL,
            progress_json TEXT NOT NULL,
            last_event_json TEXT,
            result_summary_json TEXT,
            error TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_task_runs_created_at
            ON task_runs(created_at_ms);

        CREATE INDEX IF NOT EXISTS idx_task_runs_status
            ON task_runs(status);
        "#,
    )?;
    Ok(())
}

fn task_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<TaskRecord>> {
    let status = row.get::<_, String>(5)?;
    let params_summary_json = row.get::<_, String>(10)?;
    let progress_json = row.get::<_, String>(11)?;
    let last_event_json = row.get::<_, Option<String>>(12)?;
    let result_summary_json = row.get::<_, Option<String>>(13)?;

    Ok((|| -> Result<TaskRecord> {
        Ok(TaskRecord {
            task_id: row.get(0)?,
            task_name: row.get(1)?,
            task_kind: row.get(2)?,
            archive_dir: row.get::<_, Option<String>>(3)?.map(PathBuf::from),
            source_dir: row.get::<_, Option<String>>(4)?.map(PathBuf::from),
            status: PersistentTaskStatus::parse(&status)?,
            created_at_ms: row.get(6)?,
            started_at_ms: row.get(7)?,
            finished_at_ms: row.get(8)?,
            dry_run: row.get::<_, i64>(9)? != 0,
            params_summary_json: serde_json::from_str(&params_summary_json)?,
            progress: serde_json::from_str(&progress_json)?,
            last_event: last_event_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            result_summary: result_summary_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            error: row.get(14)?,
        })
    })())
}

fn build_list_tasks_query(query: &TaskListQuery) -> (String, Vec<SqlValue>) {
    let mut sql = format!(
        r#"
        SELECT {TASK_RECORD_COLUMNS}
        FROM task_runs
        "#
    );
    let mut clauses = Vec::new();
    let mut params = Vec::new();

    if !query.statuses.is_empty() {
        let placeholders = (0..query.statuses.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        clauses.push(format!("status IN ({placeholders})"));
        params.extend(
            query
                .statuses
                .iter()
                .map(|status| SqlValue::Text(status.as_str().to_string())),
        );
    }

    if let Some(task_kind) = &query.task_kind {
        clauses.push("task_kind = ?".to_string());
        params.push(SqlValue::Text(task_kind.clone()));
    }

    if let Some(created_at_from_ms) = query.created_at_from_ms {
        clauses.push("created_at_ms >= ?".to_string());
        params.push(SqlValue::Integer(created_at_from_ms));
    }

    if let Some(created_at_to_ms) = query.created_at_to_ms {
        clauses.push("created_at_ms <= ?".to_string());
        params.push(SqlValue::Integer(created_at_to_ms));
    }

    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }

    sql.push_str(" ORDER BY created_at_ms DESC, task_id DESC");
    if let Some(limit) = query.limit {
        sql.push_str(" LIMIT ?");
        params.push(SqlValue::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
    }

    (sql, params)
}

fn ensure_task_updated(task_id: &str, updated: usize) -> Result<()> {
    if updated == 0 {
        return Err(ArchiverError::Other(format!("任务不存在: {task_id}")));
    }
    Ok(())
}

fn bool_to_int(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn path_to_string(path: Option<&Path>) -> Option<String> {
    path.map(|path| path.to_string_lossy().to_string())
}

fn now_ms() -> i64 {
    i64::try_from(now_epoch_ms()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task_create(task_id: &str) -> TaskCreate {
        task_create_with_kind(task_id, "extract_images")
    }

    fn task_create_with_kind(task_id: &str, task_kind: &str) -> TaskCreate {
        TaskCreate {
            task_id: task_id.to_string(),
            task_name: "抽取图片".to_string(),
            task_kind: task_kind.to_string(),
            archive_dir: Some(PathBuf::from("/tmp/archive")),
            source_dir: Some(PathBuf::from("/tmp/source")),
            dry_run: false,
            params_summary_json: json!({
                "task_kind": "extract_images",
                "image_aes_key_provided": true,
                "image_aes_key_sha256": "cf20965eec1a6a1024eba8120c5b33a98a8e4e3b0f2a8218ecf7d70ac8a3f1bb"
            }),
        }
    }

    fn task_create_with_paths(
        task_id: &str,
        source_dir: PathBuf,
        archive_dir: PathBuf,
    ) -> TaskCreate {
        TaskCreate {
            source_dir: Some(source_dir),
            archive_dir: Some(archive_dir),
            ..task_create(task_id)
        }
    }

    fn set_created_at_ms(store: &SqliteTaskStore, task_id: &str, created_at_ms: i64) {
        let conn = store.connection().unwrap();
        conn.execute(
            "UPDATE task_runs SET created_at_ms = ?2 WHERE task_id = ?1",
            params![task_id, created_at_ms],
        )
        .unwrap();
    }

    fn summary(run_id: &str) -> ExtractSummary {
        let mut summary = ExtractSummary::new(
            run_id.to_string(),
            PathBuf::from("/tmp/source"),
            PathBuf::from("/tmp/archive"),
            false,
        );
        summary.scanned_files = 2;
        summary.candidates = 2;
        summary.archived = 1;
        summary.failed = 1;
        summary
    }

    fn event(progress: TaskProgress) -> TaskEvent {
        TaskEvent {
            run_id: "run-1".to_string(),
            task_name: "抽取图片".to_string(),
            kind: crate::task::TaskEventKind::CandidateFound,
            progress,
            source_path: Some(PathBuf::from("/tmp/source/a.dat")),
            action: Some(crate::types::ScanAction::WouldArchive),
            message: Some("发现候选媒体".to_string()),
        }
    }

    #[test]
    fn schema_initializes_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("app.sqlite");

        let first = SqliteTaskStore::open(&db_path).unwrap();
        drop(first);
        let second = SqliteTaskStore::open(&db_path).unwrap();

        let conn = second.connection().unwrap();
        let migration_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        let task_table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'task_runs'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(migration_count, CURRENT_SCHEMA_VERSION);
        assert_eq!(task_table_count, 1);
    }

    #[test]
    fn task_store_creates_and_updates_terminal_task() {
        let store = SqliteTaskStore::open_in_memory().unwrap();
        let created = store.create_task(&task_create("task-1")).unwrap();
        assert_eq!(created.status, PersistentTaskStatus::Queued);
        assert_eq!(created.progress, TaskProgress::default());

        store.mark_running("task-1").unwrap();

        let progress = TaskProgress {
            scanned_files: 2,
            candidates: 2,
            ..TaskProgress::default()
        };
        let last_event = event(progress.clone());
        store
            .update_snapshot("task-1", &progress, Some(&last_event))
            .unwrap();

        let result = summary("run-1");
        store.mark_completed("task-1", &result).unwrap();

        let record = store.get_task("task-1").unwrap().unwrap();
        assert_eq!(record.status, PersistentTaskStatus::Completed);
        assert!(record.started_at_ms.is_some());
        assert!(record.finished_at_ms.is_some());
        assert_eq!(record.progress.archived, 1);
        assert_eq!(record.last_event.unwrap().kind, last_event.kind);
        assert_eq!(record.result_summary.unwrap().run_id, "run-1");
        assert!(record.error.is_none());
    }

    #[test]
    fn task_store_writes_failed_and_cancelled_terminal_tasks() {
        let store = SqliteTaskStore::open_in_memory().unwrap();
        store.create_task(&task_create("failed")).unwrap();
        store.create_task(&task_create("cancelled")).unwrap();

        store.mark_failed("failed", "权限不足").unwrap();
        store.mark_cancelled("cancelled", "用户取消").unwrap();

        let failed = store.get_task("failed").unwrap().unwrap();
        assert_eq!(failed.status, PersistentTaskStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("权限不足"));
        assert!(failed.finished_at_ms.is_some());

        let cancelled = store.get_task("cancelled").unwrap().unwrap();
        assert_eq!(cancelled.status, PersistentTaskStatus::Cancelled);
        assert_eq!(cancelled.error.as_deref(), Some("用户取消"));
        assert!(cancelled.finished_at_ms.is_some());
    }

    #[test]
    fn task_store_marks_queued_and_running_as_interrupted() {
        let store = SqliteTaskStore::open_in_memory().unwrap();
        store.create_task(&task_create("queued")).unwrap();
        store.create_task(&task_create("running")).unwrap();
        store.create_task(&task_create("completed")).unwrap();
        store.mark_running("running").unwrap();
        store
            .mark_completed("completed", &summary("completed-run"))
            .unwrap();

        let updated = store
            .mark_unfinished_interrupted("应用重启，上一进程任务未完成")
            .unwrap();

        assert_eq!(updated, 2);
        assert_eq!(
            store.get_task("queued").unwrap().unwrap().status,
            PersistentTaskStatus::Interrupted
        );
        assert_eq!(
            store.get_task("running").unwrap().unwrap().status,
            PersistentTaskStatus::Interrupted
        );
        assert_eq!(
            store.get_task("completed").unwrap().unwrap().status,
            PersistentTaskStatus::Completed
        );
        assert!(store
            .get_task("running")
            .unwrap()
            .unwrap()
            .finished_at_ms
            .is_some());
    }

    #[test]
    fn task_store_does_not_require_sensitive_params() {
        let store = SqliteTaskStore::open_in_memory().unwrap();
        store.create_task(&task_create("task-1")).unwrap();

        let conn = store.connection().unwrap();
        let stored_json: String = conn
            .query_row(
                "SELECT params_summary_json FROM task_runs WHERE task_id = ?1",
                params!["task-1"],
                |row| row.get(0),
            )
            .unwrap();

        assert!(stored_json.contains("image_aes_key_sha256"));
        assert!(!stored_json.contains("plain-secret-aes-key"));
        assert!(!stored_json.contains("image_aes_key\":\""));
    }

    #[test]
    fn list_tasks_orders_recent_first_and_applies_limit() {
        let store = SqliteTaskStore::open_in_memory().unwrap();
        store.create_task(&task_create("old")).unwrap();
        store.create_task(&task_create("newest")).unwrap();
        store.create_task(&task_create("middle")).unwrap();
        set_created_at_ms(&store, "old", 1_000);
        set_created_at_ms(&store, "newest", 3_000);
        set_created_at_ms(&store, "middle", 2_000);

        let records = store
            .list_tasks(&TaskListQuery::new().with_limit(2))
            .unwrap();

        assert_eq!(
            records
                .iter()
                .map(|record| record.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["newest", "middle"]
        );
    }

    #[test]
    fn list_tasks_filters_by_status_and_task_kind() {
        let store = SqliteTaskStore::open_in_memory().unwrap();
        store
            .create_task(&task_create_with_kind("image-ok", "extract_images"))
            .unwrap();
        store
            .create_task(&task_create_with_kind("video-ok", "extract_videos"))
            .unwrap();
        store
            .create_task(&task_create_with_kind("image-failed", "extract_images"))
            .unwrap();
        store
            .mark_completed("image-ok", &summary("image-ok"))
            .unwrap();
        store
            .mark_completed("video-ok", &summary("video-ok"))
            .unwrap();
        store.mark_failed("image-failed", "失败").unwrap();
        set_created_at_ms(&store, "image-ok", 1_000);
        set_created_at_ms(&store, "video-ok", 2_000);
        set_created_at_ms(&store, "image-failed", 3_000);

        let records = store
            .list_tasks(
                &TaskListQuery::new()
                    .with_status(PersistentTaskStatus::Completed)
                    .with_task_kind("extract_images"),
            )
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task_id, "image-ok");
    }

    #[test]
    fn list_tasks_filters_by_multiple_statuses_and_time_range() {
        let store = SqliteTaskStore::open_in_memory().unwrap();
        store.create_task(&task_create("queued")).unwrap();
        store.create_task(&task_create("completed")).unwrap();
        store.create_task(&task_create("failed")).unwrap();
        store
            .mark_completed("completed", &summary("completed"))
            .unwrap();
        store.mark_failed("failed", "失败").unwrap();
        set_created_at_ms(&store, "queued", 1_000);
        set_created_at_ms(&store, "completed", 2_000);
        set_created_at_ms(&store, "failed", 3_000);

        let records = store
            .list_tasks(
                &TaskListQuery::new()
                    .with_statuses([
                        PersistentTaskStatus::Completed,
                        PersistentTaskStatus::Failed,
                    ])
                    .with_created_at_from_ms(1_500)
                    .with_created_at_to_ms(2_500),
            )
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task_id, "completed");
    }

    #[test]
    fn list_tasks_does_not_write_source_or_archive_paths() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("app.sqlite");
        let source_dir = temp.path().join("wechat-source");
        let archive_dir = temp.path().join("archive");
        let store = SqliteTaskStore::open(&db_path).unwrap();
        store
            .create_task(&task_create_with_paths(
                "task-1",
                source_dir.clone(),
                archive_dir.clone(),
            ))
            .unwrap();
        assert!(!source_dir.exists());
        assert!(!archive_dir.exists());

        let records = store.list_tasks(&TaskListQuery::new()).unwrap();

        assert_eq!(records.len(), 1);
        assert!(!source_dir.exists());
        assert!(!archive_dir.exists());
    }
}
