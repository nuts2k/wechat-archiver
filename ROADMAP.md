# 微信媒体归档器路线图

更新日期：2026-06-07

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
- 不读取微信进程内存。
- 不重签微信。
- 不提权。
- 不自动解密 SQLCipher 微信数据库。

### 账号发现

- 支持发现 macOS 微信 4.x 常见账号目录。
- 支持默认路径发现和显式 `--root`。
- 支持识别 `xwechat_files/<account>` 账号目录。
- 支持识别 `db_storage`、`msg/attach` 和推荐扫描源目录。
- 已处理部分 macOS 文件系统遍历中的 `Interrupted system call`，发现流程不会因单个 walkdir 错误整体失败。

### 图片扫描与归档

- 支持 `scan` dry-run。
- 支持 `extract-images` 从指定源目录扫描图片。
- 支持 `extract-db-images` 从已解密/普通 SQLite 消息库枚举图片消息并定位本地 `.dat`。
- 支持普通图片扩展名：`jpg`、`jpeg`、`png`、`gif`、`bmp`、`webp`、`tif`、`tiff`、`heic`、`heif`。
- 支持旧 XOR `.dat` 图片自动识别 XOR key。
- 支持 V1 AES `.dat` 固定 key 解码。
- 支持 V2 AES `.dat` 在用户显式传入 `--image-aes-key` 时解码。
- 支持 V1/V2 尾段 XOR key 参数 `--image-xor-key`。
- 支持 `wxgf` 私有图片格式：
- `--wxgf-mode jpg`：默认模式，提取内部 HEVC 分片并调用 `ffmpeg` 转首帧 JPG。
- `--wxgf-mode raw`：归档解密后的原始 `wxgf`。
- `--wxgf-mode mp4`：调用 `ffmpeg` 将内部 HEVC 分片封装为 MP4。
- `--wxgf-mode off`：关闭 `wxgf` 处理，保留旧行为。
- 支持 `--wxgf-ffmpeg-path` 指定 ffmpeg 路径。

### 消息库图片枚举

- 支持读取 `db_storage/message/message_*.db`。
- 支持读取 `message_resource.db`。
- 支持基于 `MessageResourceInfo` 和 `Msg_<md5(talker)>` 枚举图片类消息。
- 支持定位 `msg/attach/<md5(talker)>/<YYYY-MM>/Img/<md5>.dat`。
- 支持 `_h`、`_W`、`_w`、`_t` 等图片变体候选。
- 对本地 `.dat` 缺失的资源记录为 `failed`。
- 对不可读取的加密数据库返回明确错误，不尝试绕过或解密。

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
- 支持 `status` 查看索引统计。
- 支持 `verify` 重新计算归档对象 hash。
- 支持重复对象去重：相同 `sha256` 不重复写入对象文件。

### 验证状态

- 单元测试覆盖普通图片格式识别、旧 XOR `.dat`、V1/V2 AES `.dat`、`wxgf` 分区解析、`wxgf raw` 和 `wxgf jpg` 转换链路。
- 已通过：

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

- 本机只读 dry-run 验证显示，提供正确图片 key 和 ffmpeg 后，两个真实账号的图片 `.dat` 候选均可归档，`unsupported=0`。

## 当前边界

- 当前主线仍是图片归档。
- `extract-db-images` 只支持已解密/普通 SQLite 数据库，不支持直接读取 SQLCipher 加密库。
- V2 图片 AES key 需要用户显式提供。
- `wxgf jpg/mp4` 依赖 `ffmpeg`，没有可用 `ffmpeg` 时应使用 `raw` 或 `off`。
- `wxgf jpg` 当前输出首帧 JPG，不保留可能存在的动态效果。
- 视频、文件、语音、表情、收藏、朋友圈等尚未形成统一归档命令。
- 尚未实现 Tauri 桌面客户端。
- 尚未实现归档后的清理建议或删除操作。

## 近期路线

### P0：完善图片归档闭环

目标：让图片归档在日常使用中稳定、可解释、可复跑。

计划：

- 增加 `derive-image-key` 命令，移植已验证的非侵入式图片 key 派生逻辑到 Rust。
- 支持从 macOS `kvcomm` 文件名提取 uin 候选。
- 支持基于账号目录 wxid 和 V2 `.dat` 模板验证 AES key。
- 支持 fallback 的 wxid 后缀候选搜索。
- 增加 `scan --explain-unsupported` 或类似报告，输出各类失败原因计数和样例。
- 优化 dry-run 性能：`wxgf jpg` dry-run 可只验证分区和 ffmpeg 可用性，避免全量转码耗时过长。
- 在 manifest/index 中记录更细的 decoder：`legacy_xor`、`v1_aes`、`v2_aes`、`wxgf_jpg`、`wxgf_raw`、`wxgf_mp4`。
- 为 `ffmpeg` 缺失、执行失败、输出格式异常提供更清晰的用户提示。
- 增加小型 synthetic fixture，避免测试依赖真实微信样本。

验收标准：

- 不依赖 Python 参考脚本即可派生图片 key。
- 对已有真实样本，图片 dry-run 能稳定达到 `unsupported=0` 或给出明确不可归档原因。
- `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets --all-features -- -D warnings` 全部通过。

### P1：统一媒体抽取命令

目标：从“图片归档器”扩展为“媒体归档器”。

计划：

- 增加统一命令，例如：

```bash
wechat-archiver extract --type image,video,file,voice
```

- 视频归档：复制本地视频、计算 hash、记录时长和分辨率。
- 文件归档：保留原始文件名、扩展名、大小、来源消息。
- 语音归档：先保存原始格式，再支持可选转换为 `wav` 或 `mp3`。
- 表情归档：识别静态图、动图和专有格式。
- 支持 `--since`、`--until`、`--chat`、`--type` 等过滤参数。
- 支持任务级进度统计和可取消执行，为未来 Tauri 做准备。

验收标准：

- 图片、视频、文件、语音可以进入同一套 archive/index/manifest。
- dry-run 可准确估算候选数量、可归档数量和失败原因。
- 非 dry-run 不写微信源目录，只写独立 archive。

### P2：索引增强与可浏览视图

目标：让归档数据可查、可核对、可迁移。

计划：

- 引入索引 schema 版本和迁移机制。
- 丰富 `media_items` 字段：消息时间、会话 ID、发送人、媒体类型、原始文件名、MIME、宽高、时长。
- 生成 `views/` 视图：按年份、类型、会话、来源路径组织。
- 支持导出 CSV/JSON 报告。
- 支持按 hash 反查来源。
- 支持按源路径查归档状态。
- 支持增量扫描状态，减少重复遍历。

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

建议优先做 P0 中的 `derive-image-key`：

- 它能去掉当前手工保存 key 的步骤。
- 它符合只读边界。
- 它能显著降低后续 CLI 和 Tauri 的使用门槛。
- 已有本机验证和外部参考算法，技术风险较低。

推荐随后做 `scan --explain-unsupported` 和 dry-run 性能优化，让工具在面对大目录时更可解释、更快。
