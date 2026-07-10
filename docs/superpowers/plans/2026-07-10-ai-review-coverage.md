# AI Review Coverage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record truthful file, diff-byte, and batch coverage for batched AI reviews and expose it in Dashboard run details without changing GitLab comments.

**Architecture:** A shared formatted-change representation is used by both the greedy batch planner and prompt truncation accounting. AI execution returns its result together with evolving coverage, which the service atomically stores on the task run and its incomplete-file child rows. Dashboard run detail reads and renders these nullable coverage records.

**Tech Stack:** Rust, Tokio, SQLx/SQLite, Axum, embedded HTML/JavaScript, Cargo tests.

---

### Task 1: Shared Payload Formatting and Coverage-Aware Batch Plan

**Files:**
- Modify: `src/review/ai_prompt.rs`
- Modify: `src/review/ai.rs`
- Test: `src/review/ai.rs`
- Test: `src/review/ai_prompt.rs`

- [ ] **Step 1: Write failing formatter and planner tests**

Add tests that assert the formatted payload identifies the exact raw diff byte range, UTF-8 truncation reports only included diff bytes, the planner scans changes beyond `max_batches`, and oversized single-file payloads are partial.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test review::ai::tests::batch_plan review::ai::ai_prompt`

Expected: compilation or assertion failures because formatted payload metadata and coverage plan types do not exist.

- [ ] **Step 3: Add shared payload types and planner**

Implement `FormattedChangePayload`, `ReviewedFilePayload`, `LimitedDiffPayload`, `ReviewCoverage`, `ReviewCoverageFile`, and `AiReviewBatchPlan`. Replace early-return splitting with a complete greedy scan followed by `take(planned_batches)`. Count raw `change.diff.len()` bytes and use the formatted range intersection for truncation.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run: `cargo test review::ai`

Expected: all review AI unit tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/review/ai.rs src/review/ai_prompt.rs
git commit -m "feat: plan AI review coverage"
```

### Task 2: Coverage-Aware Batch Execution

**Files:**
- Modify: `src/review/ai.rs`
- Modify: `src/review/service.rs`
- Test: `tests/e2e_review.rs`

- [ ] **Step 1: Write failing execution tests**

Add an end-to-end test where two batches succeed and later files exceed `max_batches`, asserting required/planned/completed counts and incomplete files. Add a failure test asserting completed batches stop at the last successful request and remaining planned files use `batch_execution_failed`.

- [ ] **Step 2: Run focused tests and verify RED**

Run the two named `cargo test --test e2e_review` cases.

Expected: compilation failures because AI execution still returns only `Vec<Finding>`.

- [ ] **Step 3: Return execution result with coverage**

Introduce an execution envelope containing `AppResult<Vec<Finding>>`, optional `ReviewCoverage`, and incomplete files. Increment `completed_batches` only after a successful batch. On failure, reclassify files in the failed and remaining planned batches as unreviewed with `batch_execution_failed`, recompute aggregates, and return the underlying error with coverage.

- [ ] **Step 4: Preserve second-pass and public behavior**

Update service execution helpers and compatibility entry points so non-batched review behavior remains unchanged and coverage is carried through a clean second pass without creating GitLab coverage comments.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run: `cargo test --test e2e_review ai_review_batches`

Expected: batch execution tests pass and request counts remain correct.

- [ ] **Step 6: Commit**

```bash
git add src/review/ai.rs src/review/service.rs tests/e2e_review.rs
git commit -m "feat: track completed AI review batches"
```

### Task 3: Persist Aggregate and Incomplete-File Coverage

**Files:**
- Modify: `src/storage/mod.rs`
- Modify: `src/review/service.rs`
- Test: `src/storage/mod.rs`
- Test: `tests/e2e_review.rs`

- [ ] **Step 1: Write failing storage tests**

Add a migration assertion for all nullable coverage columns and `review_coverage_files`. Add a write test that finishes an AI task with coverage, then verifies aggregate values and incomplete rows. Repeat the write and assert child rows are replaced rather than duplicated.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test storage::tests`

Expected: compilation or schema assertion failure because coverage storage does not exist.

- [ ] **Step 3: Add migration and atomic write API**

Use `ensure_column` for nullable aggregate columns, create the child table and index, and add borrowed storage DTOs. Implement one transaction that updates the task aggregate, deletes old child rows, inserts current incomplete rows, and commits.

- [ ] **Step 4: Store coverage before outcome handling**

Update the service success and failure paths to pass optional coverage into task finalization. Do not alter `build_ai_review_clean_body` or add any MR-level comment.

- [ ] **Step 5: Run focused and end-to-end tests**

Run: `cargo test storage::tests && cargo test --test e2e_review ai_review`

Expected: storage and AI review end-to-end tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/storage/mod.rs src/review/service.rs tests/e2e_review.rs
git commit -m "feat: persist AI review coverage"
```

### Task 4: Dashboard Coverage API and Run Detail

**Files:**
- Modify: `src/dashboard/queries.rs`
- Modify: `src/dashboard/views.rs`
- Modify: `src/dashboard/server.rs`
- Test: `src/dashboard/server.rs`
- Test: `src/dashboard/views.rs`

- [ ] **Step 1: Write failing Dashboard tests**

Extend run-detail API fixtures with one partial AI task and incomplete files. Assert nullable aggregate fields and incomplete rows serialize correctly. Add HTML contract assertions for coverage labels and localized reason mappings.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test dashboard::`

Expected: API assertions fail because task coverage fields and child rows are absent.

- [ ] **Step 3: Extend Dashboard schema checks and query types**

Require the new table and aggregate columns, add serializable coverage and incomplete-file DTOs, query child rows by run/task, and attach them to each `DashboardTaskRun`.

- [ ] **Step 4: Render task coverage**

Add a task section to run detail. Render complete/partial/unavailable state, file counts, byte percentage, completed/planned/required batches, and an incomplete-file table. Map reasons to `达到批次上限`, `单文件 Diff 超过批次限制`, and `批次执行失败`.

- [ ] **Step 5: Run Dashboard tests and verify GREEN**

Run: `cargo test dashboard::`

Expected: all Dashboard query, API, and HTML tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/dashboard/queries.rs src/dashboard/views.rs src/dashboard/server.rs
git commit -m "feat: show AI review coverage in dashboard"
```

### Task 5: Documentation and Full Verification

**Files:**
- Modify: `README.md`
- Modify: `README.en.md`

- [ ] **Step 1: Document Dashboard-only coverage**

Explain that batched reviews record file/diff/batch coverage in SQLite and Dashboard, including max-batch omissions and oversized-file truncation, while GitLab comments remain unchanged.

- [ ] **Step 2: Run formatting and full test suite**

Run: `cargo fmt --check`

Expected: pass.

Run: `cargo test`

Expected: all unit and integration tests pass.

- [ ] **Step 3: Verify comment text and working tree**

Run: `rg -n "AI Review 完成|未发现高置信度问题" src/review/service.rs tests/e2e_review.rs`

Expected: the existing clean comment remains unchanged and no coverage wording appears in GitLab comment builders.

Run: `git diff --check && git status --short`

Expected: no whitespace errors; only intended documentation changes remain after prior commits.

- [ ] **Step 4: Commit**

```bash
git add README.md README.en.md
git commit -m "docs: explain AI review coverage"
```
