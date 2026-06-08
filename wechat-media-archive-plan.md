# 微信媒体归档器规划

## 结论

目标不是传统的“聊天记录导出器”，而是一个面向长期存储和管理的“微信媒体归档器”。

核心问题是：微信聊天历史记录体量很大，手机聊天记录会定期同步到 macOS，但长期保存在 macOS 微信目录里会持续占用空间。更合理的方案是定期从 macOS 微信本地数据中提取图片、视频、音频、文件等媒体资源，复制到独立归档库，并建立索引。未来再通过 AI 做重复文件处理、同名文件合并、分类、筛选和备份管理。

技术路线建议从第一天采用 Rust。第一版仍然先做 CLI，不急着做 UI，但工程结构要按未来 Tauri 客户端来拆分：归档、索引、解密、校验等核心能力沉淀在 Rust core crate，CLI 和未来 Tauri 只是不同入口。这样可以避免先用 Python POC 跑通后再重写一遍核心逻辑。

第一版必须坚持只读微信目录、复制归档、hash 校验、不删除原件。删除或清理应作为后续阶段，并优先通过微信自带空间管理完成。

## 现有项目评估

| 项目 | 适合度 | 说明 |
| --- | --- | --- |
| [jackwener/wx-cli](https://github.com/jackwener/wx-cli) | 最高 | 约 3.1k stars。已有 daemon 缓存、增量消息查询、`attachments` / `extract` 图片附件提取设计，最接近“定期归档”的目标。缺点是当前附件提取主要覆盖图片。 |
| [ylytdeng/wechat-decrypt](https://github.com/ylytdeng/wechat-decrypt) | 底层能力最强 | 约 3.8k stars。支持 macOS 微信 4.x 数据库解密、批量导出、图片 `.dat` 解密、语音转录等，适合作为解密和解析参考。 |
| [r266-tech/wechat-cli](https://github.com/r266-tech/wechat-cli) | 值得参考 | 星数较低，但 README 明确有 `media` 命令，目标是读取本机微信 4.x 的消息、媒体、收藏、朋友圈等数据。 |
| [huohuoer/wechat-cli](https://github.com/huohuoer/wechat-cli) | 次要参考 | 约 1.2k stars。偏聊天查询、搜索、统计、文本导出，不是媒体归档主线。 |

不建议优先依赖 `chatlog`、`PyWxDump` 等已删库或合规风险较高的项目。它们虽然曾经星数较高，但不适合作为新项目底座。

## 推荐定位

建议做一个独立项目，可以命名为 `wechat-archiver`。

它只做三件事：

1. 扫描微信本地数据。
2. 把媒体复制到独立归档库。
3. 建立可长期维护的索引。

工程形态建议：

```text
Rust core library
  -> CLI command
  -> future Tauri desktop client
```

核心原则是 UI 不持有业务逻辑。CLI 和未来 Tauri 都只负责任务触发、参数输入、进度展示和结果呈现，真正的扫描、解析、归档、索引、校验都在同一套 Rust 核心库里完成。

核心流程：

```text
macOS 微信本地数据
  -> 读取已解密/普通 SQLite 消息库
  -> 枚举图片、视频、语音、文件消息
  -> 定位原始媒体或解密 .dat
  -> 计算 sha256 / 感知 hash / 元数据
  -> 复制到内容寻址归档目录
  -> 写入 SQLite 索引
  -> 后续 AI 做去重、分类、筛选、备份策略
```

## 归档目录设计

建议使用内容寻址存储，避免同一文件重复占用空间。

```text
wechat-archive/
  objects/
    sha256/
      ab/
        abcd1234...jpg
        abcd5678...mp4
  index.sqlite
  manifests/
    2026-05-31-scan.jsonl
  views/
    by-chat/
    by-year/
    by-type/
  staging/
  logs/
```

说明：

- `objects` 只按内容 hash 存一份真实文件。
- `index.sqlite` 保存消息、来源、文件、hash、归档路径、原始文件名、MIME 类型、源文件指纹等结构化信息，并通过 `schema_migrations` 记录显式 schema 版本迁移；索引 API 支持只读按 `sha256` 或 `source_path` 反查归档记录，并支持 `status` 分组统计和 `verify` 索引引用完整性检查。
- `manifests` 保存每次扫描和归档结果，便于审计和回滚。
- `views` 可以通过软链接或索引生成“按联系人、群、年份、类型”的视图。
- `staging` 用于临时解密、转换和校验，成功后再移动到正式归档区。
- `logs` 保存运行日志和错误信息。

## 索引字段建议

`media_items` 表可以包含：

| 字段 | 说明 |
| --- | --- |
| `id` | 内部 ID |
| `message_id` | 微信消息 ID |
| `chat_id` | 会话 ID |
| `chat_name` | 会话名称 |
| `sender_id` | 发送人 ID |
| `sender_name` | 发送人名称 |
| `message_time` | 消息时间 |
| `source_kind` | 来源类型，例如直接图片、目录 `.dat`、消息库图片 |
| `media_type` | 图片、视频、语音、文件、表情等 |
| `original_path` | 微信本地原始路径 |
| `archive_path` | 归档后路径 |
| `sha256` | 内容 hash |
| `size_bytes` | 文件大小 |
| `source_size_bytes` | 源文件大小，用于增量扫描判断 |
| `source_modified_ms` | 源文件修改时间，用于增量扫描判断 |
| `mime_type` | 文件类型 |
| `original_filename` | 原始文件名 |
| `width_px` | 图片或视频宽度，当前图片和视频 best-effort 写入 |
| `height_px` | 图片或视频高度，当前图片和视频 best-effort 写入 |
| `duration_ms` | 视频或音频时长，当前视频和部分音频 best-effort 写入 |
| `extension` | 扩展名 |
| `decoder` | 解码路径，例如 `legacy_xor`、`v1_aes`、`v2_aes`、`wxgf_jpg` |
| `decrypt_status` | 解密状态 |
| `verify_status` | 校验状态 |
| `created_at` | 索引创建时间 |
| `updated_at` | 索引更新时间 |

后续可以增加：

- 视频关键帧 hash。
- 音频转写文本。
- 文件摘要。
- AI 标签。
- 相似文件分组 ID。

## 分阶段路线

### 第 1 阶段：Rust CLI 只读扫描和图片归档 MVP

目标是先安全跑通最常见、最占空间的一类媒体。

当前已实现：

- 建立 Rust workspace、核心库和 CLI 入口。
- 将扫描、归档、索引、校验逻辑放在可被 Tauri 复用的核心库中。
- 只读发现 macOS 微信 4.x 常见账号目录。
- 只读递归扫描普通图片和 `.dat` 图片源目录。
- 只读读取已解密/普通 SQLite 消息库：`message_*.db`、`message_resource.db` 和语音使用的 `media_*.db/VoiceInfo`。
- 只读诊断消息库目录是否为当前可读的普通 SQLite，并支持显式指定已解密消息库目录。
- 只读统计已解密/普通 SQLite 消息库中的 image/video/file/voice 候选数量，不读取媒体文件，不写归档目录。
- 基于 `ChatName2Id`、`MessageResourceInfo`、`Msg_<md5(talker)>` 枚举图片类消息。
- 定位 `msg/attach/<md5(talker)>/<YYYY-MM>/Img/<md5>.dat`，并兼容 `_h`、`_W`、`_w`、`_t` 变体。
- 基于消息库视频资源 md5 定位 `msg/video/<YYYY-MM>/<md5>.mp4`。
- 基于消息库文件附件资源保守识别安全文件名，并定位 `msg/file/<YYYY-MM>/<file_name>`。
- 基于消息库 `VoiceInfo.voice_data` 和 `Msg_<md5(talker)>` 的 `local_type=34` 归档语音原始 BLOB。
- 支持普通图片、旧 XOR `.dat`、V1 AES `.dat`，V2 AES `.dat` 仅在用户显式提供 key 时解码。
- 计算 `sha256`。
- 复制到归档目录。
- 写入 SQLite 索引，并保持 schema 迁移可追踪、可幂等复跑。
- 索引和 manifest 会记录 `original_filename`、`mime_type`、`width_px`、`height_px`、`duration_ms`、`source_size_bytes`、`source_modified_ms`、`decode_fingerprint`；MIME 当前基于归档扩展名保守推断，不做内容嗅探，图片宽高、视频宽高/时长和部分音频时长当前 best-effort 写入。直接媒体复跑时可基于未变源文件指纹复用已校验记录，`.dat` 图片可基于未变源文件指纹、解码参数指纹和已校验对象复用旧记录；`decode_fingerprint` 只保存参数哈希，不保存原始 key。
- 抽取 summary 会输出旧索引复用、实际 `.dat` 解码、元数据补写、新对象写入和已有对象命中计数，便于判断每次运行的真实工作量。
- core 提供任务级 `TaskEvent`、`TaskProgress`、`TaskReporter`、`CancelToken`、`TaskRunner` 和 `TaskHandle`；CLI 抽取类命令可用 `--jsonl-progress` 输出 JSONL 进度事件，未来 Tauri 可直接复用同一套结构化事件、后台任务状态和取消信号。
- 任务队列持久化、只读任务历史 CLI 和重启恢复边界见 `docs/task-persistence-and-recovery.md`；当前设计不自动续跑重启前的 running 任务，也不自动执行 retry。
- 消息库图片、视频、文件附件和语音归档会在索引和 manifest 中记录可用的 `message_talker`、`message_sender`、`message_local_id`、`message_create_time`；`message_sender` 当前仅在 `Msg_*` 表存在 `real_sender_id` 且同库 `Name2Id` 可映射时写入稳定 `user_name`。
- 支持 `status` 查看索引总量、唯一对象、唯一字节数，并按媒体类型、来源类型、解密状态和校验状态分组。
- 支持 `report` 只读导出 JSON/CSV 索引报告，供人工审计、备份流程和后续 AI 分类使用。
- 支持 `views` 基于索引生成 `archive/views/` 下的可浏览相对软链接视图；`views/` 是可再生成的派生层，不是客户端。
- 支持 `verify` 重新计算归档对象 hash，并检查 `verify_status=ok` 索引记录的 `archive_path`、`sha256`、对象存在性和路径/hash 一致性。
- 对未知 `.dat` 记录 `unsupported`，对消息库中存在但本地 `.dat` 缺失的资源记录 `failed`。

当前边界：

- 不自动解密 SQLCipher 微信数据库。
- 不提取微信进程密钥、不重签微信、不提升权限。
- 不写入、不删除、不覆盖任何微信源目录文件。
- 当前覆盖图片、直接视频文件、直接文件附件、直接语音/音频文件，以及消息库语音原始 BLOB；语音转码、转写和发送人显示名补充仍在第 2 阶段增强。

建议命令：

```bash
wechat-archiver scan
wechat-archiver extract --type image
wechat-archiver extract --type video
wechat-archiver extract --type file
wechat-archiver extract --type voice
wechat-archiver inspect-db
wechat-archiver count-db-media
wechat-archiver extract-images
wechat-archiver extract-db-images
wechat-archiver extract-db-videos
wechat-archiver extract-db-files
wechat-archiver extract-db-voices
wechat-archiver status
wechat-archiver lookup
wechat-archiver report
wechat-archiver views
wechat-archiver verify
```

说明：`extract --type image` 复用图片归档流程。`extract --type video`、`extract --type file` 和 `extract --type voice` 当前扫描直接媒体文件；当 source 是账号目录或 `msg/attach` 时，会分别自动扫描同账号 `msg/video`、`msg/file`，以及存在时的 `msg/voice` 或 `msg/audio`。`inspect-db` 用于抽取前只读诊断消息库是否可读。`count-db-media` 用于在已解密/普通 SQLite 消息库上估算 image/video/file/voice 候选量，不读取微信媒体目录、不写归档目录。`extract-db-images`、`extract-db-videos`、`extract-db-files` 和 `extract-db-voices` 从已解密/普通 SQLite 消息库枚举对应资源并记录消息来源字段；如果已解密消息库不在账号目录内，可通过 `--message-db-dir` 指定。图片、视频和文件仍从 `--account/msg` 定位，语音 BLOB 从 `--message-db-dir` 指向的 `media_*.db/VoiceInfo` 只读读取。`lookup` 只读打开索引，支持按 `sha256` 反查来源或按 `source_path` 查归档状态，并输出原始文件名、MIME 类型、图片/视频宽高、部分媒体时长、源文件指纹和 `.dat` 解码参数指纹。`status` 输出归档总量和按 `media_type`、`source_kind`、`decrypt_status`、`verify_status` 分组的索引健康统计。`report` 只读导出全量索引记录，支持包含原始文件名、MIME 类型、图片/视频宽高、部分媒体时长、源文件指纹和 `.dat` 解码参数指纹的 JSON/CSV。`views` 默认 dry-run 预览 `views/` 相对软链接计划，传 `--write` 才写入。`verify` 覆盖对象 hash 校验和索引引用完整性检查，异常时返回非零退出码。`extract-images` 保留用于兼容旧脚本。

注意事项：

- 只复制，不删除微信原文件。
- 不自动解密 SQLCipher 数据库；加密库需要先通过独立流程得到用户明确提供的已解密副本。
- 对无法解析的消息保留错误记录。
- 每次运行生成 manifest。
- 归档成功必须以 hash 校验为准。

### 第 2 阶段：扩展视频、文件和语音

目标是覆盖主要占空间资源。

功能：

- 视频归档：已支持直接文件和消息库枚举，并对 MP4/MOV/M4V best-effort 提取时长和分辨率；后续继续提高更多容器和异常样本覆盖率。
- 文件归档：已支持直接文件和保守消息库枚举，并记录原始文件名、MIME 类型和可用的稳定 sender ID；后续再补更完整的 appmsg 元数据、大小和发送人显示名。
- 语音归档：已支持直接语音/音频文件和消息库 `VoiceInfo.voice_data` 原始 BLOB，并对 WAV/MP3/M4A/AAC 或可识别 BLOB best-effort 提取时长；后续再支持可选转换为 `wav` 或 `mp3`、更多格式时长和转写。
- 支持按时间范围、会话、类型过滤。

建议命令：

```bash
wechat-archiver extract --type image,video,file,voice
wechat-archiver extract --chat "某个群名"
wechat-archiver extract --since 2025-01-01
```

当前 `extract --type image,video,file,voice` 已支持按类型顺序执行并输出聚合 summary；总计字段是各子任务相加，不代表跨类型去重后的唯一文件数。聚合 summary 会同时汇总 `reused_records`、`decoded_dat`、`metadata_backfilled`、`new_objects` 和 `existing_objects`。抽取类命令也可输出结构化 JSONL 进度事件到 stderr，默认 summary 输出保持不变。

### 第 3 阶段：去重和 AI 管理

目标是把媒体资产变成可治理的数据集。

功能：

- 精确去重：基于 `sha256`。
- 图片近似去重：基于 `pHash` 或 `dHash`。
- 视频近似去重：基于关键帧 hash。
- 同名冲突处理：结合 hash、大小、来源聊天、时间聚类。
- 文件分类：基于文件名、扩展名、来源、AI 标签。
- 生成清理建议和备份建议。

AI 层原则：

- AI 默认只读索引、缩略图和摘要。
- 不默认上传原文件。
- 对隐私敏感文件必须支持本地模型或手动确认。

### 第 4 阶段：备份与清理

目标是安全释放 macOS 空间。

功能：

- 生成可清理报告。
- 对归档文件做二次校验。
- 检查外部备份是否完成。
- 输出待清理列表。
- 支持 dry-run。
- 支持保留回滚 manifest。

清理前置条件：

- 归档 hash 校验通过。
- 至少有一份独立备份。
- manifest 可追溯。
- index 可反查原始来源。
- 用户明确确认。

不建议第一版做自动删除。删除微信内部媒体文件可能导致消息预览、原图、视频、文件状态异常。

## 技术选型建议

### 总体路线：Rust-first

不要先做 Python POC 再迁移。第一版直接用 Rust 写 CLI，但把 CLI 做薄，把可复用能力放到核心库里。

```text
wechat-archiver/
  crates/
    core/       # 扫描、解密适配、媒体定位、hash、归档、索引、校验
    cli/        # 命令行入口
  apps/
    desktop/    # 后续 Tauri 客户端，第一阶段可不创建
```

建议模块边界：

- `config`：微信数据目录、归档目录、扫描范围、运行参数。
- `wechat`：微信数据库读取、版本适配、消息和媒体元数据解析。
- `media`：图片 `.dat` 解密、媒体路径定位、MIME 和扩展名识别。
- `archive`：内容寻址存储、文件复制、临时目录和原子移动。
- `index`：SQLite schema、增量扫描状态、来源反查。
- `verify`：sha256 校验、索引引用完整性检查、归档一致性检查。
- `manifest`：每次扫描和归档的 JSONL 审计记录。
- `task`：任务进度、错误收集、取消信号，方便未来接入 Tauri。

### Rust 技术栈

```text
Rust + SQLite + CLI first + Tauri-ready core
```

建议：

- CLI 使用 `clap` 组织命令。
- 序列化使用 `serde`，manifest 使用 JSONL。
- 日志使用 `tracing`，同时输出到终端和日志文件。
- SQLite 优先选择简单可靠的同步访问方式，先保证 schema、事务和校验逻辑稳定。
- 文件扫描、复制和 hash 计算可以先同步实现，后续再按性能瓶颈引入并发。
- 长任务要从一开始返回结构化进度事件，不要只依赖终端输出，方便未来 Tauri 展示进度条、错误列表和运行报告。

### 对现有项目的使用方式

`jackwener/wx-cli`、`ylytdeng/wechat-decrypt`、`r266-tech/wechat-cli` 仍然值得参考，但不建议作为第一版运行时依赖。

更合理的用法：

- 参考它们的数据库定位、解密流程、媒体路径规则和命令设计。
- 用小样本数据对照验证 Rust 实现结果。
- 必要时把 Python 或外部工具作为临时研究脚本，而不是产品链路的一部分。

### 为什么有利于未来 Tauri

- Tauri 后端本身就是 Rust，未来可以直接调用同一套 core crate。
- CLI 和桌面客户端共享归档、索引、校验逻辑，避免行为分叉。
- 不需要在桌面端额外打包 Python runtime。
- 大文件复制、hash、SQLite 写入等任务由 Rust 后台任务处理，更适合桌面客户端。
- 从第一阶段就设计任务进度和取消机制，后续 UI 接入成本更低。

AI 层：

```text
后置服务，不进入第一版核心链路
```

原因：

- 第一版核心风险在数据读取、解密、索引、归档和校验。
- AI 分类和筛选可以建立在稳定索引之上。
- 避免一开始把隐私、安全和准确性问题混在核心归档链路里。

## 风险和约束

主要风险：

- 微信版本变化导致数据库结构或文件路径变化。
- macOS 权限限制导致无法读取微信目录。
- Rust 直接适配数据库和解密流程，前期验证速度会慢于 Python 脚本。
- 如果核心库和 CLI 耦合过深，未来接 Tauri 时会产生重构成本。
- 图片 `.dat`、语音、视频、文件的存储方式不一致。
- 大体量文件扫描耗时长。
- 重复文件和同名文件处理容易误判。
- 直接删除微信内部文件存在破坏消息状态的风险。
- 聊天数据高度敏感，AI 处理和备份都需要隐私边界。

控制策略：

- 默认只读微信目录。
- 所有写入只发生在归档目录。
- 使用 hash 做完整性校验。
- 每次扫描保留 manifest。
- CLI 入口保持薄封装，核心逻辑保持可复用库 API。
- 长任务从一开始支持结构化进度和错误报告。
- 清理动作必须独立于归档动作。
- 清理前必须 dry-run。
- 不默认上传任何原始媒体到云端 AI。

## 推荐下一步

第一步先做 Rust CLI MVP，不急着做 UI 和 AI，但代码结构要为 Tauri 预留。

建议 MVP 范围：

1. 建立 Rust workspace：`core` crate + `cli` crate。
2. 定义配置文件、归档目录结构、SQLite schema 和 manifest 格式。
3. 在测试微信账号或小范围真实数据上读取 macOS 微信 4.x 数据。
4. 提取最近 30 天图片消息。
5. 解密或定位图片文件。
6. 复制到内容寻址归档目录。
7. 写入 SQLite 索引。
8. 生成一份归档报告。

MVP 成功标准：

- 能稳定找到图片媒体。
- 能正确复制到归档目录。
- 能通过 hash 校验。
- 能从索引反查来源聊天和时间。
- 重复图片只保存一份。
- 多次运行不会重复归档。
- 核心流程可以通过 Rust library API 调用，不依赖解析 CLI 文本输出。
- 任务进度、错误和结果是结构化数据，未来可直接映射到 Tauri UI。

推荐优先参考：

1. `jackwener/wx-cli` 的 daemon、缓存和附件提取思路。
2. `ylytdeng/wechat-decrypt` 的数据库解密、图片 `.dat` 解密、语音处理能力。
3. `r266-tech/wechat-cli` 的 `media` 命令设计。
