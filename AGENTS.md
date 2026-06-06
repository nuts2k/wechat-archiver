# AGENTS.md

## 项目定位

`wechat-archiver` 是一个面向长期存储和管理的微信媒体归档器，不是聊天记录导出器。

核心目标：

- 只读扫描 macOS 微信本地数据。
- 将图片、视频、语音、文件等媒体复制到独立归档库。
- 使用内容 hash 建立可校验、可去重、可追溯的长期索引。
- 后续支持 AI 辅助分类、近似去重、备份和清理建议。

第一版必须坚持：

- 不删除微信目录中的任何文件。
- 不修改微信数据库或微信内部文件。
- 不默认上传任何原始媒体、聊天内容或索引到云端。
- 所有写入只发生在用户指定的归档目录。

## 技术路线

项目从第一天采用 Rust-first 路线。

第一阶段先做 CLI MVP，不急着做 UI，但架构必须为未来 Tauri 桌面客户端预留：

```text
Rust core library
  -> CLI command
  -> future Tauri desktop client
```

设计要求：

- CLI 保持薄封装，只负责参数解析、命令分发、终端输出和退出码。
- 扫描、解析、媒体定位、归档、索引、校验等业务逻辑必须放在可复用的 Rust core crate。
- 不要先实现一套 Python 产品链路再迁移到 Rust。
- 可以参考 Python 或其他开源项目验证数据库结构、解密流程和媒体路径，但不要把它们作为长期运行时依赖。
- 长任务从一开始输出结构化进度和结构化错误，方便未来 Tauri UI 复用。

## 建议工程结构

优先使用 Rust workspace：

```text
wechat-archiver/
  crates/
    core/
    cli/
  apps/
    desktop/
  docs/
  AGENTS.md
  wechat-media-archive-plan.md
```

说明：

- `crates/core`：归档核心库，包含业务逻辑和可测试 API。
- `crates/cli`：命令行入口，只调用 core。
- `apps/desktop`：未来 Tauri 客户端，第一阶段可以不存在。
- `docs`：补充设计文档、schema、数据样本说明和风险说明。

如果实际创建项目结构，应优先保持 crate 边界清晰，而不是按“先能跑”把所有逻辑塞进 CLI。

## 核心模块边界

建议在 core crate 中按以下职责拆分：

- `config`：微信数据目录、归档目录、扫描范围、运行参数。
- `wechat`：微信数据库读取、版本适配、消息和媒体元数据解析。
- `media`：图片 `.dat` 解密、媒体路径定位、MIME 和扩展名识别。
- `archive`：内容寻址存储、文件复制、临时目录、原子移动。
- `index`：SQLite schema、增量扫描状态、来源反查。
- `verify`：sha256 校验、归档一致性检查。
- `manifest`：每次扫描和归档的 JSONL 审计记录。
- `task`：任务进度、错误收集、取消信号。

模块之间通过明确的数据结构交互，避免直接共享全局状态。

## 数据和安全约束

这是处理高度敏感本地数据的工具。任何实现都必须优先满足安全边界。

强制约束：

- 默认只读微信目录。
- 不做自动清理、自动删除、自动覆盖微信内部文件。
- 归档文件写入必须先进入临时路径，校验成功后再移动到正式对象目录。
- 归档成功必须以 hash 校验为准。
- 每次扫描和归档必须保留 manifest，便于审计和回滚。
- 对无法解析、无法解密、无法复制的项目保留错误记录，不要静默跳过。
- AI 能力默认只能读取索引、缩略图或摘要；访问原始文件必须由用户明确确认。

清理功能如果未来实现，必须满足：

- 与归档动作完全分离。
- 默认 dry-run。
- 要求归档 hash 校验通过。
- 要求至少一份独立备份。
- 输出可审计的待清理列表。
- 需要用户明确确认后才能执行。

## 归档目录原则

归档库应采用内容寻址存储，避免同一文件重复占用空间。

推荐结构：

```text
wechat-archive/
  objects/
    sha256/
      ab/
        abcd1234...jpg
  index.sqlite
  manifests/
  views/
  staging/
  logs/
```

要求：

- `objects` 只按内容 hash 存一份真实文件。
- `index.sqlite` 保存消息来源、文件元数据、hash、归档路径和校验状态。
- `manifests` 保存每次运行结果。
- `staging` 只存临时文件，成功归档后应清理或标记。
- `views` 可以由索引生成，不应成为唯一真实数据来源。

## Rust 实现偏好

优先选择简单、可验证、可维护的实现。

建议：

- CLI 使用 `clap`。
- 日志和诊断使用 `tracing`。
- 序列化使用 `serde`。
- SQLite 访问先选择稳定同步方案，优先保证事务和一致性。
- hash 使用流式读取，避免一次性读入大文件。
- 文件复制和校验先实现正确性，再根据瓶颈引入并发。
- 错误类型要能区分权限、路径不存在、数据库解析失败、解密失败、hash 不一致、索引写入失败等情况。
- 对外 API 不要返回纯文本状态，应返回结构化结果。

避免：

- 在 CLI 中堆业务逻辑。
- 为了快速验证引入不可控的全局状态。
- 对微信目录做写操作。
- 把路径、数据库表名、微信版本假设硬编码到不可替换的位置。
- 让 Tauri UI 未来依赖解析 CLI stdout。

## 命令设计

第一阶段建议命令：

```bash
wechat-archiver scan
wechat-archiver extract-images
wechat-archiver status
wechat-archiver verify
```

第二阶段可扩展：

```bash
wechat-archiver extract --type image,video,file,voice
wechat-archiver extract --chat "某个群名"
wechat-archiver extract --since 2025-01-01
```

命令要求：

- 所有会写入归档目录的命令必须明确接收或解析归档目录。
- 默认不执行任何删除。
- 支持 dry-run 的命令应优先实现 dry-run。
- 输出应同时兼顾人类可读和机器可读，必要时提供 `--json`。

## 测试要求

新增核心逻辑时必须优先考虑可测试性。

建议测试范围：

- 内容寻址路径生成。
- sha256 计算和校验。
- manifest 写入格式。
- SQLite schema 初始化和幂等迁移。
- 重复文件只归档一份。
- 文件复制失败、hash 不一致、索引写入失败的错误路径。
- 使用小型 fixture 覆盖媒体路径解析和 `.dat` 解密逻辑。

不要把真实微信数据提交到仓库。测试 fixture 必须脱敏，并尽量使用人工构造的小样本。

## 文档要求

涉及以下变更时应同步更新文档：

- 归档目录结构变化。
- SQLite schema 变化。
- CLI 命令或参数变化。
- 微信版本适配策略变化。
- 安全边界、清理策略、AI 处理策略变化。

主规划文档是 `wechat-media-archive-plan.md`。如果实现与规划出现偏差，应先更新规划或在 PR/提交说明中解释原因。

## Git 和提交习惯

提交应小而清晰，优先使用 conventional commits 风格：

```text
docs: update archive plan
feat: add archive object store
fix: handle missing media path
test: cover sha256 verifier
refactor: split cli from core
```

提交前建议检查：

- `git status --short`
- `cargo fmt`
- `cargo test`
- 必要时运行 `cargo clippy`

不要提交：

- 真实微信数据。
- 私钥、token、cookie、个人路径配置。
- 大型二进制媒体样本。
- 构建产物和缓存目录。

## 参考优先级

优先参考本仓库文档和实现，其次参考外部项目。

外部项目已统一放在本地目录 `references/external/`，该目录已加入 `.gitignore`，只用于本地阅读、搜索和对照验证，不进入本仓库提交。

本地参考项目：

- `references/external/jackwener-wx-cli`：daemon、缓存、附件提取思路。
- `references/external/ylytdeng-wechat-decrypt`：macOS 微信 4.x 数据库解密、图片 `.dat` 解密、语音处理能力。
- `references/external/r266-tech-wechat-cli`：本机微信 4.x 的媒体命令设计。
- `references/external/huohuoer-wechat-cli`：聊天查询、搜索、统计、文本导出等辅助参考。

使用约束：

- 只读参考，不要修改这些外部项目源码。
- 不要在外部项目目录内运行格式化、批量重写、依赖升级或清理命令。
- 不要把外部项目代码复制进本项目，除非先确认许可证兼容性并保留必要来源说明。
- 需要对照行为时，优先在本项目中写 fixture、测试或小型验证程序。
- 如果确实需要改动外部项目，应在单独 fork 或独立目录中处理，不要在 `references/external/` 内直接修改。

参考外部项目时要注意许可证和合规风险。不要直接复制不兼容许可证代码。
