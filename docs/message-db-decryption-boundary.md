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
wechat-archiver extract-db-videos \
  --account "/path/to/xwechat_files/<wxid>" \
  --message-db-dir "/path/to/decrypted/message" \
  --archive "/path/to/wechat-archive" \
  --dry-run \
  --json
```

`--message-db-dir` 只改变消息数据库读取位置。图片、视频和文件附件仍从 `--account/msg` 下定位和读取，保证不会从外部副本目录误读媒体。

## 后续可做

- 增强 `inspect-db` 的汇总字段，例如各类消息候选计数。
- 增加只读 `count-db-media` 命令，在已解密消息库上统计 image/video/file/voice 候选数量。
- 如果未来实现数据库解密，也必须作为单独显式命令，默认 dry-run，并要求用户明确提供密钥或已授权的解密材料；不得隐式读取进程内存或修改微信应用。
