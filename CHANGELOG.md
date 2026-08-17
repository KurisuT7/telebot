# Changelog

User-visible changes to telebot are recorded here.

## Unreleased

### Added

- Add interactive Telegram user authorization with hidden 2FA password input; existing GramJS
  session JSON import remains available as an optional migration path.
- Add explicit `gemini_interactions`, `openai_chat_completions` and `openai_responses` AI formats.
- Add standard OpenAI-compatible text, image and Responses web-search request handling without
  gateway-specific branches.
- Add bounded response parsing, transient retry handling and citation extraction for the new API
  formats.
- Add Simplified Chinese README, operations, contribution and security guides.

### Changed

- Replace the abbreviated setup notes with a complete first-deployment path, explicit session-import
  limitation, AI data-flow boundaries and verified service checks.
- Use the primary model for slow-search hedging when no fallback model is configured, and allow
  hedging to be disabled with a zero-second setting.
