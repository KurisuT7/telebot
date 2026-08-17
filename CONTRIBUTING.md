# Contributing

Contributions should keep telebot small, bounded and usable without the original maintainer's
accounts or infrastructure.

## Development checks

Use the pinned Rust toolchain and run these commands from the repository root:

```sh
cargo fmt --all --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
```

The container-backed equivalent is `scripts/server/build-container.sh` on a Linux host with Docker.

Add tests for protocol or parsing changes. Fixtures must be synthetic or explicitly redistributable;
do not commit Telegram sessions, databases, real messages, credentials, production configuration,
logs or user media. Keep README instructions, configuration examples and the `Unreleased` changelog
section consistent with user-visible behavior.

Use focused commits and explain compatibility or operating risk when it is not obvious from the
change. Do not add provider-specific behavior to an OpenAI-compatible adapter unless it is part of
the documented wire protocol and covered by a test.
