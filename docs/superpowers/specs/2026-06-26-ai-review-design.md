# AI Review Design

> Historical design note: this document records the original AI Review implementation plan. It is not the current behavior contract. The current service is manual MR comment-triggered; see `docs/design.md` and `docs/gitlab-webhook.md`.

## Goal

Add native AI-powered merge request review to GitLabWorkRunner while preserving the existing rule, script task, GitLab comment, and deduplication model.

## Scope

The first implementation adds an OpenAI-compatible provider configured from `rules.toml`. It supports automatic review on merge request events and manual review from merge request comments such as `@ai-review`.

The implementation does not download the full repository for AI review, does not run arbitrary repository commands, and does not attempt multi-provider abstractions beyond an OpenAI-compatible HTTP API.

## Configuration

AI reviews are configured in `rules.toml`:

```toml
[[ai_reviews]]
enabled = true
id = "ai-review"
title = "AI Review"
provider = "openai-compatible"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
model = "gpt-4.1-mini"
trigger = "auto_and_manual"
timeout_seconds = 60
max_diff_bytes = 60000
when_changed = ["**/*.rs", "**/*.toml"]
```

Fields:

- `enabled`: controls automatic execution only; default `true`.
- `id`: unique command and review identifier.
- `title`: title used in GitLab comments.
- `provider`: currently only `openai-compatible`.
- `base_url`: API base URL, with or without a trailing slash.
- `api_key_env`: environment variable containing the AI API token.
- `model`: model name sent to the provider.
- `trigger`: `auto`, `manual`, or `auto_and_manual`; default `auto_and_manual`.
- `timeout_seconds`: HTTP timeout; default `60`.
- `max_diff_bytes`: maximum prompt diff payload size; default `60000`.
- `when_changed`: optional glob list used only for automatic execution.

## Triggering

Automatic review runs from `ReviewService::review_merge_request` after built-in line rules and before script tasks. It requires:

- `enabled = true`
- `trigger` allows automatic execution
- `when_changed` is empty or matches at least one changed path
- GitLab diff refs are complete

Manual review runs from `ReviewService::review_merge_request_note`. It detects standalone `@<ai_review.id>` tokens. Manual review ignores `enabled` and `when_changed`, but still requires `trigger` to allow manual execution. This mirrors manual script task behavior while keeping trigger intent explicit.

## AI Input

The service sends only GitLab merge request diff data. It includes file paths, deletion/rename metadata, and unified diff text. The prompt instructs the model to review only added lines and return line comments for actionable findings.

The diff payload is truncated by `max_diff_bytes` at UTF-8 character boundaries. If truncation occurs, the prompt explicitly states that the diff is partial.

## AI Output

The provider must return JSON in the assistant message:

```json
{
  "findings": [
    {
      "path": "src/lib.rs",
      "line": 42,
      "severity": "warning",
      "title": "Possible panic",
      "message": "This unwrap can panic when the value is absent."
    }
  ]
}
```

The service parses this into existing `Finding` values:

- `rule_id`: `ai:<id>`
- `severity`: parsed from `info`, `warning`, or `error`; invalid values become `warning`
- `path`: normalized to repository-style `/` separators
- `new_line`: required for line-level comments
- `title`: fallback to the configured AI review title if empty
- `message`: required

Findings whose path and line are not added lines in the current MR diff are filtered out and logged. This prevents the model from posting stale or invented positions.

## Publishing

Valid findings reuse existing `build_comment_drafts` and `publish_comment_drafts`, so AI comments behave like normal GitLab line discussions and use the same fallback when GitLab rejects a line position.

The stable marker is:

```text
<!-- gitlab-work-runner:rule=ai:<id> -->
```

## Errors

AI errors do not block built-in rules or script tasks.

The service logs and skips AI comments for:

- missing API key environment variable
- HTTP timeout or transport failure
- non-2xx provider response
- malformed provider response
- invalid JSON content
- incomplete GitLab diff refs

Secrets are never logged. Logs may include provider, model, AI review id, elapsed time, and finding counts.

## Tests

Tests cover:

- `[[ai_reviews]]` TOML parsing and default values.
- automatic task selection by `enabled`, `trigger`, and `when_changed`.
- manual selection by `@ai-review`.
- OpenAI-compatible response parsing.
- filtering findings to added lines only.
- e2e mock GitLab plus mock AI provider publishing a line-level discussion.
