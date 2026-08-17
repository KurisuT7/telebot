# Changelog

User-visible changes to telebot are recorded here.

## Unreleased

### Added

- Add explicit `gemini_interactions`, `openai_chat_completions` and `openai_responses` AI formats.
- Add standard OpenAI-compatible text, image and Responses web-search request handling without
  gateway-specific branches.
- Add bounded response parsing, transient retry handling and citation extraction for the new API
  formats.
