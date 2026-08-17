# telebot 运维说明

简体中文 | [English](operations.md)

本文档对应仓库提供的 Linux、systemd 和 Docker 部署脚本。除非命令中另有说明，以下操作都在
仓库根目录执行，并使用 `sudo` 完成需要 root 权限的步骤。

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

### 2. 构建

主机需要 Docker，并且当前账号能够使用 Docker。构建脚本固定使用 Rust 1.90，依次运行格式
检查、测试、Clippy 和 release 构建：

```sh
sudo scripts/server/build-container.sh
```

Cargo 下载和编译缓存位于 `/var/cache/telebot-build`。生成的程序会复制到
`target/release/telebot`，命令最后会输出 SHA-256。

### 3. 创建运行账号和配置

```sh
sudo useradd --system --home-dir /var/lib/telebot --shell "$(command -v nologin)" telebot
sudo install -d -m 0755 /etc/telebot
sudo install -d -o telebot -g telebot -m 0700 /var/lib/telebot
sudo install -m 0644 config.example.toml /etc/telebot/config.toml
sudo install -m 0600 deploy/telebot.env.example /etc/telebot/telebot.env
```

如果 `telebot` 账号已经存在，不要重复执行 `useradd`。

在 `/etc/telebot/config.toml` 中至少修改：

- `telegram.api_id`；
- `ai.api_format`、`ai.base_url` 和 `ai.model`；只有需要第二个搜索模型时才填写
  `ai.search_fallback_model`；
- 如果不使用 Gemini 示例，把 `ai.api_key_env` 改成自己的环境变量名。

在 `/etc/telebot/telebot.env` 中填写：

- `TELEBOT_TELEGRAM_API_HASH`；
- `ai.api_key_env` 指定的 AI Key 变量。

环境变量文件的所有者应为 root，权限为 `0600`。systemd 会在启动 telebot 前读取它，Key
不需要写进 TOML。

### 4. 登录 Telegram

运行登录脚本：

```sh
sudo scripts/server/login.sh
```

按提示输入带国家代码的手机号和 Telegram 验证码。账号启用了两步验证时，脚本还会要求输入
2FA 密码。验证码和密码都不会显示在终端中。成功后会话保存在
`/var/lib/telebot/telegram.session`，权限设为 `0600`。

如果 telebot 服务正在运行，脚本会先停止服务，登录结束后再恢复；不要同时运行登录命令和
`telebot.service`。会话已经有效时，命令会直接确认状态，不会再次发送验证码。

已有 GramJS Session 的迁移方法单独放在[迁移说明](gramjs-migration.zh-CN.md)中，不影响新用户登录。

### 5. 安装语录渲染服务

```sh
sudo scripts/server/install-quote-api.sh
```

脚本会检出 [LyoSU/quote-api](https://github.com/LyoSU/quote-api) 的固定提交，构建本地镜像，并启动
`telebot-quote-api` 容器。服务只绑定 `127.0.0.1:3210`，没有持久卷；中文字体包含在镜像中。
安装完成时会检查 `/health`。

### 6. 部署并检查

`release-id` 只能包含字母、数字、点、下划线和短横线：

```sh
sudo scripts/server/deploy.sh first
sudo systemctl enable telebot.service
sudo scripts/server/status.sh
```

部署脚本会：

1. 把程序和运维文件复制到新的 release 目录；
2. 读取环境变量并运行 `telebot validate`；
3. 原子切换 `/opt/telebot/current`；
4. 重启服务并等待日志出现 `telebot is ready`；
5. 检查失败时切回先前版本。

`status.sh` 应显示 `ActiveState=active`、`SubState=running`，quote renderer 的健康状态应为
`ok`。

## 更新

更新代码后，重新构建并使用新的 release ID 部署：

```sh
sudo scripts/server/build-container.sh
sudo scripts/server/deploy.sh 2026-08-17
sudo scripts/server/status.sh
```

不要复用已经存在的 release ID。部署不会删除旧版本、配置、会话或 SQLite 数据。

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
