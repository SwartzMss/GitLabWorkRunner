# AI Timeout Diff-Only Fallback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run one independently timed, batched diff-only AI review after an eligible context-assisted AI timeout, and disclose its reason and phase timings in storage, Dashboard, and GitLab summaries.

**Architecture:** Introduce a small execution-metadata value that travels with `AiReviewExecution`, persist it atomically with the final task result, and render it in both user surfaces. The service owns the one-shot fallback decision: only structured AI timeout codes from a context-assisted execution qualify; the fallback calls the existing batched engine with `source_dir = None`, a trusted fallback instruction, and clean confirmation disabled.

**Tech Stack:** Rust 2021, Tokio deadlines, serde/TOML, SQLx/SQLite additive migrations, Axum E2E mocks, embedded Dashboard HTML/JavaScript.

---

## File Structure

- Modify `src/review/ai.rs`: add execution metadata, trusted fallback prompt input, and tests for independent deadline/batching/tool behavior.
- Modify `src/review/service.rs`: measure phases, classify eligible timeout codes, perform a single independent fallback, release context first, and feed summary/storage metadata.
- Modify `src/storage/mod.rs`: add nullable execution metadata columns and atomically store them with task completion/coverage.
- Modify `src/dashboard/queries.rs`, `src/dashboard/server.rs`, `src/dashboard/views.rs`: expose and render mode, reason, phase durations, totals, and legacy/unknown values.
- Modify `tests/e2e_review.rs`: verify timeout fallback, non-recursion, batching, coverage replacement, and GitLab summary notes.
- Modify `README.md`, `README.en.md`, `docs/design.md`, `CHANGELOG.md`: document independent timeout semantics and maximum duration.

### Task 1: Add Execution Metadata and Additive Storage

**Files:**
- Modify: `src/review/ai.rs`
- Modify: `src/storage/mod.rs`
- Test: `src/storage/mod.rs`

- [ ] **Step 1: Add failing migration and round-trip tests**

Add a storage test that migrates an in-memory database, finishes an AI task with fallback metadata, and queries the four new columns:

```rust
let metadata = AiReviewExecutionMetadata {
    execution_mode: AiReviewExecutionMode::DiffOnlyFallback,
    fallback_reason: Some(AiReviewFallbackReason::AiToolLoopTimeout),
    context_elapsed_ms: Some(2_400_000),
    fallback_elapsed_ms: Some(386_000),
};

store.finish_task_run_with_coverage(
    &finish,
    Some(&coverage),
    &files,
    Some(&metadata),
).await.unwrap();

assert_eq!(row.get::<String, _>("execution_mode"), "diff_only_fallback");
assert_eq!(row.get::<String, _>("fallback_reason"), "ai_tool_loop_timeout");
assert_eq!(row.get::<i64, _>("context_elapsed_ms"), 2_400_000);
assert_eq!(row.get::<i64, _>("fallback_elapsed_ms"), 386_000);
```

Add a migration test proving an existing task row keeps all four fields `NULL`.

- [ ] **Step 2: Run the focused storage tests and verify RED**

Run: `cargo test storage::tests::records_ai_execution_metadata`

Expected: FAIL because the metadata types, columns, and storage argument do not exist.

- [ ] **Step 3: Define stable metadata types**

In `src/review/ai.rs`, add types whose database strings match the spec:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiReviewExecutionMode {
    Context,
    DiffOnlyFallback,
}

impl AiReviewExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::DiffOnlyFallback => "diff_only_fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiReviewFallbackReason {
    ArchiveLimitExceeded,
    ReviewRunTimeout,
    AiRequestTimeout,
    AiToolLoopTimeout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiReviewExecutionMetadata {
    pub execution_mode: AiReviewExecutionMode,
    pub fallback_reason: Option<AiReviewFallbackReason>,
    pub context_elapsed_ms: Option<u64>,
    pub fallback_elapsed_ms: Option<u64>,
}
```

Give `AiReviewFallbackReason` an `as_str()` matching `ReviewErrorCode` strings.

- [ ] **Step 4: Add additive columns and atomic writes**

Extend the `review_task_runs` create/migrate path with nullable columns:

```sql
execution_mode text,
fallback_reason text,
context_elapsed_ms integer,
fallback_elapsed_ms integer
```

Update the existing atomic finish/coverage transaction to write metadata in the same `UPDATE review_task_runs` statement. Bind `NULL` when metadata is absent and use saturating `u64 -> i64` conversion.

- [ ] **Step 5: Run storage tests and commit**

Run: `cargo test storage`

Expected: PASS, including legacy `NULL` and metadata round trips.

```bash
git add src/review/ai.rs src/storage/mod.rs
git commit -m "feat: store AI fallback execution metadata"
```

### Task 2: Implement One-Shot Independent Timeout Fallback

**Files:**
- Modify: `src/review/ai.rs`
- Modify: `src/review/service.rs`
- Test: `src/review/ai.rs`
- Test: `src/review/service.rs`

- [ ] **Step 1: Add failing timeout classification tests**

Add table-driven service tests:

```rust
for code in [
    ReviewErrorCode::ReviewRunTimeout,
    ReviewErrorCode::AiRequestTimeout,
    ReviewErrorCode::AiToolLoopTimeout,
] {
    let err = AppError::ai_review(code, "timed out");
    assert_eq!(timeout_fallback_reason(&err).map(|reason| reason.as_str()), Some(code.as_str()));
}

for code in [
    ReviewErrorCode::PermissionDenied,
    ReviewErrorCode::AiRequestFailed,
    ReviewErrorCode::AiResponseParseFailed,
] {
    assert!(timeout_fallback_reason(&AppError::ai_review(code, "failed")).is_none());
}
```

- [ ] **Step 2: Run classification tests and verify RED**

Run: `cargo test review::service::tests::timeout_fallback_`

Expected: FAIL because `timeout_fallback_reason` does not exist.

- [ ] **Step 3: Add a trusted fallback instruction without mutating administrator config**

Extend the internal AI execution entry point with an optional trusted runtime instruction, separate from MR-provided `review_request`:

```rust
pub(crate) async fn run_ai_review_execution_with_context_and_instruction(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
    source_dir: Option<&Path>,
    review_request: Option<&str>,
    runtime_instruction: Option<&str>,
) -> AiReviewExecution
```

The existing public/internal wrapper passes `None`. Prompt tests must prove the fallback instruction appears inside the trusted system/admin section and not inside the untrusted diff or reviewer-preference boundary.

- [ ] **Step 4: Return metadata with every service execution**

Introduce a service-level result that keeps AI output and metadata together:

```rust
struct TimedAiReviewExecution {
    execution: AiReviewExecution,
    metadata: AiReviewExecutionMetadata,
}
```

Measure elapsed milliseconds with `Instant`. For successful normal context execution, record `Context` and context elapsed. For archive-limit `None` context, record `DiffOnlyFallback`, `ArchiveLimitExceeded`, archive preparation elapsed as context time, and AI execution elapsed as fallback time.

- [ ] **Step 5: Implement the one-shot timeout branch**

After a context-assisted execution returns an eligible timeout:

```rust
let fallback_reason = timeout_fallback_reason(
    execution.result.as_ref().expect_err("eligible fallback requires failure")
);
drop(context);

let fallback_started = Instant::now();
let fallback = run_ai_review_execution_with_context_and_instruction(
    &fallback_review_config(review),
    &changes.changes,
    None,
    review_request,
    Some("Context-assisted review timed out. Complete this fallback review using only the supplied MR diff."),
).await;
```

`fallback_review_config` clones the review and sets only `second_pass_on_clean = false`. Do not call the fallback decision function around this second execution, which makes recursion impossible. The existing batched engine supplies a new full `timeout_seconds` deadline and preserves `max_batches`.

Discard first-execution findings, coverage, and incomplete files. Return only fallback execution data with context/fallback phase timings.

- [ ] **Step 6: Add behavioral unit tests**

Using local mock servers or a narrow injected test seam, prove:

- each eligible timeout starts exactly one fallback;
- fallback starts at batch one and observes `max_batches`;
- source-unavailable requests omit context tools;
- fallback ignores `second_pass_on_clean`;
- fallback timeout does not start a third execution;
- non-eligible errors return without fallback;
- the context guard is dropped before the fallback request.

- [ ] **Step 7: Run focused AI/service tests and commit**

Run: `cargo test review::ai && cargo test review::service`

Expected: PASS.

```bash
git add src/review/ai.rs src/review/service.rs
git commit -m "feat: retry AI timeouts with diff-only review"
```

### Task 3: Persist Final Metadata and Add GitLab Degradation Notes

**Files:**
- Modify: `src/review/service.rs`
- Test: `src/review/service.rs`
- Test: `tests/e2e_review.rs`

- [ ] **Step 1: Add failing summary-format tests**

Build an `AiReviewRunSummary` containing one degraded item and assert the final body includes the review ID, reason, and both formatted durations:

```rust
assert!(body.contains("`ai-review`"));
assert!(body.contains("已使用 Diff-only 模式完成"));
assert!(body.contains("Context 40 分 00 秒"));
assert!(body.contains("Diff-only 6 分 26 秒"));
```

Add a failure case proving a failed fallback never emits the success wording.

- [ ] **Step 2: Run summary tests and verify RED**

Run: `cargo test review::service::tests::manual_review_summary_`

Expected: FAIL because degradation items are not represented.

- [ ] **Step 3: Carry degradation items through orchestration**

Add:

```rust
struct AiReviewFallbackSummary {
    id: String,
    reason: AiReviewFallbackReason,
    context_elapsed_ms: u64,
    fallback_elapsed_ms: u64,
}
```

On successful fallback, append one item. Pass metadata to `finish_ai_task_run` so storage completion, final coverage replacement, and metadata are atomic. On fallback failure, persist metadata with the fallback error but do not append a success item.

- [ ] **Step 4: Render concise GitLab notes**

Extend `build_manual_review_summary_body` with a degradation section. Use a shared duration formatter that handles seconds/minutes/hours and escapes review IDs. Map archive-limit and the three timeout reasons to user-facing Chinese text. Keep normal summaries unchanged when no fallback occurred.

- [ ] **Step 5: Add end-to-end timeout fallback coverage**

Create an E2E mock where context-assisted execution deterministically reaches `ai_tool_loop_timeout`, then a source-free fallback succeeds across multiple diff batches. Inspect request bodies to assert context tools are absent in fallback, `max_batches` is obeyed, and no clean second pass occurs. Assert:

- only fallback findings/comments publish;
- stored coverage is fallback coverage;
- stored execution metadata/timings are populated;
- final GitLab summary contains the degradation note;
- a second fallback timeout creates no third execution.

- [ ] **Step 6: Run service/E2E tests and commit**

Run: `cargo test review::service && cargo test --test e2e_review`

Expected: PASS.

```bash
git add src/review/service.rs tests/e2e_review.rs
git commit -m "feat: report diff-only timeout fallback"
```

### Task 4: Expose Fallback Metadata in Dashboard

**Files:**
- Modify: `src/dashboard/queries.rs`
- Modify: `src/dashboard/server.rs`
- Modify: `src/dashboard/views.rs`
- Test: `src/dashboard/queries.rs`
- Test: `src/dashboard/server.rs`
- Test: `src/dashboard/views.rs`

- [ ] **Step 1: Add failing query/render tests**

Insert a task with known and unknown execution metadata. Assert detail JSON returns the four stored fields. Add view tests that require:

```text
執行模式 / Diff-only 降級
降級原因 / Context tool loop 超時
Context / 40 分 00 秒
Diff-only / 6 分 26 秒
AI 合計 / 46 分 26 秒
```

Unknown mode/reason values must be escaped and rendered raw. Legacy `NULL` rows must retain the current UI without empty metadata rows.

- [ ] **Step 2: Run dashboard tests and verify RED**

Run: `cargo test dashboard`

Expected: FAIL because models, projections, and rendering do not expose metadata.

- [ ] **Step 3: Extend Dashboard models and SQL mapping**

Add nullable fields to `DashboardTaskRun`, every task SELECT projection, and `row_to_task`. Do not make schema checks reject databases before the additive migration has run; the runner migration remains the source of schema upgrades.

- [ ] **Step 4: Render metadata and calculated duration**

Add escaped mappings for known mode/reason values. Calculate total with null-safe integer addition only when at least one phase exists. Keep the existing coverage and generic legacy-task branches intact.

- [ ] **Step 5: Run Dashboard/full tests and commit**

Run: `cargo test dashboard && cargo test --all-targets --all-features`

Expected: PASS.

```bash
git add src/dashboard/queries.rs src/dashboard/server.rs src/dashboard/views.rs
git commit -m "feat: show AI fallback details in dashboard"
```

### Task 5: Documentation and Final Verification

**Files:**
- Modify: `README.md`
- Modify: `README.en.md`
- Modify: `docs/design.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Document independent timeout behavior**

State that the three AI timeout codes can start one independent diff-only execution with another complete `timeout_seconds`, so worst-case AI duration approaches twice the configured value. Document batching/`max_batches`, no tools, no second pass, non-recursion, Dashboard timing, and GitLab summary disclosure. Do not add a configuration key.

- [ ] **Step 2: Verify semantic and documentation searches**

Run:

```bash
rg -n "context_timeout|diff_only_fallback|fallback_reason|context_elapsed_ms|fallback_elapsed_ms" src tests README.md README.en.md docs/design.md CHANGELOG.md
```

Expected: runtime, persistence, UI, tests, and docs all have intentional matches.

- [ ] **Step 3: Commit documentation**

```bash
git add README.md README.en.md docs/design.md CHANGELOG.md
git commit -m "docs: describe independent timeout fallback"
```

- [ ] **Step 4: Run complete quality gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
git diff --check
git status --short
```

Expected: all commands exit 0; status is clean after the documentation commit.
