# AI Timeout Diff-Only Fallback Design

## Goal

When a context-assisted AI review exhausts an AI timeout, run one independent diff-only review so the merge request can still receive useful findings. The fallback has its own full timeout budget, remains transparent to GitLab and Dashboard users, and requires no new configuration.

## Trigger Boundary

The independent fallback is eligible only when a context-assisted AI execution fails with one of these structured codes:

- `review_run_timeout`;
- `ai_request_timeout`;
- `ai_tool_loop_timeout`.

Eligibility is based on `ReviewErrorCode`, never rendered error text.

The fallback does not apply to permission failures, non-timeout HTTP failures, response parsing failures, internal or filesystem errors, GitLab API failures, or non-limit archive failures. Existing archive-limit fallback remains supported through the same execution metadata model.

A timeout fallback is attempted only when repository context was successfully prepared and the first execution therefore ran in context-assisted mode. If a review already started in diff-only mode because an archive limit was exceeded, a later AI timeout fails normally and cannot recursively trigger another fallback.

## Independent Execution Model

The context-assisted execution receives the complete configured `timeout_seconds`. If it fails with an eligible timeout, its incomplete findings and coverage are discarded and its repository context work directory is released before fallback begins.

The fallback is a new AI execution with another complete `timeout_seconds` budget. It starts from the first diff batch and:

- uses no `read_file`, `search_code`, or `list_files` tools;
- rejects and does not echo hallucinated context-tool calls;
- does not run `second_pass_on_clean`;
- retains existing diff batching;
- retains `max_batch_diff_bytes` and `max_batches` semantics;
- retains per-request `request_timeout_seconds` and the existing HTTP retry policy;
- can run only once.

The worst-case AI task duration can therefore approach twice `timeout_seconds`, plus archive preparation and small orchestration overhead. This is intentional: the two executions do not share a deadline.

The fallback prompt includes a trusted instruction stating that the context-assisted review timed out and that final findings must be based only on the supplied MR diff. The untrusted MR diff boundary and all existing output/safety rules remain unchanged.

If fallback succeeds, only fallback findings and fallback coverage are published and persisted as the final review result. If fallback fails, the task fails with the fallback error; the original timeout remains available in structured logs and fallback metadata but is not substituted for the final error.

## Archive-Limit Integration

Archive-limit fallback continues to run diff-only without first invoking a context-assisted AI request. It uses:

```text
execution_mode = diff_only_fallback
fallback_reason = archive_limit_exceeded
```

For archive-limit fallback, `context_elapsed_ms` measures the failed archive preparation attempt and `fallback_elapsed_ms` measures the diff-only AI execution. Non-limit archive failures remain fatal.

## Timing and Persistence

Add nullable columns to `review_task_runs`:

```text
execution_mode text
fallback_reason text
context_elapsed_ms integer
fallback_elapsed_ms integer
```

Values are:

- `execution_mode`: `context` or `diff_only_fallback`;
- `fallback_reason`: `archive_limit_exceeded`, `review_run_timeout`, `ai_request_timeout`, or `ai_tool_loop_timeout`;
- `context_elapsed_ms`: archive preparation plus the context-assisted AI execution, or only the failed archive preparation for archive-limit fallback;
- `fallback_elapsed_ms`: the independent diff-only execution.

Normal context-assisted success records `execution_mode = context`, `fallback_reason = NULL`, its elapsed time in `context_elapsed_ms`, and `fallback_elapsed_ms = NULL`.

The AI task total shown by the Dashboard is calculated as:

```text
context_elapsed_ms + fallback_elapsed_ms
```

The review-run duration remains `finished_at - started_at` and can be longer because it also includes fetching MR changes, publishing GitLab discussions, storage work, and other selected reviews.

Schema migration is additive. Historical rows keep `NULL` values and remain readable. Unknown historical execution modes or fallback reasons render as their stored raw values rather than breaking Dashboard details.

## Dashboard

AI task details display:

- execution mode;
- fallback reason when present;
- context phase duration;
- diff-only phase duration when present;
- calculated AI task total.

Chinese labels map the known reasons to:

- `archive_limit_exceeded`: Archive 超出安全限制;
- `review_run_timeout`: Context Review 整體超時;
- `ai_request_timeout`: AI 請求超時;
- `ai_tool_loop_timeout`: Context tool loop 超時.

The existing coverage display represents the final successful execution. For a successful timeout fallback, this means diff-only fallback coverage, not partial coverage from the abandoned context execution.

## GitLab Summary

The final GitLab summary remains successful when fallback succeeds and includes one concise line per degraded AI review. For example:

```text
`ai-review` 的 Context-assisted Review 超時，已使用 Diff-only 模式完成。Context 40 分 00 秒，Diff-only 6 分 26 秒。
```

Archive-limit fallback uses corresponding wording that identifies the archive safety limit. The summary does not expose internal stack traces or replace normal finding comments.

If fallback fails, the existing partial/full failure summary path is used. It must not claim that Diff-only completed successfully.

## Observability

Structured logs identify:

- the first execution mode;
- the eligible timeout code;
- context elapsed milliseconds;
- fallback start and completion;
- fallback elapsed milliseconds;
- final execution mode and outcome.

Logs must distinguish a context timeout from a fallback timeout and must make clear that fallback is never recursive.

## Testing

Tests cover:

- each of the three eligible timeout codes starts one independent diff-only fallback;
- permission, parse, non-timeout HTTP, non-limit archive, and internal errors do not start fallback;
- archive-limit fallback records the shared metadata shape;
- fallback receives a new complete `timeout_seconds` deadline;
- fallback starts again from batch one, retains batching, and obeys `max_batches`;
- fallback exposes no context tools and rejects hallucinated context calls;
- fallback does not run `second_pass_on_clean`;
- a fallback timeout fails without starting a third execution;
- abandoned context findings and partial coverage are not published as final results;
- context work is released before independent fallback;
- additive storage migration and legacy `NULL` rows;
- Dashboard known and unknown mode/reason rendering and duration totals;
- GitLab success summary degradation notes and failure-summary accuracy;
- full formatting, Clippy, unit, integration, and end-to-end suites.

## Success Criteria

A context-assisted AI review that fails with an eligible AI timeout can complete through one independent, fully budgeted diff-only execution without new configuration. The fallback remains batched and bounded by existing diff limits, never uses context tools or a clean confirmation pass, records phase timing and reason metadata, and is disclosed in both Dashboard and the GitLab final summary.
