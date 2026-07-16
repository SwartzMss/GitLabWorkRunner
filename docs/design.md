# GitLab MR Manual Review Service Design

## Goal

GitLabWorkRunner is a GitLab-specific Merge Request review service. It receives GitLab webhooks, runs explicitly configured review tasks when a user requests them from an MR comment, and publishes findings back to GitLab MR Discussions.

The current service is comment-driven. Merge request events are accepted for visibility and webhook testing, but they are ignored with reason `merge_request_events_manual_triggers_only`; they do not enter the review queue.

The review loop is:

1. Receive a GitLab MR note event containing a standalone `@id`.
2. Validate the webhook token.
3. Resolve the requested `[[ai_reviews]]` entries by exact id.
4. Guard against duplicate in-flight work for `project_id + mr_iid + commit_sha`.
5. Fetch MR changes through the GitLab API.
6. Parse the diff and map added lines.
7. Run the selected AI reviews.
8. Publish line-level or MR-level comments.
9. Store review runs, task runs, findings, and published comments in SQLite.

## Non-Goals

- It is not a GitLab Runner replacement.
- It does not automatically execute CI scripts from the target repository.
- It does not support GitHub, Bitbucket, Gitea, or other review platforms.
- It does not implement a container sandbox or distributed scheduler.
- It does not provide a general LLM plugin system; AI review currently uses OpenAI-compatible Chat Completions.

## Architecture

```text
GitLab MR Note Event
  -> Webhook Server
  -> Manual Command Selector
  -> Active Review Guard
  -> GitLab API Client
  -> Diff Parser
  -> Review Engine
  -> Comment Builder
  -> GitLab Discussion Publisher
  -> SQLite Store
```

Merge request events follow a shorter path:

```text
GitLab Merge Request Event
  -> Webhook Server
  -> ignored: merge_request_events_manual_triggers_only
```

## Webhook Server

Responsibilities:

- Expose `POST /webhooks/gitlab`.
- Validate `X-Gitlab-Token` against `[server].webhook_secret`.
- Parse MR note events and merge request events.
- For MR events, log project/MR/commit context and return ignored.
- For MR note events, extract `project_id`, `mr_iid`, note id, note text, and current commit sha.
- Return quickly while review work runs in the background.

GitLab 14.5 note payloads may omit `object_attributes.action`; missing action is treated as `create`. Explicit non-create note actions are ignored.

## Manual Command Selection

Manual commands are standalone tokens in MR comments:

```text
@ai-review
```

Selection rules:

- `@id` matches `[[ai_reviews]].id`.
- Unknown ids are ignored.
- Issue, wiki, work item, and other non-MR comments are ignored.
- A single comment may request multiple configured AI reviews.
- The same commit can be triggered again after the current run finishes.

The current implementation does not perform an additional GitLab role check for the comment author. Any user who can comment on the MR and knows a configured `@id` can request that task.

## Active Review Guard

The service keeps an in-process active key:

```text
project_id + mr_iid + commit_sha
```

If the same key is already running, the new request is skipped. For MR note triggers, the service adds an `eyes` emoji to the triggering note and posts an MR-level comment telling the user that the current commit is already being reviewed.

`[server].max_concurrent_reviews` limits total in-process review runs. When the limit is reached, the new request is skipped and the MR receives a queue-busy comment.

The guard is process-local. Multi-instance deployments need a shared lock in SQLite/PostgreSQL or another coordination layer.

## GitLab API Client

Required GitLab REST API calls:

```text
GET /projects/:id/merge_requests/:merge_request_iid/changes
GET /projects/:id/repository/archive.zip
POST /projects/:id/merge_requests/:merge_request_iid/discussions
```

The service uses `[gitlab].token`, not the webhook secret, for API calls. The token needs `api` scope and permission to read MR diffs, download repository archives, and publish MR discussions.

## Diff Parser

The parser reads GitLab MR diffs and builds mappings for changed files, hunks, and added lines. Review findings are only published as line-level comments when they point to an added line in the current MR diff. Findings that cannot be positioned are published as MR-level summaries when appropriate.

## AI Review

AI reviews are configured with `[[ai_reviews]]` in `rules.toml`.

The runner sends MR diff content to an OpenAI-compatible `POST /chat/completions` endpoint. It requests structured `tool_calls` output through `submit_review_findings` and falls back to parsing JSON from assistant content when needed.

The runner provides these built-in read-only context tools by default whenever archive context preparation succeeds:

- `read_file(path)`
- `search_code(query, glob?)`
- `list_files(glob?)`

The service downloads the MR head archive and exposes only repository-local text content through these tools. It does not execute shell commands and rejects or skips unsafe paths such as `.env`, `.git`, absolute paths, and `..` traversal.

If any configured `[archive]` download or extraction limit is exceeded, the service logs a warning and runs the AI Review with diff only. The built-in context tools are unavailable in that fallback, and no archive-limit failure notification is posted. A normal archive still enables all three context tools. Archive timeout, permission, HTTP, corrupt-ZIP, and filesystem errors remain fatal.

A context-assisted execution that ends with exactly one of `review_run_timeout`, `ai_request_timeout`, or `ai_tool_loop_timeout` is discarded and restarted once as an independent diff-only execution. The restart gets a fresh full `timeout_seconds` budget and preserves `request_timeout_seconds`, file batching, and `max_batches`; it has only `submit_review_findings`, no context tools. A timeout or any other failure in the fallback does not recurse. The worst case is close to twice `timeout_seconds`, plus archive preparation, cleanup, and publishing overhead. Non-timeout AI failures and non-limit archive failures are not eligible.

## Comment Builder

Findings are normalized into a common shape:

```rust
struct Finding {
    rule_id: String,
    severity: Severity,
    path: String,
    new_line: Option<u32>,
    title: String,
    message: String,
}
```

The comment builder groups multiple findings on the same file and line, adds stable hidden markers, and falls back to MR-level comments when GitLab cannot position a line-level discussion.

## SQLite Store

The store records:

- review runs
- task runs
- findings
- published comments

Timestamps are stored as RFC3339 UTC strings. The dashboard reads these tables to display summaries, run lists, findings, and audit data.

AI task rows also store nullable `execution_mode`, `fallback_reason`, `context_elapsed_ms`, and `fallback_elapsed_ms`. Null values preserve compatibility with legacy rows. Dashboard run details expose those fields and server-formatted context, fallback, and summed total durations. A successful fallback also degrades visibly in the GitLab Review summary; fallback failure uses the normal AI failure reporting path.

## Work Directory Cleanup

Downloaded archive zip bytes stay in memory and are not written to disk. When repository context is needed, the archive is extracted under `work/`:

- AI context tools: `work/ai_review_context/.../<review_run_id>/source`

After completion or failure, AI Review deletes the current context run directory.

The service cleans stale work directories older than 24 hours on startup and repeats cleanup hourly while running. Cleanup failures are logged as warnings and do not block reviews.

## Configuration

Service configuration lives in `config.toml`:

```toml
[server]
bind = "0.0.0.0:8080"
webhook_secret = "change-me"
max_concurrent_reviews = 4

[gitlab]
base_url = "https://gitlab.example.com"
token = "<your-gitlab-token>"

[storage]
database_url = "sqlite://gitlab-work-runner.db"

[rules]
file = "rules.toml"

[dashboard]
bind = "127.0.0.1:8082"

[archive]
max_archive_bytes = 104857600
max_extracted_files = 10000
max_extracted_bytes = 209715200
max_single_file_bytes = 10485760
max_entry_path_bytes = 512
```

Review configuration lives in `rules.toml`; see `rules.example.toml` for a complete example.

## Error Handling

- Invalid webhook token: `401 Unauthorized`.
- Unsupported or irrelevant event: accepted and ignored.
- MR event: accepted and ignored with `merge_request_events_manual_triggers_only`.
- Missing required payload fields: `400 Bad Request`.
- Duplicate in-flight commit: skipped with an MR-level status comment for note triggers.
- Queue busy: skipped with an MR-level queue-busy comment.
- GitLab API failure, non-limit archive failure, diff processing failure, or internal fatal error: review run fails and posts an MR-level failure comment when possible.
- Archive safety limit: log a warning and continue AI Review with diff only.
- Single AI review failure while other AI reviews can continue: a partial failure summary is posted before the run finishes.

## Dashboard

The dashboard is a separate binary. It reads the same SQLite database and exposes:

```text
GET /api/summary
GET /api/finding-summary
GET /api/runs
GET /api/runs/<review_run_id>
GET /api/projects
GET /api/merge-requests
GET /api/findings
GET /api/comments
```

The dashboard does not run migrations; start the runner once first when using a new database.

## Future Work

- Shared active-review locks for multi-instance deployments.
- Optional polling compensation for missed webhooks.
- More AI provider adapters.
- Richer MR-level summaries.
- Permission checks or allowlists for manual command users.
