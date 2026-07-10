# Structured Review Errors Design

## Goal

Classify Review failures with stable error codes while retaining the original error message. Show both run-level and task-level failures in Dashboard Review run details without changing GitLab comment behavior.

## Error Model

Each persisted failure contains exactly two user-facing values:

```rust
struct ReviewFailure {
    code: ReviewErrorCode,
    message: String,
}
```

`ReviewErrorCode` serializes to a stable snake_case string:

```rust
enum ReviewErrorCode {
    GitLabApiTimeout,
    GitLabApiFailed,
    ArchiveDownloadTimeout,
    ArchiveDownloadFailed,
    ArchiveExtractFailed,
    ArchiveLimitExceeded,
    AiRequestTimeout,
    AiRequestFailed,
    AiToolLoopTimeout,
    AiResponseParseFailed,
    ReviewRunTimeout,
    PermissionDenied,
    InvalidConfiguration,
    ScriptTaskFailed,
    Internal,
}
```

Task status remains `running`, `completed`, or `failed`. Error classification is not encoded in status.

## Classification Boundary

Errors are classified where they originate. The implementation must not parse rendered error strings to infer an error code.

Review-related `AppError` variants carry a `ReviewFailure`:

```rust
enum AppError {
    AiReview(ReviewFailure),
    GitLab(ReviewFailure),
    Archive(ReviewFailure),
    ScriptTask(ReviewFailure),
    // Existing non-review infrastructure variants remain available.
}
```

Associated constructors keep call sites concise and require an explicit code. `ReviewFailure` implements `Display` using its message so existing logs and notifications retain readable text.

Initial mappings are:

| Failure source | Code |
|---|---|
| GitLab request deadline | `gitlab_api_timeout` |
| GitLab non-timeout request failure | `gitlab_api_failed` |
| GitLab or AI HTTP 401/403 | `permission_denied` |
| Archive download deadline | `archive_download_timeout` |
| Archive download transport failure | `archive_download_failed` |
| Archive configured limit exceeded | `archive_limit_exceeded` |
| ZIP corruption, unsafe entry, or extraction IO | `archive_extract_failed` |
| One Chat Completions request deadline | `ai_request_timeout` |
| AI HTTP/transport failure | `ai_request_failed` |
| Tool follow-up request deadline | `ai_tool_loop_timeout` |
| AI JSON/tool arguments/schema parse failure | `ai_response_parse_failed` |
| Overall AI Review deadline | `review_run_timeout` |
| Invalid review configuration | `invalid_configuration` |
| Script process failure or unexpected exit | `script_task_failed` |
| Unclassified review failure | `internal` |

Tool-loop timeout is assigned only within the tool follow-up phase. Overall deadline exhaustion remains `review_run_timeout`, including when it interrupts a batch.

## Persistence

Add nullable columns to both levels:

```text
review_requests.error_code text null
review_requests.error text null

review_task_runs.error_code text null
review_task_runs.error text null  # existing column retained
```

`review_requests` stores failures that prevent or abort the overall run before a task can own them, such as fetching MR changes. `review_task_runs` stores failures owned by an AI Review or script task.

Successful completion clears both fields. Starting a reused run/task also clears stale error fields.

Existing rows remain valid:

- `error_code IS NULL` and `error IS NULL`: no persisted failure.
- `error_code IS NULL` and `error IS NOT NULL`: legacy unclassified failure.
- New failures write both columns.

The full error message is retained in SQLite. Review request finalization gains an optional structured failure so top-level failures are not discarded.

## Non-Fatal Events

GitLab comment publication currently logs an individual failure and continues. It does not change the Review or task status and is not stored as the primary failure. A future warning/event table may track these events separately.

Archive extraction currently has no time deadline. Extraction failures are classified as `archive_extract_failed` or `archive_limit_exceeded`, never `archive_extract_timeout`.

## Dashboard API

Run detail exposes nullable run-level fields:

```text
run.error_code
run.error
```

Each task exposes its existing `error` plus new `error_code`.

The API returns at most 4 KiB of each error message, truncated at a UTF-8 boundary. The database retains the full text. List endpoints do not add error messages, avoiding payload growth and accidental exposure outside detail views.

## Dashboard UI

Review run detail shows a run-level failure section only when `run.error` exists. It includes:

- Localized error label.
- Stable snake_case error code.
- Escaped error message preview.

Each task is rendered with its title and status. A failed task shows the same error fields before its Coverage data. This allows partial Coverage and the failure cause to appear together.

Legacy errors with no code use the label `未分类错误` and display no fabricated code. Unknown future codes use a generic localized label while preserving the raw code.

Dashboard summary metrics and GitLab comments remain unchanged.

## Testing

Tests cover:

- Stable `ReviewErrorCode` serialization and localized Dashboard labels.
- Classification at GitLab, archive, AI request, AI tool-loop, response parsing, overall deadline, and script boundaries.
- SQLite migration and clearing/writing run-level and task-level errors.
- Legacy rows with an error but no code.
- UTF-8-safe 4 KiB Dashboard preview truncation.
- Run-level and task-level failure rendering with HTML escaping.
- Failed batch details showing both structured error and partial Coverage.
- Existing GitLab comment behavior and non-fatal comment failures.
