# 微信媒体归档器

`wechat-archiver` 是一个 Rust-first 的微信媒体归档器。它的目标不是导出聊天记录，而是把本机微信目录里的媒体资源复制到独立归档库，并用 `sha256`、SQLite 索引和 manifest 形成可校验、可追溯、可长期维护的归档。

## 当前 MVP

当前实现的是安全优先的 Rust CLI MVP：

- 只读发现 macOS 微信 4.x 常见账号目录：`xwechat_files/<wxid>/db_storage` 和 `msg/attach`。
- 只读递归扫描用户指定的源目录。
- 只读读取已解密/普通 SQLite 消息库：`db_storage/message/message_*.db` 和 `message_resource.db`。
- 基于 `MessageResourceInfo` 和 `Msg_<md5(talker)>` 枚举图片消息，定位 `msg/attach/<md5(talker)>/<YYYY-MM>/Img/<md5>.dat`。
- 归档普通图片：`jpg`、`jpeg`、`png`、`gif`、`bmp`、`webp`、`tif`、`tiff`、`heic`、`heif`。
- 参考 `jackwener/wx-cli` 和 `ylytdeng/wechat-decrypt` 的旧格式图片 `.dat` 思路，支持自动识别 XOR key 并解码旧 XOR `.dat` 图片。
- 支持 V1 AES `.dat` 的固定 key 解码。
- 支持 V2 AES `.dat` 在用户显式提供 `--image-aes-key` 时解码；不会自动读取微信进程内存或提取密钥。
- 支持解密后的微信 `wxgf` 私有图片格式：默认调用 `ffmpeg` 提取 HEVC 首帧并转成 JPG；也可选择归档原始 `wxgf` 或封装为 MP4。
- 对未知 `.dat`、缺少 V2 key 或无法识别文件记录为 `unsupported`，不会写出不可信的垃圾文件；对消息库中存在但本地 `.dat` 缺失的资源记录为 `failed`。
- 归档文件写入独立 archive 目录，使用内容寻址路径 `objects/sha256/<prefix>/<sha256>.<ext>`。
- 每次非 dry-run 运行写入 `index.sqlite` 和 `manifests/*.jsonl`。
- 支持 `status` 查看索引统计，支持 `verify` 重新计算归档对象 hash。

当前 MVP 不会解密微信加密数据库，也不会提取微信进程密钥、重签微信、修改微信或写入微信源目录。`extract-db-images` 只支持已经可被 SQLite 直接读取的消息库，例如测试 fixture、用户自行准备的已解密副本，或本机上已经是普通 SQLite 的目录。

## 安全边界

强制约束：

- 源目录只读，不删除、不覆盖、不修改任何源文件。
- 微信消息数据库只以 SQLite 只读模式打开，并启用 `query_only`。
- 归档目录不能等于源目录，不能位于源目录内部，也不能包含源目录。
- dry-run 不创建归档目录，不写索引，不写 manifest。
- 归档成功必须经过 `sha256` 校验。
- V2 `.dat` 解码 key 必须由用户显式传入；工具不读取微信进程内存、不提权、不重签微信。
- SQLite 索引只保留每个源文件的最新状态，manifest 保留每次运行历史。
- 不上传任何媒体、聊天内容、索引或日志。

## 使用

构建：

```bash
cargo build
```

只读发现本机微信 4.x 账号目录：

```bash
cargo run -p wechat-archiver -- discover --json
```

也可以显式指定 `xwechat_files` 根目录或单个账号目录：

```bash
cargo run -p wechat-archiver -- discover \
  --root "$HOME/Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files"
```

只读扫描，不写入归档目录：

```bash
cargo run -p wechat-archiver -- scan \
  --source "/path/to/wechat/source" \
  --archive "/path/to/wechat-archive"
```

归档图片：

```bash
cargo run -p wechat-archiver -- extract-images \
  --source "/path/to/wechat/source" \
  --archive "/path/to/wechat-archive"
```

按消息库枚举并归档图片：

```bash
cargo run -p wechat-archiver -- extract-db-images \
  --account "/path/to/xwechat_files/<wxid>" \
  --archive "/path/to/wechat-archive"
```

该命令会读取 `<account>/db_storage/message/message_*.db` 和 `message_resource.db`，并只从 `<account>/msg/attach` 复制或解码图片资源。它不会解密 SQLCipher 数据库，也不会读取微信进程内存；如果数据库不是普通 SQLite，会返回错误。

如果需要解码 V2 AES `.dat`，必须显式提供图片 AES key：

```bash
cargo run -p wechat-archiver -- extract-images \
  --source "/path/to/wechat/source" \
  --archive "/path/to/wechat-archive" \
  --image-aes-key "0123456789abcdef"
```

`extract-db-images` 同样支持 `--image-aes-key` 和 `--image-xor-key`。

`--image-aes-key` 支持普通 16+ 字节字符串，也支持 `hex:<hex-encoded-key>`。`--image-xor-key` 默认是 `0x88`，通常不需要修改。

解密后如果遇到 `wxgf`，默认 `--wxgf-mode jpg` 会调用 `ffmpeg` 从内部 HEVC 分片转换首帧 JPG。若 `ffmpeg` 不在 `PATH`，可显式传入路径：

```bash
cargo run -p wechat-archiver -- extract-images \
  --source "/path/to/wechat/source" \
  --archive "/path/to/wechat-archive" \
  --image-aes-key "0123456789abcdef" \
  --wxgf-mode jpg \
  --wxgf-ffmpeg-path "/opt/homebrew/bin/ffmpeg"
```

`--wxgf-mode` 支持：

- `jpg`：默认值，转出首帧 JPG，适合图片归档和预览。
- `raw`：不转码，归档解密后的原始 `wxgf`。
- `mp4`：调用 `ffmpeg` 将内部 HEVC 分片封装为 MP4。
- `off`：关闭 `wxgf` 处理，保留旧行为并记录为 `unsupported`。

查看统计：

```bash
cargo run -p wechat-archiver -- status \
  --archive "/path/to/wechat-archive"
```

校验归档对象：

```bash
cargo run -p wechat-archiver -- verify \
  --archive "/path/to/wechat-archive"
```

所有命令支持 `--json` 输出结构化结果。

## 归档目录

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

`objects` 是真实内容存储，`index.sqlite` 是当前索引，`manifests` 是每次运行的审计记录。

## 外部项目参考

本项目已在 `references/external/` 本地浅克隆几个外部项目，只作只读参考，不提交、不修改：

- `jackwener/wx-cli`：Rust CLI 分层、附件解析、旧 XOR `.dat` 解码、资源定位思路。
- `ylytdeng/wechat-decrypt`：`.dat` 解码、V1/V2 格式判断、macOS 微信 4.x 解密研究参考。
- `r266-tech/wechat-cli`：strict read-only、安全边界、JSON 输出和媒体命令设计。
- `huohuoer/wechat-cli`：本地优先、结构化输出、隐私说明参考。

不要直接复制外部项目代码；需要实现相同能力时，应确认许可证并在本项目中独立实现。

## 开发验证

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

更多规划见 [wechat-media-archive-plan.md](wechat-media-archive-plan.md)，代理开发约束见 [AGENTS.md](AGENTS.md)。
