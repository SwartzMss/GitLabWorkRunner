# GitLabWorkRunner

GitLabWorkRunner is a Rust service for automated GitLab Merge Request review.

It is not a full GitLab Runner replacement. The first version focuses on a
small review loop:

1. Receive GitLab Merge Request webhooks.
2. Fetch MR diffs through the GitLab API.
3. Parse unified diff hunks and added-line positions.
4. Run configurable review rules.
5. Publish GitLab MR discussions.
6. Store processed commit state to avoid duplicate comments.

## Project Direction

The service is inspired by reviewdog's useful diff and line-positioning ideas,
but it is intentionally narrower:

- GitLab-only.
- Webhook-driven.
- Rule configuration through `rules.toml`.
- SQLite state storage for the first version.
- MR discussion output instead of generic multi-platform reporters.

See [docs/design.md](docs/design.md) for the current architecture and first
delivery boundary.

## Planned Architecture

```text
GitLab Merge Request Event
  -> Webhook Server
  -> Event Scheduler / Deduplicator
  -> GitLab API Client
  -> Diff Fetcher
  -> Diff Parser
  -> Rule Engine
  -> Comment Builder
  -> GitLab Discussion Publisher
  -> State Store
```

## First Version Scope

The first implementation should be able to:

- Start a local HTTP service.
- Accept GitLab Merge Request webhook events.
- Validate the webhook secret token.
- Fetch MR changes from GitLab.
- Apply regex-based rules to added lines.
- Publish line-level MR comments.
- Avoid reprocessing the same commit and ruleset.

## Configuration Draft

Service configuration will use `config.toml`:

```toml
[server]
bind = "0.0.0.0:8080"
webhook_secret = "change-me"

[gitlab]
base_url = "https://gitlab.example.com"
token_env = "GITLAB_TOKEN"

[storage]
database_url = "sqlite://gitlab-work-runner.db"

[rules]
file = "rules.toml"
```

Rules will use `rules.toml`:

```toml
[[rules]]
id = "forbid-unwrap"
title = "Avoid unwrap"
severity = "warning"
path = "**/*.rs"
pattern = "\\.unwrap\\(\\)"
message = "Direct unwrap can panic at runtime. Prefer explicit error handling."
```

## Development Status

The first Rust implementation is in progress from the first-version scope in
[docs/design.md](docs/design.md).

## Local Run

```powershell
Copy-Item config.example.toml config.toml
Copy-Item rules.example.toml rules.toml
$env:GITLAB_TOKEN = "<your-token>"
cargo run
```

Configure a GitLab project webhook:

- URL: `http://<host>:8080/webhooks/gitlab`
- Secret token: the value of `[server].webhook_secret`
- Trigger: Merge request events
