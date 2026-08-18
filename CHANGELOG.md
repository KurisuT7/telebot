# Changelog

User-visible changes to telebot are recorded here.

## Unreleased

## [0.3.0] - 2026-08-18

### Added

- Add publisher-built x86_64 and ARM64 Linux release packages with checksums, an installer, and
  clean-runner package checks.
- Add interactive Telegram user authorization with hidden 2FA password input; existing GramJS
  session JSON import remains available as an optional migration path.
- Add explicit `gemini_interactions`, `openai_chat_completions` and `openai_responses` AI formats.
- Add standard OpenAI-compatible text, image and Responses web-search request handling without
  gateway-specific branches.
- Add bounded response parsing, transient retry handling and citation extraction for the new API
  formats.
- Add Simplified Chinese README, operations, contribution and security guides.

### Changed

- Make the prebuilt release the canonical installation path and keep source builds in the
  contribution guide.
- Leave optional quote rendering disabled in the example configuration so the core installation
  does not require Docker.
- Replace the abbreviated setup notes with a complete first-deployment path, explicit session-import
  limitation, AI data-flow boundaries and verified service checks.
- Use the primary model for slow-search hedging when no fallback model is configured, and allow
  hedging to be disabled with a zero-second setting.

[Unreleased]: https://github.com/KurisuT7/telebot/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/KurisuT7/telebot/compare/v0.2.0...v0.3.0
