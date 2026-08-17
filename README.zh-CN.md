# telebot

简体中文 | [English](README.md)

`telebot` 以你的 Telegram 账号登录，在聊天中响应 `.ai` 和 `.q` 命令。它可以调用 AI
回答问题、联网搜索或分析图片，也可以把消息做成语录贴纸和图片。

这是 Telegram 用户客户端，不是 Bot API 机器人。它只处理当前账号本人发出的命令，不会把
其他人的消息当成指令。本项目与 Telegram 没有隶属或授权关系；请只使用自己控制的账号，并遵守
[Telegram API 使用条款](https://core.telegram.org/api/terms)。

## 主要功能

- `.ai`：普通问答、联网搜索、图片理解和可选的多轮上下文。
- `.q`：把一至五条消息生成 WebP 贴纸或 PNG 图片，并可保存到贴纸包。
- 支持 Gemini Interactions、OpenAI Chat Completions 和 OpenAI Responses 三种 API 格式。
- 模型、接口地址、Key、超时和提示词等常用设置可以直接在 Telegram 收藏夹中修改。
- 同时执行的命令数量有限制；短暂重启期间收到的命令会在恢复后继续处理；联网搜索过慢时会并发重试。

## 开始前

当前部署脚本面向带 systemd 的 Linux 服务器。首次部署需要：

- Docker、Docker Compose、Git 和 curl；
- 自己申请的 Telegram `api_id` 和 `api_hash`，申请方法见
  [Telegram 官方文档](https://core.telegram.org/api/obtaining_api_id)；
- 一个使用 Gemini Interactions、OpenAI Chat Completions 或 OpenAI Responses 格式的 AI 接口，
  以及对应的模型和 API Key。

以下命令都在仓库根目录执行。

## 首次部署

先构建程序：

```sh
sudo scripts/server/build-container.sh
```

创建运行账号和目录，再安装示例配置：

```sh
sudo useradd --system --home-dir /var/lib/telebot --shell "$(command -v nologin)" telebot
sudo install -d -m 0755 /etc/telebot
sudo install -d -o telebot -g telebot -m 0700 /var/lib/telebot
sudo install -m 0644 config.example.toml /etc/telebot/config.toml
sudo install -m 0600 deploy/telebot.env.example /etc/telebot/telebot.env
```

在 `/etc/telebot/config.toml` 中填写 `telegram.api_id` 和 AI 接口信息。在
`/etc/telebot/telebot.env` 中填写 `TELEBOT_TELEGRAM_API_HASH` 以及配置文件中
`ai.api_key_env` 指向的变量。不要把这两个文件、Telegram 会话或数据库提交到 Git。

授权 Telegram 账号：

```sh
sudo scripts/server/login.sh
```

按提示输入带国家代码的手机号和 Telegram 验证码。账号启用了两步验证时，还会要求输入密码。
验证码和密码都不会显示在终端中。telebot 只登录已有账号，不能注册新账号。

最后安装本地语录渲染服务并部署 telebot：

```sh
sudo scripts/server/install-quote-api.sh
sudo scripts/server/deploy.sh first
sudo systemctl enable telebot.service
sudo scripts/server/status.sh
```

部署脚本会先检查配置，再切换到新版本。如果启动失败或 35 秒内没有出现
`telebot is ready`，脚本会切回上一个版本。完整的更新、检查、备份和回滚步骤见
[运维文档](docs/operations.zh-CN.md)。

## Telegram 命令

### AI

- `.ai <问题>`：按当前默认模式回答；示例配置默认联网。
- `.ai chat <问题>`：不联网，直接使用模型回答。
- `.ai search <问题>`：强制使用当前 API 格式支持的联网搜索。
- 回复文字、选中的一段文字、图片或静态贴纸后发送 `.ai`，可把它作为本次请求的输入。
- `.ai context on|off` 或 `.ai context <1-20>`：设置当前聊天保留多少轮上下文。
- `.ai reset`：清除当前聊天已经保存的上下文。
- `.ai status` / `.ai config`：查看当前生效的设置；不会显示 Key。
- `.ai help`：在 Telegram 中查看完整命令说明。

AI 回答会保留标题、强调、代码、列表和链接等格式。长消息会自动拆分，链接和富文本不会因此
错位。

### 语录图片和贴纸

- 回复一条消息后发送 `.q [1-5]`：从该消息开始，生成包含一至五条消息的 WebP 贴纸。
- `.q r [1-5]`：同时显示消息引用的内容。
- `.q image [1-5]`：生成 PNG 图片。
- `.q stories [1-5]`：生成适合 Stories 的 PNG 图片。
- `.q s`：把回复的图片或贴纸保存到配置的贴纸包。
- `.q history` / `.q history <ID>`：查看或重新发送已存档的语录。
- `.q config`：查看贴纸包和存档设置；`history on|off` 和 `history limit <1-500>` 用于管理存档。

语录存档默认关闭，开启后只保存新生成的内容。程序会同时按条数和磁盘占用淘汰最旧记录。

语录图片在自己的服务器上生成，不会发送到公共渲染接口。quote-api 的安装和更新方法见
[运维文档](docs/operations.zh-CN.md)。

默认命令前缀为 `.`、`。`、`,`、`，`、`$`、`!` 和 `！`，可在配置文件中修改。命令可以在
普通聊天或收藏夹中使用；telebot 只执行当前账号本人发出的命令。

## AI 接口

`ai.api_format` 决定实际发送的请求格式，`ai.provider` 只是显示名称。telebot 不会根据服务商、
模型名或 URL 猜测协议，也没有针对某个网关的专用分支。

| `api_format` | 联网搜索 | 说明 |
| --- | --- | --- |
| `gemini_interactions` | 支持 | 使用 Gemini Interactions 的原生搜索 |
| `openai_responses` | 支持 | 使用 Responses API 的 `web_search` 工具 |
| `openai_chat_completions` | 不支持 | 该格式没有统一的搜索工具；`.ai search` 会改为普通回答，并注明没有联网 |

接入通用 Chat Completions 接口时，可按下面修改 `/etc/telebot/config.toml`：

```toml
[ai]
provider = "generic-oai"
api_format = "openai_chat_completions"
api_key_env = "TELEBOT_AI_API_KEY"
base_url = "https://api.example.com/v1"
model = "example-model"
search_fallback_model = ""
default_search = false
```

`base_url` 填 API 前缀即可；若末尾不是完整端点，telebot 会按所选格式追加
`chat/completions` 或 `responses`。只有服务端确实实现 Responses API 时才选择
`openai_responses`。

对于支持联网搜索的格式，如果主请求在 `search_hedge_seconds` 内没有完成，telebot 会再发起
一次请求，并采用先成功的结果。`search_fallback_model` 留空时，两次请求使用同一个主模型；
填写模型名后，第二次请求才会改用该模型。将 `search_hedge_seconds` 设为 `0` 可以关闭抢跑。
带图请求有单独的 `image_search_timeout_seconds`，不会拉长纯文字请求的等待上限。

## 运行中修改 AI 设置

以下命令只能在 Telegram 收藏夹中使用，修改后立即生效：

- `.ai config provider <API格式> <名称> <BaseURL>`
- `.ai config model <主模型> [搜索备用模型|off]`
- `.ai config key <Key>` / `.ai config clear-key`
- `.ai config prompt <系统提示词>`
- `.ai config thinking <minimal|low|medium|high>`
- `.ai config search <on|off>`
- `.ai config timeout <文字秒> <图片秒> <抢跑秒> <兜底秒>`
- `.ai config tokens <1-65536>`
- `.ai config collapse <on|off>`
- `.ai config message searching|thinking <文案>`
- `.ai config reload` / `.ai config reset`

这些设置保存在 `/var/lib/telebot/telebot.db`。`.ai config key` 会先隐藏含 Key 的命令，
不会在回复中显示 Key；`clear-key` 会恢复使用服务器环境变量。AI 并发数和插件开关仍需修改
TOML 并重启服务。

## 数据会发送到哪里

- 消息的读取、编辑、发送和贴纸包操作仍通过 Telegram 完成。
- 登录时，手机号、验证码和可选的 2FA 密码只发送给 Telegram；telebot 保存登录后的 Session，
  不把验证码或密码写入配置和数据库。
- AI 服务会收到本次问题、回复的文字或图片，以及已开启的聊天上下文。
- 语录渲染默认只经过本机的 quote-api。
- Telegram 会话、动态 AI 设置、聊天上下文和语录存档保存在 `/var/lib/telebot`。

请把 `/etc/telebot/telebot.env` 和 `/var/lib/telebot` 限制为服务器管理员及 telebot 运行账号可读。

## 当前限制

- 仓库提供的部署和运维脚本只面向 Linux、systemd 和 Docker。
- 登录支持手机号、验证码和 2FA 密码，暂不支持二维码登录或注册新账号。
- Chat Completions 没有统一的联网搜索协议。
- AI 图片输入只接受照片和静态贴纸；语录渲染需要单独运行 quote-api。

## 项目文档

- [运维与升级](docs/operations.zh-CN.md)
- [参与开发](CONTRIBUTING.zh-CN.md)
- [报告安全问题](SECURITY.zh-CN.md)
- [更新记录](CHANGELOG.md)
- [MIT License](LICENSE)
