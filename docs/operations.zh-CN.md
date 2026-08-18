# telebot 运维说明

简体中文 | [English](operations.md)

本文档对应带 systemd 且 glibc 不低于 2.35 的 64 位 Linux Release 安装包；Docker 只用于
可选的语录渲染。除非命令中另有说明，以下操作都在解压后的 Release 目录执行，需要 root 权限
的步骤使用 `sudo`。

## 文件位置

| 内容 | 路径 |
| --- | --- |
| 已部署版本 | `/opt/telebot/releases/<release-id>/` |
| 当前版本链接 | `/opt/telebot/current` |
| 主配置 | `/etc/telebot/config.toml` |
| Telegram 与 AI 密钥 | `/etc/telebot/telebot.env` |
| Telegram 会话、SQLite 数据库和语录存档 | `/var/lib/telebot/` |
| 部署回滚记录 | `/var/backups/telebot/<release-id>/` |
| systemd 服务 | `telebot.service` |

不要把生产配置、环境变量文件、Telegram 会话、数据库、日志或编译产物提交到仓库。

## 首次部署

### 1. 准备账号和密钥

从 [Telegram 官方页面](https://core.telegram.org/api/obtaining_api_id) 申请自己的 `api_id` 和
`api_hash`。还需要准备：

- 一个 AI API Key；
- 对应的模型名、API 地址和 API 格式；
- 一个已经注册并可正常登录的 Telegram 账号。

telebot 可以通过手机号、验证码和 2FA 密码登录已有账号，但不能注册新账号，也暂不支持二维码
登录。

### 2. 下载并校验

按照[根 README](../README.zh-CN.md#安装-release)中的 CPU 判断、Release 下载和 SHA-256
校验命令操作，然后进入解压后的目录。安装包支持 x86_64 和 ARM64 Linux，服务器不需要安装
Rust 或 Cargo。

### 3. 创建运行账号和配置

```sh
sudo ./install.sh --prepare
sudoedit /etc/telebot/config.toml
sudoedit /etc/telebot/telebot.env
```

准备步骤会创建 `telebot` 运行账号、`/var/lib/telebot` 和两个配置文件，不会覆盖已有文件。
至少填写：

- `config.toml` 中的 `telegram.api_id`、`ai.api_format`、`ai.base_url` 和 `ai.model`；
- `telebot.env` 中的 `TELEBOT_TELEGRAM_API_HASH` 以及 `ai.api_key_env` 指向的 AI Key。

环境变量文件归 root 所有，权限为 `0600`。systemd 会在启动 telebot 前读取它，Key 不需要
写进 TOML。

### 4. 授权、部署和检查

```sh
sudo ./install.sh
```

安装器会检查配置，询问 Telegram 手机号和验证码，安装带版本号的 release，启用 systemd 服务
并等待日志出现 `telebot is ready`。账号启用了两步验证时还会询问 2FA 密码；验证码和密码均
不回显。Session 保存在 `/var/lib/telebot/telegram.session`，权限为 `0600`。

部署过程会：

1. 把程序和运维文件复制到 `/opt/telebot/releases/v<version>/`；
2. 读取环境变量并运行 `telebot validate`；
3. 原子切换 `/opt/telebot/current`；
4. 重启服务并等待 `telebot is ready`；
5. 检查失败时恢复先前版本。

已有 GramJS Session 的迁移方法单独放在[迁移说明](gramjs-migration.zh-CN.md)中，不影响新用户
登录。

### 5. 可选语录渲染

示例配置为 `quote.enabled = false`。如果安装时需要启用 `.q`，修改该设置后运行：

```sh
sudo ./install.sh --with-quote
```

这条路径需要 Docker、Docker Compose、Git 和 curl。脚本会检出固定的
`LyoSU/quote-api` 提交，构建本地镜像，启动只监听环回地址的服务，并检查
`http://127.0.0.1:3210/health`。

## 更新

按 README 下载并校验新版本，解压后运行：

```sh
cd "telebot-linux-$(uname -m)"
sudo ./install.sh
```

如果 ARM 系统的 `uname -m` 返回 `arm64`，解压目录仍是 `telebot-linux-aarch64`。每个版本
使用独立 release 目录；安装不会删除旧版本、配置、Telegram Session 或 SQLite 数据。

## 日常检查

查看服务、最近日志和 quote-api 状态：

```sh
sudo scripts/server/status.sh
```

以下检查会打开 Telegram MTProto 会话，不能与正在运行的 `telebot.service` 同时执行。这个脚本
会先停止服务，检查完成后再启动，并确认服务重新就绪：

```sh
sudo scripts/server/check-telegram.sh session
sudo scripts/server/check-telegram.sh format
sudo scripts/server/check-telegram.sh plugins
sudo scripts/server/check-telegram.sh image /path/to/test-image.png
```

- `session` 检查 Telegram 会话是否仍获授权。
- `format` 把测试消息发送到收藏夹，确认 Telegram 接受富文本实体，然后删除测试消息。
- `plugins` 实际执行 AI 回复、帮助、动态文案和语录贴纸检查，并删除测试消息。
- `image` 会把指定图片上传到收藏夹，执行带图联网请求，再删除测试消息。

最后两项会真实调用 AI 接口。不要使用含个人信息的测试图片。

## AI 配置

`ai.api_format` 可选：

- `gemini_interactions`
- `openai_chat_completions`
- `openai_responses`

OpenAI 兼容接口的 `ai.base_url` 填版本前缀，例如 `https://api.example.com/v1`。程序会按
`ai.api_format` 追加 `chat/completions` 或 `responses`，不会根据名称或 URL 猜测格式。

只有 Gemini Interactions 和 OpenAI Responses 支持当前实现的原生联网搜索。Chat
Completions 收到搜索命令时会给出普通回答，并明确标注没有联网。

在 Telegram 收藏夹中使用 `.ai config` 可以修改常用设置。修改值保存在 SQLite，并覆盖
TOML：

- `.ai config reload` 重新读取 TOML，但保留 SQLite 中的覆盖项；
- `.ai config reset` 删除所有 `ai.runtime.*` 覆盖，恢复 TOML；
- AI 并发数和插件开关只在启动时读取，修改后需要重启服务。

## 备份

`/var/lib/telebot/telebot.db` 可能包含动态设置、显式保存的 AI Key、聊天上下文和语录存档。
Telegram 会话位于同一目录。备份文件也必须限制访问权限。

备份 SQLite 时，使用 SQLite 的在线备份功能；如果要直接复制文件，先停止服务，并同时复制
数据库及现有的 WAL 文件。完成后对备份运行 `PRAGMA integrity_check`，不要只凭文件存在就认为
备份可用。

`/etc/telebot` 和 `/var/lib/telebot` 不包含在 release 目录中，切换或删除旧 release 不会还原
配置和状态。

## 回滚

先从 `/opt/telebot/releases/` 选择一个仍存在的版本：

```sh
sudo scripts/server/rollback.sh <existing-release-id>
sudo scripts/server/status.sh
```

回滚只切换程序版本并重启服务，不会修改配置、数据库、Telegram 会话或 release 文件。如果目标
版本无法启动，脚本会尝试恢复回滚前的版本。

## 停止服务

```sh
sudo systemctl disable --now telebot.service
sudo docker compose -f /opt/telebot/quote-api/compose.yml down
```

这些命令不会删除 `/etc/telebot`、`/var/lib/telebot`、release 目录、构建缓存或 quote-api
源码。仓库没有提供自动卸载脚本，清理这些数据前请先完成并验证备份。
