# 参与开发

简体中文 | [English](CONTRIBUTING.md)

提交的改动应当能在不依赖原作者账号、服务器或私有配置的情况下使用。不要把只适用于某个网关、
某台主机或某个账号的判断写进通用逻辑。

## 提交前检查

仓库固定使用 Rust 1.90。请在仓库根目录运行：

```sh
cargo fmt --all --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
```

Linux 主机也可以使用 `scripts/server/build-container.sh` 在 Docker 中完成同样的检查。

修改协议、响应解析或配置格式时，请补充正常和异常输入测试。测试数据必须是人工构造或明确允许
再分发的内容。不要提交 Telegram 会话、数据库、真实消息、API Key、生产配置、日志或用户图片。

用户可见行为发生变化时，请同时更新：

- README 中的命令和限制；
- `config.example.toml` 中的示例；
- 中英文运维文档；
- `CHANGELOG.md` 的 `Unreleased` 部分。

OpenAI 兼容适配器只按公开协议区分请求格式。除非某项行为属于所选协议，并有测试覆盖，否则不要
添加服务商或网关名称判断。

每个提交只处理一个明确目的。兼容性变化、数据迁移或运维风险不能从代码差异直接看出时，请在提交
说明中写清楚。
