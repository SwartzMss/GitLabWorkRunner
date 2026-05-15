# GitLabWorkRunner

Language: [简体中文](README.md) | **English**

GitLabWorkRunner is a Rust service for automated GitLab Merge Request review.

It is not a full GitLab Runner replacement, and it does not automatically execute CI scripts from the target repository. The service only runs review rules and script tasks explicitly configured in `rules.toml`.

1. Receive GitLab Merge Request webhooks.
2. Fetch MR diffs through the GitLab API.
3. Parse unified diff hunks and added-line positions.
4. Run configurable rules from `rules.toml`.
5. Publish comments to GitLab MR Discussions.
6. Store processed commit state in SQLite to avoid duplicate comments.

## Project Direction

The service is inspired by reviewdog's useful diff parsing and line-positioning ideas, but it is intentionally narrower:

- GitLab-only.
- Webhook-driven.
- Review rules configured through `rules.toml`.
- SQLite state storage for the first version.
- GitLab MR Discussion output instead of generic multi-platform reporters.

See [docs/design.md](docs/design.md) for the detailed design.

## Architecture

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

The first version currently supports:

- Starting a local HTTP service.
- Accepting GitLab Merge Request webhook events.
- Validating the webhook secret token.
- Fetching MR changes from GitLab.
- Applying regex-based rules to added lines.
- Downloading the MR head snapshot and running configured script tasks.
- Publishing line-level MR comments.
- Avoiding reprocessing the same commit and ruleset.
- Writing the full review flow to stdout and a log file.
- Rotating log files by size so a single file does not grow forever.

## Configuration

Service configuration uses `config.toml`:

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

[logging]
file = "logs/gitlab-work-runner.log"
max_bytes = 10485760
max_files = 5
```

Rules use `rules.toml`:

```toml
[[rules]]
id = "forbid-unwrap"
title = "Avoid unwrap"
severity = "warning"
path = "**/*.rs"
pattern = "\\.unwrap\\(\\)"
message = "Direct unwrap can panic at runtime. Prefer explicit error handling."

[[script_tasks]]
enabled = false
id = "check-todo-tbd"
title = "TODO/TBD marker check"
command = "python3 examples/scripts/check_todo_tbd.py"
timeout_seconds = 30
when_changed = ["**/*.c", "**/*.cc", "**/*.cpp", "**/*.h", "**/*.hpp", "**/*.rs"]
```

### Script Tasks

`[[script_tasks]]` are independent tasks and do not change existing `[[rules]]` line checks. Each task has its own `enabled` flag; there is no global switch.

Behavior:

- `enabled` defaults to `true` when omitted.
- If `when_changed` is omitted or empty, the task runs for every MR.
- The service always downloads the current MR head commit archive.
- The command runs from the extracted MR head repository root, which is the code snapshot being checked.
- stdout and stderr are merged into one `output.log`.
- `exit 0` means pass and does not create a comment.
- `exit != 0` or timeout creates one MR-level comment.
- Timeout is enforced by the Rust process; `timeout_seconds` defaults to `60`.
- The service appends the MR head source snapshot root to the command, so scripts can read it as their first business argument.

Work directory:

```text
work/script_tasks/<project_id>/<mr_iid>/<commit_sha>/<task_id>/
  output.log
```

After execution, the extracted `source/` directory is removed and only `output.log` is kept for debugging. Script tasks remove the configured GitLab token environment variable before running the command.

The repository includes a minimal script example: [examples/scripts/check_todo_tbd.py](examples/scripts/check_todo_tbd.py). It reads the first argument as the directory to check and fails when it finds `//TODO` or `//TBD`, printing file locations.

Note: the relative path in `command = "python3 examples/scripts/check_todo_tbd.py"` is resolved from the MR source snapshot root. If the target GitLab repository does not contain that script, either copy the example script into the target repository or change `command` to an absolute path on the runner machine. On Windows, exit code `9009` usually means the command is not found; use `python` instead of `python3` or add Python to `PATH`.

## Local Run

Windows PowerShell:

```powershell
Copy-Item config.example.toml config.toml
Copy-Item rules.example.toml rules.toml
$env:GITLAB_TOKEN = "<your-token>"
cargo run
```

Linux / macOS:

```bash
cp config.example.toml config.toml
cp rules.example.toml rules.toml
export GITLAB_TOKEN="<your-token>"
cargo run
```

Configure a GitLab project webhook:

- URL: `http://<host>:8080/webhooks/gitlab`
- Secret token: the value of `[server].webhook_secret`
- Trigger: Merge request events

For details about when `Merge request events` are triggered and which payload fields matter, see [GitLab Webhook notes](docs/gitlab-webhook.md).

## Logs

The service writes logs to both stdout and the configured log file:

```toml
[logging]
file = "logs/gitlab-work-runner.log"
max_bytes = 10485760
max_files = 5
```

Use `RUST_LOG` to control verbosity:

```powershell
$env:RUST_LOG = "info"
cargo run
```

For each Merge Request webhook, the log flow includes:

- Webhook received and payload size.
- Webhook token validation failure, if rejected.
- Parsed `project_id`, `mr_iid`, `commit_sha`, action, source branch, and target branch.
- Review start with `ruleset_hash`.
- Duplicate commit/ruleset skip decision.
- GitLab MR changes fetch start and completion.
- Script task archive download, execution, timeout, and output file path.
- diff refs: `base_sha`, `start_sha`, `head_sha`.
- Changed file count.
- Per-file diff evaluation: path, hunk count, finding count, new/renamed/deleted flags.
- Total findings and comment drafts.
- Comment publish attempts with path and line number.
- GitLab discussion id and note id after publish.
- Fallback from line-level discussion to MR-level discussion when GitLab rejects a position.
- Final review summary: skipped, finding count, comment count.

GitLab tokens and webhook secrets are intentionally not logged.

### Log Rotation

The service includes built-in size-based log rotation. The default configuration is:

```toml
[logging]
file = "logs/gitlab-work-runner.log"
max_bytes = 10485760
max_files = 5
```

When the next write would exceed `max_bytes`, the service rotates before writing:

- The active file is renamed to `gitlab-work-runner.log.1`.
- Existing `.1` files are shifted to `.2`, up to `max_files`.
- At most `max_files` history files are retained.
- With `max_files = 0`, no history files are retained and the active log file is recreated.

This is size-based rotation only. It does not provide time-based rotation, compression, upload, or centralized collection. In production, container runtimes, logging platforms, `logrotate`, or Windows log collection systems can still manage logs externally.

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for details.
