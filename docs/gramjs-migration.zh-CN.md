# 导入 GramJS 会话

简体中文 | [English](gramjs-migration.md)

本文只适用于已经有 GramJS StringSession、希望继续使用原 Telegram 登录状态的用户。全新安装
请运行 `scripts/server/login.sh`，不需要准备 Session JSON。

迁移前停止 `telebot.service`，并保留原始 JSON。文件需要包含字符串字段 `session`，目标会话文件
必须不存在：

```sh
sudo ./target/release/telebot import-gramjs-session \
  --from /path/to/gramjs-session.json \
  --to /var/lib/telebot/telegram.session
sudo chown -R telebot:telebot /var/lib/telebot
sudo chmod 0700 /var/lib/telebot
sudo chmod 0600 /var/lib/telebot/telegram.session
```

导入命令只读取 JSON 中的 GramJS StringSession，不会修改源文件，也不会覆盖已有目标
会话。部署完成后运行 `sudo scripts/server/check-telegram.sh session`；确认检查通过并且服务恢复
就绪后，再决定如何保留或清理旧配置。

原始 JSON 和导入后的 session 都能直接登录 Telegram，请按密钥文件保管，不要提交到仓库或
附在公开 Issue 中。
