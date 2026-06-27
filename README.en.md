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
- Calling an OpenAI-compatible API for configured AI Review.
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
command = "python examples/scripts/check_todo_tbd.py"
timeout_seconds = 30
when_changed = ["**/*.c", "**/*.cc", "**/*.cpp", "**/*.h", "**/*.hpp", "**/*.rs"]

[[ai_reviews]]
enabled = false
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

### AI Review

`[[ai_reviews]]` configures native AI Review independently from `[[rules]]` and `[[script_tasks]]`. The current provider is an OpenAI-compatible `POST /chat/completions` API.

Behavior:

- `enabled` defaults to `true` and controls automatic execution only.
- `trigger` supports `auto`, `manual`, and `auto_and_manual`; the default is `auto_and_manual`.
- Automatic execution requires `enabled = true`, an automatic-capable `trigger`, and an empty or matching `when_changed`.
- Manual execution uses a standalone command token in an MR comment, for example `@ai-review`.
- Manual execution ignores `enabled` and `when_changed`, but `trigger` must allow `manual`.
- `api_key_env` names the environment variable that contains the AI API token; tokens are not logged.
- The service sends only the GitLab MR diff to AI and does not download the full repository.
- `max_diff_bytes` limits the diff text sent to AI; the default is `60000`.
- AI findings are published only when they point to added lines in the current MR diff. Other findings are filtered and logged.
- AI failures, timeouts, non-2xx responses, and invalid JSON do not block regex rules or script tasks.
- If GitLab returns incomplete diff refs, automatic MR review skips line rules, AI Review, and script tasks as a whole, then publishes one MR-level skip notice; manual AI Review is also skipped.

The AI service should return an OpenAI-compatible chat completion whose assistant message `content` is strict JSON:

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

Set the AI token before running locally, for example:

```bash
export OPENAI_API_KEY="<your-ai-api-key>"
```

### Script Tasks

`[[script_tasks]]` are independent tasks and do not change existing `[[rules]]` line checks. Each task has its own `enabled` flag; there is no global switch.

Behavior:

- `enabled` defaults to `true` when omitted.
- When `enabled = true`, MR creation or updates run the task automatically according to `when_changed`.
- When `enabled = false`, the task does not run automatically; it can still be triggered manually from an MR comment with `@task-id`, for example `@check-todo-tbd`.
- Manual triggers ignore `enabled` and `when_changed`; they select script tasks only by exact `id`.
- Manual triggers are not deduplicated; each valid command comment runs once.
- If `when_changed` is omitted or empty, automatic triggers run the task for every MR.
- The service always downloads the current MR head commit archive.
- The command runs from the runner executable directory; relative paths in `command` are resolved from that directory.
- stdout and stderr are merged into `run.log` for script execution logs.
- The service passes the `result.txt` path as the second argument; scripts should write check results to that file.
- `exit 0` means the check passed.
- `exit 1` means the check found issues; the service reads `result.txt` and publishes MR comments.
- Other exit codes, missing exit codes, and timeouts mean script execution errors.
- Execution errors and timeouts do not create MR comments; the service only logs them and keeps `run.log` / `result.txt`.
- Timeout is enforced by the Rust process; `timeout_seconds` defaults to `60`.
- The service appends the MR head source snapshot root as the first argument.

`result.txt` supports a simple line-comment format:

```text
src/config.rs:5: //TODO aa
```

Each line is parsed as `repository-relative path:line number:message`. Parsed results are published as line-level comments when possible and when the current MR diff refs are complete. If the result cannot be parsed, the service publishes one MR-level summary comment instead. Scripts may keep header lines such as `Found //TODO or //TBD markers:` in `result.txt`; those lines are not treated as line results.

Note: automatic MR review skips the whole review when GitLab returns incomplete diff refs and publishes one MR-level skip notice, so automatic script tasks do not continue in that case. For manually triggered script tasks, if diff refs are incomplete but the archive can be downloaded, the task still runs; issue results are published as an MR-level summary comment.

Work directory:

```text
work/script_tasks/<project_id>/<mr_iid>/<commit_sha>/<task_id>/
  run.log
  result.txt
```

After execution, the extracted `source/` directory is removed and only `run.log` and `result.txt` are kept for debugging. Script tasks remove the configured GitLab token environment variable before running the command.

The repository includes a minimal script example: [examples/scripts/check_todo_tbd.py](examples/scripts/check_todo_tbd.py). It reads the first argument as the directory to check and the second argument as the result file path; process logs go to stdout and check results are written to `result.txt` as `path:line:message`.

Note: the relative path in `command = "python examples/scripts/check_todo_tbd.py"` is resolved from the runner executable directory. If you use the example script from the release package, keep this path. If the script lives elsewhere, use an absolute path. On Windows, exit code `9009` usually means the command is not found; add Python to `PATH`.

Manual triggers require the GitLab Webhook to enable both `Comments` and `Merge request events`. The service only handles standalone command tokens in MR comments, for example:

```text
@check-todo-tbd
```

It also works inside multi-line comments:

```text
Please run:
@check-todo-tbd
```

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
- Trigger: `Merge request events`; also enable `Comments` if you need manual script task or AI Review triggers from MR comments

For details about when `Merge request events` and `Comments` are triggered and which payload fields matter, see [GitLab Webhook notes](docs/gitlab-webhook.md).

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
- AI Review provider, model, result, and finding count.
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
