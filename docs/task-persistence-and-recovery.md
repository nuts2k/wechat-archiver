# 任务队列持久化与恢复设计

本文记录 `wechat-archiver` 对后台任务队列持久化、任务历史和重启恢复的设计边界。

## 结论

- 当前 `TaskRunner` 是进程内单 worker 队列，只保证当前进程生命周期内的排队、运行、完成、失败、取消、进度快照和事件消费。
- 重启后不恢复正在运行的任务，也不自动继续未完成的扫描或抽取。
- 后续持久化优先保存任务历史和可审计状态，不保存原始媒体、聊天内容、明文密钥或完整消息文本。
- 任务恢复的默认语义应是“标记上次未完成任务为 interrupted，并允许用户重新发起同参数任务”，不是静默续跑。
- 任务历史应与 archive/index/manifest 解耦；archive 负责媒体对象和索引，任务历史负责 UI 和操作审计。

## 当前内存态能力

当前 core 已提供：

- `TaskRunner`：进程内后台任务队列，当前为单 worker 顺序执行。
- `TaskHandle`：查询任务 ID、任务名、状态、进度快照，取消任务，消费事件，等待终态。
- `TaskStatus`：`queued`、`running`、`completed`、`failed`、`cancelled`。
- `TaskSnapshot`：当前状态、最后事件、进度、错误和完成结果。
- `TaskEvent` / `TaskProgress`：扫描、候选、单项完成、完成、取消等结构化事件。
- `CancelToken`：任务协作式取消信号。

这些能力足够支持 Tauri 客户端在单次进程运行中展示长任务进度和取消按钮，但不承担跨进程恢复。

## 持久化目标

后续持久化层应满足：

- 记录用户发起过哪些任务。
- 记录任务从 queued 到 running 到终态的状态变化。
- 记录最后一次进度快照，便于 UI 重启后展示最近状态。
- 记录任务参数摘要，便于用户确认并重新发起。
- 记录错误信息，便于排查权限、路径、数据库、解码、hash 或索引写入问题。
- 不改变现有 archive 的媒体对象、索引和 manifest 安全边界。

非目标：

- 不在重启后自动继续复制、解码或写索引。
- 不把任务历史当作归档真实性依据；真实性仍以对象 hash、`index.sqlite` 和 manifest 为准。
- 不为任务历史存储原始媒体、聊天内容、联系人昵称、消息文本或密钥明文。
- 不把 UI 任务状态写进微信源目录。

## 建议存储位置

优先选择独立 app 配置库，而不是写入每个 archive：

```text
~/Library/Application Support/wechat-archiver/
  app.sqlite
```

原因：

- 一个用户可能管理多个 archive，任务历史天然是应用级视图。
- Tauri 需要展示跨 archive 的最近任务和失败历史。
- archive 可以保持长期归档库定位，不混入桌面客户端运行状态。

可选补充：

- 每个 archive 可继续保留 `manifests/*.jsonl` 作为归档审计事实。
- app 配置库只保存任务控制平面，不替代 manifest。
- CLI 默认不创建 app 配置库；只有桌面客户端或显式任务历史命令需要写入。

## 建议 schema

第一版可用普通 SQLite：

```sql
CREATE TABLE task_runs (
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

CREATE INDEX idx_task_runs_created_at
  ON task_runs(created_at_ms);

CREATE INDEX idx_task_runs_status
  ON task_runs(status);
```

可选事件表：

```sql
CREATE TABLE task_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL,
  event_index INTEGER NOT NULL,
  event_json TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  FOREIGN KEY(task_id) REFERENCES task_runs(task_id)
);

CREATE UNIQUE INDEX idx_task_events_task_index
  ON task_events(task_id, event_index);
```

事件表第一版可以不开启，只保存最后事件和进度快照；如果 UI 需要完整时间线，再补事件表。

## 参数摘要边界

`params_summary_json` 应只保存重新发起任务所需的非敏感摘要：

允许保存：

- `task_kind`，例如 `extract_images`、`extract_videos`、`extract_db_voices`。
- `source_dir`、`archive_dir`、`message_db_dir`。
- `dry_run`。
- 媒体类型列表。
- `wxgf_mode`。
- 是否提供了 AES key。
- AES key 的 sha256 摘要，不保存明文 key。
- XOR key 数值。
- ffmpeg 路径。

禁止保存：

- 图片 AES key 明文。
- 微信数据库解密密钥。
- 原始媒体字节。
- 语音 BLOB。
- 消息文本、聊天内容、联系人昵称。
- cookie、token、私钥。

路径是个人信息。桌面客户端展示路径时应明确这是本地应用状态；如果未来支持导出诊断包，应默认脱敏路径。

## 状态转换

建议状态流：

```text
queued -> running -> completed
queued -> running -> failed
queued -> running -> cancelled
queued -> cancelled
running -> interrupted
```

说明：

- `interrupted` 仅用于应用重启后发现上一进程遗留的 `queued` 或 `running` 任务。
- 启动时不自动恢复这些任务，应批量标记为 `interrupted`。
- UI 可提供“重新发起”按钮，用保存的参数摘要创建新 `task_id`。
- 取消是协作式取消，只有任务检查到 `CancelToken` 后才进入 `cancelled`。

当前内存态 `TaskStatus` 还没有 `interrupted`。后续持久化实现时可在持久化层增加该状态，或扩展 core 枚举。

## 启动恢复流程

桌面客户端启动时建议：

1. 打开 app 配置库并执行 schema 迁移。
2. 查询状态为 `queued` 或 `running` 的历史任务。
3. 将这些任务标记为 `interrupted`，写入 `finished_at_ms` 和错误说明。
4. 展示最近任务列表。
5. 对 `interrupted` 任务提供重新发起入口，但不自动执行。

CLI 默认不执行上述恢复流程。当前已有显式命令可读取或写入指定 app SQLite，但都不会自动创建 app 配置库：

```bash
wechat-archiver tasks list --app-db <path>
wechat-archiver tasks show --app-db <path> <task_id>
wechat-archiver tasks retry-candidate --app-db <path> <task_id>
wechat-archiver tasks retry --app-db <path> <task_id>
```

其中 `list`、`show` 和 `retry-candidate` 只读打开 app SQLite；`retry` 是显式写命令，会基于安全候选创建新的任务记录并执行，不会修改原任务记录，也不会自动恢复明文 key。

## 与 archive manifest 的关系

任务历史不是归档事实来源。

- manifest 记录每次归档运行中每个媒体项的审计事件。
- `index.sqlite` 记录当前可查询索引状态。
- task history 记录应用任务状态和 UI 操作历史。

如果任务完成并写入 archive，应通过 `ExtractSummary.manifest_path` 关联 manifest。即使 task history 丢失，archive 仍可通过 manifest/index/verify 自证。

## 错误和隐私策略

错误记录应保留足够上下文，但避免泄露敏感内容：

- 可以记录错误类别、路径、SQLite 错误摘要、权限错误和 hash mismatch。
- 不记录 SQL 查询返回的消息正文。
- 不记录 `.dat` 解密 key 明文。
- 不记录语音 BLOB 或媒体内容。
- 错误字符串在导出诊断包前应支持路径脱敏。

## 实施顺序

当前持久化、runner 接入、只读历史查询、retry 候选准备、显式只读 CLI 和最小显式 retry CLI 已实现，后续继续扩展 retry 范围：

1. 已增加 `TaskStore` trait 和 `SqliteTaskStore` SQLite 实现，只记录 `task_runs` 快照，不保存完整事件流。
2. 已让 `TaskRunner` 可选接入 store，在状态变化和事件到来时更新快照。
3. 已提供 `TaskListQuery` 和 `TaskStore::list_tasks` 只读查询。
4. 已提供 `TaskStore::retry_candidate` 和 `TaskRetryCandidate`，只从非敏感参数摘要准备可重试候选，不自动执行任务。
5. 已提供 `wechat-archiver tasks list/show/retry-candidate --app-db <path>` 只读 CLI，支持人类可读输出和 `--json`。
6. 已提供 `wechat-archiver tasks retry --app-db <path> <task_id>` 显式 CLI，会写入新的任务历史记录并通过 `TaskRunner::with_store` 执行。

当前能力：

- 新任务创建时写入 `queued`。
- 通过 `TaskRunner::with_store` 和 `spawn_with_metadata` 可显式启用任务历史写入。
- worker 开始时更新为 `running`。
- 任务事件到来时更新 `progress_json` 和 `last_event_json`。
- 完成、失败、取消时写入终态和结果摘要。
- 启动恢复可把遗留 `queued/running` 标记为 `interrupted`。
- 可按状态、任务类型、`created_at_ms` 时间范围和 limit 查询最近任务历史，默认按 `created_at_ms DESC` 返回。
- 可为已完成、失败、取消或中断任务生成 retry 候选；`queued/running`、缺少必要目录、缺少任务类型、参数摘要非对象、包含疑似明文 key/token/cookie/secret 或图片任务历史参数显示曾提供 AES key 的任务会标记为不可重试。
- CLI `tasks` 子命令必须显式传入已有 `--app-db`，缺失路径不会创建数据库；`list/show/retry-candidate` 只读打开，`retry` 读写打开并只写任务历史和归档目录。
- CLI `tasks retry` 必须显式传入已有 `--app-db`，当前只支持 `extract_images`、`extract_videos`、`extract_files` 和 `extract_voices`；图片任务若历史参数显示曾提供 AES key，会被候选规则拒绝，要求用户手动重新运行并显式提供 key。
- 单元测试覆盖 schema 初始化、幂等迁移、终态更新、runner/store 集成、只读历史查询、retry 候选安全边界、CLI 只读任务命令、CLI 显式 retry 和敏感参数不落盘。

CLI 默认仍不会创建 app 配置库。任务历史写入需要调用方显式打开 `SqliteTaskStore`，并创建带 store 的 `TaskRunner`。

## 当前保持不实现的内容

- 不实现自动续跑。
- 不实现后台 daemon。
- 不实现跨进程实时事件订阅。
- 不实现自动执行 retry、消息库任务 retry 或敏感参数恢复。
- 不实现任务历史导出。
- 不实现任务清理策略。

这些能力应在任务历史 schema 稳定后逐步补充。
