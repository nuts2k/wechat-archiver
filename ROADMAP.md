# 微信媒体归档器路线图

更新日期：2026-06-08

本文记录 `wechat-archiver` 已实现能力、当前边界和后续规划。公开文档不记录任何本机账号 ID、目录、密钥或媒体样本。

## 项目定位

`wechat-archiver` 不是聊天记录导出器，而是面向长期保存和治理的微信媒体归档器。

核心目标：

- 只读扫描 macOS 微信本地数据。
- 将图片、视频、语音、文件等媒体复制到独立归档库。
- 使用 `sha256`、SQLite 索引和 manifest 建立可校验、可追溯、可长期维护的归档。
- 后续通过去重、分类、检索、备份和桌面客户端降低微信本地数据占用。

工程路线：

```text
Rust core library
  -> CLI
  -> future Tauri desktop client
```

原则是 CLI 和未来 Tauri 入口都保持薄层，扫描、解码、归档、索引、校验等核心能力沉淀在 Rust core crate。

## 已实现

### 工程基础

- 已建立 Rust workspace。
- 已拆分 `crates/core` 和 `crates/cli`。
- 核心能力放在可复用 Rust core crate，CLI 只负责参数解析和输出。
- 已配置并使用本地参考项目目录 `references/external/`，该目录只读参考并被 `.gitignore` 忽略。
- `.codegraph/`、本地 key 文档、本地样本目录均已加入 `.gitignore`。

### 安全边界

- 微信源目录只读，不删除、不覆盖、不修改源文件。
- 归档目录不能等于源目录，不能位于源目录内部，也不能包含源目录。
- dry-run 不创建归档目录，不写索引，不写 manifest。
- 归档写入使用 staging，再移动到内容寻址对象路径。
- 归档对象以 `sha256` 校验为准。
- 消息数据库只按 SQLite 只读模式打开，并启用 `query_only`。
- 语音消息库 BLOB 只从已解密/普通 SQLite `media_*.db/VoiceInfo` 只读读取，归档写入仍只发生在 archive。
- 不读取微信进程内存。
- 不重签微信。
- 不提权。
- 不自动解密 SQLCipher 微信数据库。
- 支持用户显式指定已解密消息库目录，但媒体文件仍从微信账号目录读取。

### 账号发现

- 支持发现 macOS 微信 4.x 常见账号目录。
- 支持默认路径发现和显式 `--root`。
- 支持识别 `xwechat_files/<account>` 账号目录。
- 支持识别 `db_storage`、`msg/attach` 和推荐扫描源目录。
- 已处理部分 macOS 文件系统遍历中的 `Interrupted system call`，发现流程不会因单个 walkdir 错误整体失败。

### 图片扫描与归档

- 支持 `scan` dry-run。
- 支持统一媒体抽取入口 `extract --type image`，当前复用图片归档流程。
- 支持 `extract-images` 从指定源目录扫描图片。
- 支持 `extract-db-images` 从已解密/普通 SQLite 消息库枚举图片消息并定位本地 `.dat`。
- 支持普通图片扩展名：`jpg`、`jpeg`、`png`、`gif`、`bmp`、`webp`、`tif`、`tiff`、`heic`、`heif`。
- 支持旧 XOR `.dat` 图片自动识别 XOR key。
- 支持 V1 AES `.dat` 固定 key 解码。
- 支持 `derive-image-key` 只读派生 macOS 微信 4.x 图片 `.dat` AES/XOR key。
- `derive-image-key` 支持从 `kvcomm` 文件名提取 uin 候选，并用 V2 `.dat` 模板验证 AES key。
- `derive-image-key` 支持在 `kvcomm` 不可用时基于账号目录 wxid 后缀搜索候选。
- 支持 V2 AES `.dat` 在用户显式传入 `--image-aes-key` 时解码。
- 支持 V1/V2 尾段 XOR key 参数 `--image-xor-key`。
- 支持 `scan --explain-unsupported` 输出无法归档原因计数和样例路径。
- `scan --explain-unsupported` 会输出 reason、中文提示和样例路径。
- 支持 `wxgf` 私有图片格式：
- `--wxgf-mode jpg`：默认模式，提取内部 HEVC 分片并调用 `ffmpeg` 转首帧 JPG。
- `--wxgf-mode raw`：归档解密后的原始 `wxgf`。
- `--wxgf-mode mp4`：调用 `ffmpeg` 将内部 HEVC 分片封装为 MP4。
- `--wxgf-mode off`：关闭 `wxgf` 处理，保留旧行为。
- 支持 `--wxgf-ffmpeg-path` 指定 ffmpeg 路径。
- `wxgf jpg/mp4` dry-run 只验证 HEVC 分片和 `ffmpeg -version`，不执行全量转码。
- 对 `ffmpeg` 缺失、探测失败、启动失败、写入失败、进程失败、输出为空、输出格式异常等情况输出细分 reason。

### 视频扫描与归档

- 支持统一媒体抽取入口 `extract --type video`。
- 当前视频归档为直接文件扫描最小版，支持 `mp4`、`mov`、`m4v`。
- 当 `--source` 是微信账号目录或该账号的 `msg/attach` 时，自动扫描同账号 `msg/video`；其他目录只扫描传入目录本身。
- 视频对象复用内容寻址归档、SQLite 索引、manifest、dry-run 和 hash 校验流程。
- 当前记录 `source_kind=direct_video`、`media_type=video`，并对 MP4/MOV/M4V best-effort 提取时长和分辨率。

### 文件扫描与归档

- 支持统一媒体抽取入口 `extract --type file`。
- 当前文件归档为直接文件扫描最小版，归档带扩展名的普通文件。
- 当 `--source` 是微信账号目录或该账号的 `msg/attach` 时，自动扫描同账号 `msg/file`；其他目录只扫描传入目录本身。
- 文件对象复用内容寻址归档、SQLite 索引、manifest、dry-run 和 hash 校验流程。
- 当前记录 `source_kind=direct_file`、`media_type=file`；需要消息库来源上下文时使用 `extract-db-files`。

### 语音扫描与归档

- 支持统一媒体抽取入口 `extract --type voice`。
- 当前语音归档为直接音频文件扫描最小版，支持 `silk`、`slk`、`amr`、`mp3`、`m4a`、`aac`、`wav`、`ogg`、`opus`。
- 当 `--source` 是微信账号目录或该账号的 `msg/attach` 时，只扫描同账号存在的 `msg/voice` 或 `msg/audio` 专用目录；其他目录只扫描传入目录本身。
- 语音对象复用内容寻址归档、SQLite 索引、manifest、dry-run 和 hash 校验流程。
- 对 `wav`、`mp3`、`m4a`、`aac` 会 best-effort 提取时长；SILK/AMR/OGG/OPUS 当前不估算时长。
- 当前直接文件扫描记录 `source_kind=direct_voice`、`media_type=voice`；消息库语音 BLOB 归档见 `extract-db-voices`。

### 消息库图片枚举

- 支持 `inspect-db` 只读诊断消息库目录，报告普通 SQLite、缺失、非 SQLite 或疑似加密库。
- 支持 `--message-db-dir` 为 `extract-db-images`、`extract-db-videos`、`extract-db-files` 和 `extract-db-voices` 指定已解密/普通 SQLite 消息库目录。
- 支持读取 `db_storage/message/message_*.db`。
- 支持读取 `message_resource.db`。
- 支持基于 `MessageResourceInfo` 和 `Msg_<md5(talker)>` 枚举图片类消息。
- 支持定位 `msg/attach/<md5(talker)>/<YYYY-MM>/Img/<md5>.dat`。
- 支持 `_h`、`_W`、`_w`、`_t` 等图片变体候选。
- 对本地 `.dat` 缺失的资源记录为 `failed`。
- 对不可读取的加密数据库返回明确错误，不尝试绕过或解密。

### 消息库视频枚举

- 支持 `extract-db-videos` 从已解密/普通 SQLite 消息库枚举视频消息并定位本地视频。
- 支持基于 `MessageResourceInfo` / `MessageResourceDetail` 提取资源 md5。
- 支持定位 `msg/video/<YYYY-MM>/<md5>.mp4`，并复用内容寻址归档、SQLite 索引、manifest、dry-run 和 hash 校验流程。
- 当前记录 `source_kind=message_db_video`、`media_type=video` 和可用的 `message_talker`、`message_sender`、`message_local_id`、`message_create_time`。
- 对本地视频缺失的资源记录为 `failed`；对可读 MP4/MOV/M4V best-effort 提取时长和分辨率。

### 消息库文件附件枚举

- 支持 `extract-db-files` 从已解密/普通 SQLite 消息库枚举文件附件并定位本地文件。
- 支持从 `MessageResourceInfo` / `MessageResourceDetail` 的 `packed_info` 中保守识别带扩展名的安全文件名。
- 支持定位 `msg/file/<YYYY-MM>/<file_name>`，并复用内容寻址归档、SQLite 索引、manifest、dry-run 和 hash 校验流程。
- 当前记录 `source_kind=message_db_file`、`media_type=file` 和可用的 `message_talker`、`message_sender`、`message_local_id`、`message_create_time`。
- 对本地文件缺失的资源记录为 `failed`；当前不解析复杂 appmsg XML。

### 消息库语音枚举

- 支持 `extract-db-voices` 从已解密/普通 SQLite 消息库枚举语音消息并归档 `VoiceInfo.voice_data` 原始字节。
- 支持读取同一消息库目录下的 `media_*.db`。
- 支持基于 `Name2Id`、`VoiceInfo` 和 `Msg_<md5(talker)>` 的 `local_type=34` 按 `talker/local_id/create_time` 匹配语音 BLOB。
- 语音 BLOB 复用内容寻址归档、SQLite 索引、manifest、dry-run 和 hash 校验流程。
- 当前记录 `source_kind=message_db_voice`、`media_type=voice` 和可用的 `message_talker`、`message_sender`、`message_local_id`、`message_create_time`。
- 对可识别 `wav`、`mp3`、`aac` 语音 BLOB 会 best-effort 提取时长。
- 对消息表存在但 `VoiceInfo.voice_data` 缺失的语音记录为 `failed`；当前不做 SILK 转码或语音转写。

### 归档与索引

- 归档目录采用内容寻址结构：

```text
wechat-archive/
  objects/
    sha256/
      ab/
        abcd1234...jpg
  index.sqlite
  manifests/
  staging/
  logs/
  views/
```

- `objects` 按内容 hash 存储真实文件。
- `index.sqlite` 保存当前索引状态。
- `manifests/*.jsonl` 保存每次运行审计记录。
- `index.sqlite` 使用 `schema_migrations` 记录已应用的 schema 版本，当前会显式迁移基础表、`decoder` 字段、消息来源字段、文件元数据字段、媒体元数据字段、源文件指纹字段和 `.dat` 解码参数指纹字段。
- `index.sqlite` 和 manifest 独立记录 `source_kind` 与 `decoder`，例如 `source_kind=dat_image`、`decoder=legacy_xor`。
- `index.sqlite` 和 manifest 支持 `original_filename` 与 `mime_type`；MIME 当前基于归档扩展名保守推断，不做内容嗅探。
- `index.sqlite` 和 manifest 支持 `width_px`、`height_px`、`duration_ms`；当前直接图片和解码后的图片会 best-effort 写入宽高，视频会 best-effort 写入宽高和时长，部分音频会 best-effort 写入时长。
- `index.sqlite` 和 manifest 支持 `source_size_bytes`、`source_modified_ms` 与 `decode_fingerprint`；直接媒体复跑时可用源文件指纹复用已校验记录，`.dat` 图片可在源文件指纹、解码参数指纹和已校验对象都匹配时跳过重解码。
- `index.sqlite` 和 manifest 支持可空消息来源字段：`message_talker`、`message_sender`、`message_local_id`、`message_create_time`；当前消息库图片、视频、文件附件和语音归档会写入 `talker/local_id/create_time`，并在 `Msg_*` 表存在 `real_sender_id` 且同库 `Name2Id` 可映射时写入稳定 sender `user_name`。
- 抽取 summary 会输出 `reused_records`、`decoded_dat`、`metadata_backfilled`、`new_objects` 和 `existing_objects`，区分旧索引复用、实际 `.dat` 解码、元数据补写、新对象写入和内容去重命中。
- 支持 `status` 查看索引统计，并按 `media_type`、`source_kind`、`decrypt_status`、`verify_status` 分组。
- 支持 `lookup` 只读按 `sha256` 反查所有来源，或按 `source_path` 查询单个源文件归档状态。
- 支持 `report` 只读导出 JSON/CSV 索引报告。
- 支持 `views` 基于索引生成 `by-type`、`by-year`、`by-chat` 相对软链接视图，默认 dry-run。
- 支持 `verify` 重新计算归档对象 hash，并检查索引引用完整性。
- 支持重复对象去重：相同 `sha256` 不重复写入对象文件。

### 验证状态

- 单元测试覆盖普通图片格式识别、图片宽高解析、MP4 视频时长/宽高解析、旧 XOR `.dat`、V1/V2 AES `.dat`、`wxgf` 分区解析、`wxgf raw`、`wxgf jpg` 转换链路、`wxgf` dry-run validate-only、manifest/index 的 `decoder`、原始文件名、MIME、图片/视频元数据和消息来源记录、任务级抽取统计、消息库诊断、消息库图片/视频/文件附件/语音归档、外部已解密消息库目录、直接视频、文件和语音归档，以及统一 `extract --type` CLI 解析。
- 已通过：

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

- 本机只读 dry-run 验证显示，提供正确图片 key 和 ffmpeg 后，两个真实账号的图片 `.dat` 候选均可归档，`unsupported=0`。

## 当前边界

- 当前主线仍是图片归档。
- `extract-db-images`、`extract-db-videos`、`extract-db-files` 和 `extract-db-voices` 只支持已解密/普通 SQLite 数据库，不支持直接读取 SQLCipher 加密库。
- `inspect-db` 只能诊断当前消息库是否可读，不会自动解密；`count-db-media` 可统计已解密/普通 SQLite 消息库，但不会统计加密库里的真实媒体数量。
- `--message-db-dir` 只改变消息库读取位置；图片、视频和文件附件仍从 `--account` 下的微信目录定位，语音 BLOB 从该消息库目录的 `media_*.db/VoiceInfo` 只读读取。
- 消息来源字段当前由消息库图片、视频、文件附件和语音归档写入 `talker/local_id/create_time`；`message_sender` 可从消息表 `real_sender_id` 和 `Name2Id` best-effort 写入稳定 `user_name`，但还不解析联系人昵称或群名片。
- V2 图片 AES key 可通过 `derive-image-key` 只读派生，但抽取命令仍需要用户显式提供。
- `derive-image-key` 不自动保存 key；本机 key 文档应继续保存在 `.gitignore` 覆盖的本地文件中。
- `wxgf jpg/mp4` 依赖 `ffmpeg`，没有可用 `ffmpeg` 时应使用 `raw` 或 `off`。
- `wxgf jpg` 当前输出首帧 JPG，不保留可能存在的动态效果。
- 统一 `extract` 入口当前接入图片、直接视频文件、直接文件附件和直接语音/音频文件；表情、收藏、朋友圈等尚未接入归档流程。
- 视频归档当前已覆盖直接文件扫描和消息库枚举，并对 MP4/MOV/M4V best-effort 提取时长、宽度和高度。
- 文件归档当前已覆盖直接文件扫描和消息库枚举，但暂不解析复杂 appmsg XML，也不补充发送人显示名。
- 语音归档当前覆盖直接音频文件扫描和消息库 `VoiceInfo.voice_data` 原始 BLOB，并对 WAV/MP3/M4A/AAC 或可识别 BLOB best-effort 提取时长；暂不做 SILK 转码、语音转写或发送人显示名解析。
- 尚未实现 Tauri 桌面客户端。
- 尚未实现归档后的清理建议或删除操作。

## 近期路线

### P0：完善图片归档闭环

目标：让图片归档在日常使用中稳定、可解释、可复跑。

已完成：

- `wxgf jpg/mp4` dry-run 只验证分区和 `ffmpeg` 可用性，避免全量转码耗时过长。
- manifest/index 独立记录 decoder：`legacy_xor`、`v1_aes`、`v2_aes`、`wxgf_jpg`、`wxgf_raw`、`wxgf_mp4`。
- 为 `ffmpeg` 缺失、探测失败、执行失败、输出为空、输出格式异常提供更清晰的 reason 和用户提示。
- 增加 synthetic 测试样本，覆盖 `wxgf` dry-run validate-only 和 manifest/index `decoder` 记录。

剩余：

- 继续用真实样本回归图片 dry-run 和 `extract-db-images` 的 `unsupported=0` 场景。

验收标准：

- 对已有真实样本，图片 dry-run 能稳定达到 `unsupported=0` 或给出明确不可归档原因。
- `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets --all-features -- -D warnings` 全部通过。

### P1：统一媒体抽取命令

目标：从“图片归档器”扩展为“媒体归档器”。

已完成：

- 增加统一命令第一步：

```bash
wechat-archiver extract --type image
wechat-archiver extract --type video
wechat-archiver extract --type file
wechat-archiver extract --type voice
```

- `image` 入口复用现有图片归档流程。
- `inspect-db` 能在抽取前诊断消息库路径是否可读，避免只看到 `file is not a database`。
- `count-db-media` 能在已解密/普通 SQLite 消息库上只读统计 image/video/file/voice 候选数量，不创建归档目录，不读取媒体文件。
- `video` 入口当前扫描直接视频文件；对微信账号目录或 `msg/attach` 会自动定位同账号 `msg/video`，复制本地视频、计算 hash，并记录到同一套 archive/index/manifest。
- `extract-db-videos` 当前从消息库枚举视频资源，定位 `msg/video/<YYYY-MM>/<md5>.mp4`，并记录消息来源字段。
- `file` 入口当前扫描直接文件附件；对微信账号目录或 `msg/attach` 会自动定位同账号 `msg/file`，复制本地文件、计算 hash，并记录到同一套 archive/index/manifest。
- `extract-db-files` 当前从消息库枚举文件附件，定位 `msg/file/<YYYY-MM>/<file_name>`，并记录消息来源字段。
- `extract-db-voices` 当前从消息库枚举 `local_type=34` 语音消息，读取 `media_*.db/VoiceInfo.voice_data` 原始 BLOB，并记录消息来源字段。
- `voice` 入口当前扫描直接语音/音频文件；对微信账号目录或 `msg/attach` 只在存在 `msg/voice` 或 `msg/audio` 专用目录时扫描，避免把 `msg/file` 中的音乐附件误归为语音消息。
- `extract --type image,video,file,voice` 支持多类型顺序执行，并输出聚合 summary 和各类型子 summary。
- 聚合 summary 会汇总每个子任务的新对象、已有对象、旧索引复用、实际 `.dat` 解码和元数据补写计数。
- core 已提供任务级 `TaskEvent`、`TaskProgress`、`TaskReporter`、`CancelToken`、`TaskRunner` 和 `TaskHandle`；直接扫描和消息库抽取均可发出结构化进度事件，CLI 抽取类命令可通过 `--jsonl-progress` 输出 JSONL 事件到 stderr。后台任务队列雏形支持单 worker 顺序执行、任务 ID、排队/运行/完成/失败/取消状态、进度快照、事件消费和取消句柄。
- core 已提供 `TaskStore` trait 和 `SqliteTaskStore` 最小实现；`TaskRunner` 可通过显式 store 接入，在独立 app SQLite 中记录 `task_runs`、更新进度快照、写入完成/失败/取消终态，并在启动恢复时把遗留 `queued/running` 标记为 `interrupted`。任务历史支持按状态、任务类型、时间范围和 limit 只读查询，并可生成安全 retry 候选；CLI 已提供 `tasks list/show/retry-candidate --app-db <path>` 只读命令和显式 `tasks retry --app-db <path> <task_id>` 最小执行命令。
- 已补充任务队列持久化与恢复设计文档：`docs/task-persistence-and-recovery.md`。

计划：

- 视频归档增强：继续提升更多容器和异常样本的时长、分辨率解析覆盖率。
- 文件归档增强：解析更完整的 appmsg 元数据，补充大小、发送人显示名等上下文。
- 语音归档增强：在已支持原始 BLOB、部分音频时长和稳定 sender ID 的基础上，补充发送人显示名，再支持可选转换为 `wav` 或 `mp3`。
- 表情归档：识别静态图、动图和专有格式。
- 支持 `--since`、`--until`、`--chat`、`--type` 等过滤参数。
- 继续完善长任务控制：扩展 retry 到更多任务类型、多 worker 配置、暂停/恢复和更细粒度进度节流，为未来 Tauri 做准备。

验收标准：

- 图片、视频、文件、语音可以进入同一套 archive/index/manifest。
- dry-run 可准确估算候选数量、可归档数量和失败原因。
- 非 dry-run 不写微信源目录，只写独立 archive。
- 多类型聚合 summary 可以保留每个子任务的 run_id、manifest 和 index 路径，便于未来 Tauri 展示。

### P2：索引增强与可浏览视图

目标：让归档数据可查、可核对、可迁移。

已完成：

- 引入索引 schema 版本和迁移机制。
- 支持按 hash 反查来源。
- 支持按源路径查归档状态。
- 增强 `status`，输出媒体类型、来源类型、解密状态和校验状态分组统计。
- 增强 `verify`，覆盖归档对象完整性和索引引用完整性。
- 支持 CSV/JSON 索引报告导出。
- 支持 `views/` 生成可浏览派生视图，默认 dry-run，显式 `--write` 才写入。
- 支持 `original_filename` 和 `mime_type` 字段，并在 `lookup`、JSON/CSV `report`、SQLite 索引和 manifest 中输出。
- 支持 `width_px`、`height_px`、`duration_ms` 字段，并在 `lookup`、JSON/CSV `report`、SQLite 索引和 manifest 中输出；当前图片宽高、视频宽高/时长和部分音频时长 best-effort 解析。
- 支持 `source_size_bytes`、`source_modified_ms` 和 `decode_fingerprint` 字段，并在 `lookup`、JSON/CSV `report`、SQLite 索引和 manifest 中输出；直接媒体可基于未变源文件指纹复用已校验记录，`.dat` 图片可基于未变源文件指纹和解码参数指纹复用已校验记录。
- 支持直接媒体的最小增量复跑：同一源路径、同一媒体类型、同一源大小和修改时间、且既有记录 `verify_status=ok` 时跳过重新 hash/复制。

计划：

- 继续丰富 `media_items` 字段：会话 ID、发送人显示名、更多音频格式时长和更多视频容器元数据。
- 继续扩展增量扫描状态：任务级扫描游标和跨运行状态缓存。

验收标准：

- 任一归档对象都能反查到来源路径和归档运行记录。
- 索引升级不破坏已有 archive。
- `verify` 能覆盖对象完整性和索引引用完整性。

### P3：去重、分类和治理

目标：把归档库变成可治理的媒体资产库。

计划：

- 精确去重：基于 `sha256`。
- 图片近似去重：基于 `pHash` 或 `dHash`。
- 视频近似去重：基于关键帧 hash。
- 同名文件冲突处理：结合 hash、大小、来源、时间聚类。
- 文件分类：基于扩展名、文件名、来源会话、时间。
- 生成重复文件报告和节省空间估算。
- 增加低风险的本地 AI 标签能力，默认只读取缩略图或摘要。

验收标准：

- 能区分完全重复、近似重复和不同文件。
- 去重建议不自动删除源文件。
- AI 功能默认本地优先，并可关闭。

### P4：备份与清理建议

目标：安全释放 macOS 微信目录空间，但不让工具直接承担高风险删除。

计划：

- 生成可清理候选报告。
- 清理前强制二次 hash 校验。
- 检查归档库是否存在外部备份。
- 输出待清理列表和风险说明。
- 支持 dry-run 清理计划。
- 支持保留回滚 manifest。
- 优先建议通过微信自带空间管理清理。

验收标准：

- 清理建议必须可追溯到归档对象、来源路径和校验记录。
- 没有通过 hash 校验和外部备份检查时，不生成“可安全清理”结论。
- 第一版不做自动删除微信源文件。

### P5：Tauri 桌面客户端

目标：提供更适合长期使用的本地 GUI，但不复制业务逻辑。

计划：

- 新增 `apps/desktop`。
- 复用 `crates/core`。
- 提供账号发现、归档任务、进度展示、错误报告、索引浏览。
- 提供归档库状态面板和校验入口。
- 提供本地配置管理：归档目录、ffmpeg 路径、key 派生结果、任务过滤器。

验收标准：

- CLI 和 Tauri 使用同一套 core API。
- GUI 不直接操作微信源目录。
- GUI 可以完整复现 CLI 的 dry-run、extract、status、verify 能力。

## 非目标

- 不做微信客户端替代品。
- 不做聊天记录完整导出器。
- 不绕过系统权限。
- 不读取运行中微信进程内存。
- 不自动重签微信。
- 不默认上传媒体或索引到云端。
- 不在没有明确校验和用户确认的情况下清理微信源文件。

## 推荐下一步

建议继续推进 P2 索引增强：

- 优先继续丰富 `media_items` 字段，例如发送人显示名和更完整的会话上下文。
- 然后补增量扫描状态，减少重复遍历。
- 语音后续增强重点是发送人显示名和可选转码/转写，而不是继续扩大直接文件扫描范围。
- 所有路线都应继续保持 dry-run、结构化错误、manifest/index 和安全边界一致。
