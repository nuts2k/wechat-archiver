# 消息库解密边界

本文记录 `wechat-archiver` 对 macOS 微信 4.x 消息库读取的当前边界和后续工作流。

## 结论

- macOS 微信 4.x 的 `db_storage/message/*.db` 通常是 SQLCipher/WCDB 加密库。
- 当前工具只读打开普通 SQLite；如果 `inspect-db` 输出 `status=encrypted_or_not_sqlite`，说明还不能枚举真实消息记录。
- 本项目不读取微信进程内存、不提权、不重签微信、不内置密钥提取流程。
- 用户如需消息库来源增强，应先通过独立可信流程准备已解密消息库副本，再使用 `--message-db-dir`。

## 推荐工作流

```bash
wechat-archiver inspect-db \
  --account "/path/to/xwechat_files/<wxid>" \
  --message-db-dir "/path/to/decrypted/message" \
  --json
```

确认 `status=ready` 后，再运行：

```bash
wechat-archiver count-db-media \
  --account "/path/to/xwechat_files/<wxid>" \
  --message-db-dir "/path/to/decrypted/message" \
  --json
```

`count-db-media` 不需要归档目录，不创建索引或 manifest，也不会读取、复制或 hash `--account/msg` 下的媒体文件。它只统计已解密/普通 SQLite 消息库里的候选数量；语音会只读统计 `media_*.db/VoiceInfo` 中非空 `voice_data` 的候选键，不归档 BLOB。

确认候选量后，再运行：

```bash
wechat-archiver extract-db-videos \
  --account "/path/to/xwechat_files/<wxid>" \
  --message-db-dir "/path/to/decrypted/message" \
  --archive "/path/to/wechat-archive" \
  --dry-run \
  --json
```

语音可运行：

```bash
wechat-archiver extract-db-voices \
  --account "/path/to/xwechat_files/<wxid>" \
  --message-db-dir "/path/to/decrypted/message" \
  --archive "/path/to/wechat-archive" \
  --dry-run \
  --json
```

`--message-db-dir` 只改变消息数据库读取位置。图片、视频和文件附件仍从 `--account/msg` 下定位和读取，保证不会从外部副本目录误读媒体。语音 BLOB 是消息库自身数据，`extract-db-voices` 会从该目录下 `media_*.db/VoiceInfo` 只读读取 `voice_data`，并只写入独立归档目录。

## 后续可做

- 增强 `inspect-db` 的汇总字段，必要时复用 `count-db-media` 的各类消息候选计数。
- 为 `extract-db-voices` 增加时长、发送人和可选 SILK 转码/转写；这些能力仍必须默认只读微信源目录。
- 如果未来实现数据库解密，也必须作为单独显式命令，默认 dry-run，并要求用户明确提供密钥或已授权的解密材料；不得隐式读取进程内存或修改微信应用。
