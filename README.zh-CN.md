# telebot

简体中文 | [English](README.md)

`telebot` 是一个独立的 Rust Telegram 用户客户端。项目刻意保持精简：运行时、命令路由、
状态存储、AI 服务和引用图片功能相互分离，因此添加新命令时不需要修改 Telegram 连接代码。

目前提供以下命令：

- `.ai <问题>`：默认使用原生联网搜索。
- 回复文字、选中的引用、图片或静态贴纸后发送 `.ai`，将回复内容用于本次请求。
- `.ai chat <问题>` / `.ai search <问题>`：强制使用普通回答或原生联网搜索。
- `.ai config`：查看当前 AI 服务和 API 格式，不显示 Key 内容。
- `.ai config provider <api_format> <name> <base_url>`：修改线协议、显示名称和接口地址。
- `.ai config key <key>` / `.ai config model <primary> [search_fallback]`：修改凭据或模型。
- `.ai config prompt|thinking|search|timeout|tokens|collapse|message`：无需重新构建，立即修改常用 AI 行为和进度文案。
- `.ai config reload`：重新读取服务器 AI 和消息 TOML，同时保留 SQLite 动态覆盖；`.ai config reset` 清除这些覆盖。
- `.ai context <0-20|on|off>`：设置每个聊天的滚动上下文；默认关闭。
- `.ai reset|status|help`：清除当前聊天上下文、查看配置或显示详细使用与安全说明。
- `.q [1-5]`：使用被回复消息及后续消息生成引用贴纸。
- `.q r [1-5]`、`.q image [1-5]`、`.q stories [1-5]`：包含回复或选择 PNG 布局。
- `.q history` / `.q history <id>`：列出最近存档的引用或按 ID 重新发送。
- `.q s`：把被回复的贴纸或图片添加到配置的贴纸包。
- `.q config`：查看引用图片设置；使用 `history on|off` 和 `history limit <1-500>` 控制存档。

所有命令都接受配置中的任意前缀。默认部署配置为 `.`、`。`、`,`、`，`、`$`、`!` 和 `！`。

Telegram 可能把同一账号写出的命令同步为 `outgoing=false`，收藏夹中尤其如此。只有消息为
outgoing、发送者为已授权账号或会话为收藏夹时，路由器才接受命令；其他用户发来的命令会被忽略。
默认部署启用更新追赶，因此短暂重启期间写出的命令不会丢失。

AI 回答使用 Telegram 原生富文本实体。Markdown 标题、强调、代码、列表和链接会被保留；
`Q:` 和 `A:` 默认都使用可展开引用。过长回答会在不破坏 UTF-16 实体偏移的位置拆分。
服务商、模型和上下文设置保存在本地应用数据库中。`.ai config key` 只能在收藏夹中使用：
命令会立即隐藏，Key 不会回显，并保存在仅所有者可访问的本地数据库中；没有动态 Key 时，
程序继续从服务器环境变量读取凭据。

AI 适配器支持 `gemini_interactions`、`openai_chat_completions` 和 `openai_responses`。
OpenAI 兼容服务只使用所选标准线协议，代码中没有针对特定网关的分支。`base_url` 应填写
API 前缀，例如 `https://api.example.com/v1`；如果地址末尾还没有对应端点，telebot 会追加
`chat/completions` 或 `responses`。

Gemini Interactions 和 OpenAI Responses 支持原生联网搜索。OpenAI Chat Completions 没有
统一的标准搜索工具，因此强制搜索失败后会返回带有明确提示的非联网回答。原生搜索会在主模型
变慢后启动备用模型并采用先完成的结果，同时对 HTTPS 引用去重后渲染为 Telegram 链接。带图搜索使用单独且更长的
总超时，不会改变纯文字请求的延迟预算。

接入通用 Chat Completions 接口时，修改 `config.toml` 中的以下字段，并在仓库外提供对应的
环境变量：

```toml
[ai]
provider = "generic-oai"
api_format = "openai_chat_completions"
api_key_env = "TELEBOT_AI_API_KEY"
base_url = "https://api.example.com/v1"
model = "example-model"
search_fallback_model = "example-model"
default_search = false
```

只有接口确实实现 Responses 协议时才使用 `openai_responses`。该格式会启用标准 Responses
`web_search` 工具，并要求配置搜索备用模型。Telebot 不会根据服务商名称、模型名称或 URL
自动猜测协议。

经常调整的 AI 值作为运行时设置保存在 SQLite 中。服务商、BaseURL、Key、模型、系统提示词、
思考等级、默认搜索、超时、最大输出、引用折叠和 AI 进度文案都可以在收藏夹中修改并立即生效。
TOML 的 `[messages]` 部分提供服务器默认值。并发上限和插件启停仍然只在启动时读取。

## 设计

- `src/main.rs`：进程生命周期、Telegram 更新流和有并发上限的命令工作器。
- `src/plugin.rs`：精简的插件 trait 和命令路由；后续功能不需要修改连接循环。
- `src/plugins/ai.rs`：与服务商名称无关的运行时设置、Gemini 与 OpenAI 兼容适配器、有限上下文和原生搜索回退。
- `src/plugins/quote.rs`：引用图片生成、有限媒体存档和 Telegram 贴纸包操作。
- `src/telegram.rs`：Telegram 原生 Markdown 格式、可展开问答引用和安全消息拆分。
- `src/store.rs`：异步 SQLite 设置、有限 AI 上下文和引用图片存档。
- `src/session_import.rs`：一次性、离线的 GramJS StringSession 迁移。

引用图片使用固定到已审查提交的官方 `LyoSU/quote-api` 源码。它与 telebot 一起自托管，
只监听 `127.0.0.1:3210`，并以无特权、只读容器运行，同时包含 Noto CJK 字体。这样不依赖
不稳定的公共渲染服务，也能正确显示中文字符。

## 命令行

```text
telebot validate --config /etc/telebot/config.toml
telebot import-gramjs-session --from /path/to/TeleBox/config.json --to /var/lib/telebot/telegram.session
telebot check-session --config /etc/telebot/config.toml
telebot check-ai --config /etc/telebot/config.toml
telebot check-quote --config /etc/telebot/config.toml
telebot check-telegram-image --config /etc/telebot/config.toml --image /path/to/test-image.png
telebot serve --config /etc/telebot/config.toml
```

会打开 Telegram 会话的命令不能与 `telebot.service` 同时运行。在 systemd 主机上，请使用
`scripts/server/check-telegram.sh session|format|plugins|all`，或使用
`scripts/server/check-telegram.sh image /path/to/test-image.png`。脚本会先停止服务，执行检查，
随后确认服务重新进入就绪状态。

构建使用固定的 Rust 1.90 工具链、已提交的 `Cargo.lock` 和可复用的容器缓存。部署、健康检查、
回滚和升级说明位于 [docs/operations.md](docs/operations.md)。

## 项目规则

- 运维：[docs/operations.md](docs/operations.md)
- 贡献：[CONTRIBUTING.md](CONTRIBUTING.md)
- 安全报告：[SECURITY.md](SECURITY.md)
- 许可：[MIT](LICENSE)
