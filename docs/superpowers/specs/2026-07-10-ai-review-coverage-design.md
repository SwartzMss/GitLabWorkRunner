# AI Review Coverage Design

## Goal

Prevent batched AI reviews from silently presenting partial work as full coverage. Coverage is persisted for Dashboard inspection only and never appears in GitLab comments.

## Scope

This change covers AI reviews with `batch_review = true`. Non-batched reviews and historical task runs report coverage as unavailable. Oversized single-file diffs are truncated using the existing byte limit; hunk splitting is out of scope.

## Batch Planning

The batch planner scans every `GitLabChange` before applying `max_batches`. It computes:

- `required_batches`: batches required for all changes without `max_batches`.
- `planned_batches`: batches scheduled for the model, equal to `min(required_batches, max_batches)`.
- `completed_batches`: planned batches that successfully returned from the model.

The plan contains only the first `planned_batches` for execution, while retaining aggregate coverage and incomplete-file details for the complete change set.

```rust
struct AiReviewBatchPlan {
    batches: Vec<Vec<GitLabChange>>,
    coverage: ReviewCoverage,
    incomplete_files: Vec<ReviewCoverageFile>,
}

struct ReviewCoverage {
    total_files: usize,
    fully_reviewed_files: usize,
    partially_reviewed_files: usize,
    unreviewed_files: usize,
    total_diff_bytes: usize,
    reviewed_diff_bytes: usize,
    required_batches: usize,
    planned_batches: usize,
    completed_batches: usize,
    complete: bool,
}
```

Coverage is complete only when no file is partial or unreviewed and every planned batch completed.

## Payload Accounting

The planner and prompt builder share one formatter:

```rust
struct FormattedChangePayload {
    content: String,
    total_payload_bytes: usize,
    diff_start: usize,
    diff_end: usize,
}
```

`total_payload_bytes` includes the path, file status, diff header, and diff. The planner uses it for batch boundaries. `diff_start..diff_end` identifies only the original `GitLabChange.diff` bytes in `content`.

Prompt truncation occurs at a UTF-8 boundary. The reviewed diff byte count is the overlap between the included content range and `diff_start..diff_end`; payload metadata is excluded. This makes `total_diff_bytes` and `reviewed_diff_bytes` consistently represent raw GitLab diff UTF-8 bytes.

A change whose complete payload exceeds `max_batch_diff_bytes` occupies one batch and is truncated by the prompt builder. It is recorded as:

```text
status = partial
reason = single_file_diff_truncated
```

Files outside `planned_batches` are recorded as:

```text
status = unreviewed
reason = max_batches_reached
reviewed_diff_bytes = 0
```

If a planned batch fails, execution stops. Files in that batch and later planned batches that were not successfully reviewed are recorded as:

```text
status = unreviewed
reason = batch_execution_failed
reviewed_diff_bytes = 0
```

This reason is distinct from the configuration limit because those files were planned but the model request did not complete.

## Execution Result

The AI execution boundary returns findings or an error together with optional coverage. Batch execution starts with `completed_batches = 0` and increments it only after each successful batch. Service code persists the final state on clean, finding, and failed outcomes before continuing its existing comment or failure handling.

For non-batched reviews, coverage is absent rather than synthesized from the legacy `max_diff_bytes` behavior.

## Storage

`review_task_runs` receives nullable aggregate columns:

```text
coverage_total_files
coverage_fully_reviewed_files
coverage_partially_reviewed_files
coverage_unreviewed_files
coverage_total_diff_bytes
coverage_reviewed_diff_bytes
coverage_required_batches
coverage_planned_batches
coverage_completed_batches
coverage_complete
```

`NULL` means coverage was not recorded. Zero remains a valid measured value.

Only incomplete files are stored in a child table:

```sql
create table review_coverage_files (
    id integer primary key autoincrement,
    review_run_id text not null,
    task_type text not null,
    task_id text not null,
    path text not null,
    status text not null,
    reason text not null,
    total_diff_bytes integer not null,
    reviewed_diff_bytes integer not null,
    created_at text not null
);

create index review_coverage_files_run_task
on review_coverage_files(review_run_id, task_type, task_id);
```

Aggregate columns and incomplete-file rows are replaced atomically in one transaction when a task finishes.

## GitLab Behavior

Coverage never appears in GitLab comments.

- Clean reviews retain the existing clean comment verbatim.
- Reviews with findings retain the existing line-level comments and do not add an MR-level coverage comment.
- Failed reviews retain the existing failure behavior.

## Dashboard

Coverage appears under each AI task in run detail. It shows:

- Status: complete, partial, or unavailable.
- Files: fully reviewed, partially reviewed, unreviewed, and total.
- Diff: reviewed bytes, total bytes, and percentage.
- Batches: completed, planned, and required.

An incomplete-file table shows path, status, localized reason, reviewed diff bytes, and total diff bytes. Reasons are presented as:

- `max_batches_reached`: batch limit reached.
- `single_file_diff_truncated`: single-file diff exceeded the batch limit.
- `batch_execution_failed`: batch execution failed.

Historical and non-batched tasks show coverage as unavailable. Dashboard summary metrics remain unchanged.

## Verification

Tests cover full change scanning, required/planned/completed batch semantics, payload metadata exclusion, UTF-8 truncation accounting, oversized files, max-batch omissions, failed batch persistence, atomic storage, Dashboard API/rendering, and unchanged GitLab comments.
