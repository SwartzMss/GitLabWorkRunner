# Structured Review Errors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist stable Review failure codes with original messages and display run-level and task-level failures in Dashboard run details.

**Architecture:** Review errors acquire structured `ReviewFailure` metadata at their source. Storage keeps nullable code/message pairs at request and task levels, while Dashboard detail queries expose UTF-8-safe 4 KiB previews and render localized labels. Existing status values, summary metrics, and GitLab comments remain unchanged.

**Tech Stack:** Rust, thiserror, Tokio, SQLx/SQLite, Axum, embedded HTML/JavaScript, Cargo tests.

---

### Task 1: Structured Error Model

**Files:**
- Modify: `src/app/error.rs`
- Test: `src/app/error.rs`

- [ ] **Step 1: Write failing code and display tests**

Add tests asserting every `ReviewErrorCode::as_str()` value is stable and `ReviewFailure` displays only its message. Assert `AppError::review_failure()` returns structured metadata for Review variants and `None` for infrastructure variants.

- [ ] **Step 2: Verify RED**

Run: `cargo test app::error::tests`

Expected: compilation fails because `ReviewErrorCode`, `ReviewFailure`, and accessors do not exist.

- [ ] **Step 3: Implement the model with compatibility constructors**

Add the enum, `as_str`, `ReviewFailure`, and `AppError` helpers `ai_review`, `gitlab`, `archive`, and `script_task`. Convert the four Review variants to contain `ReviewFailure`; keep non-Review variants unchanged.

- [ ] **Step 4: Migrate existing constructors to explicit default codes**

Update existing call sites so the project compiles. Use source-appropriate general codes (`ai_request_failed`, `gitlab_api_failed`, `archive_extract_failed`, `script_task_failed`) without parsing messages.

- [ ] **Step 5: Verify GREEN**

Run: `cargo test app::error::tests && cargo check --all-targets --all-features`

Expected: tests pass and all targets compile.

### Task 2: Classify Failure Sources

**Files:**
- Modify: `src/gitlab/client.rs`
- Modify: `src/review/ai.rs`
- Modify: `src/review/ai_http.rs`
- Modify: `src/review/scripts.rs`
- Modify: `src/review/service.rs`
- Test: `src/gitlab/client.rs`
- Test: `src/review/ai.rs`
- Test: `src/review/scripts.rs`
- Test: `tests/e2e_review.rs`

- [ ] **Step 1: Write failing source-classification tests**

Extend existing timeout, HTTP status, archive limit, response parse, overall deadline, and script failure tests to assert `ReviewErrorCode`, not rendered strings alone. Add a tool follow-up timeout test that asserts `ai_tool_loop_timeout`.

- [ ] **Step 2: Verify RED**

Run the named GitLab, AI, archive, and script tests.

Expected: assertions fail because general compatibility codes are returned.

- [ ] **Step 3: Classify at each source**

Use explicit constructors at timeout/status/parse/limit boundaries. HTTP 401/403 maps to `permission_denied`; total AI deadline maps to `review_run_timeout`; tool follow-up deadline maps to `ai_tool_loop_timeout`.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test gitlab::client::tests && cargo test review::ai::tests && cargo test review::scripts::tests && cargo test --test e2e_review`

Expected: all classification tests pass.

### Task 3: Persist Run and Task Errors

**Files:**
- Modify: `src/storage/mod.rs`
- Modify: `src/review/service.rs`
- Modify: `src/app/server.rs`
- Test: `src/storage/mod.rs`
- Test: `src/app/server_tests.rs`
- Test: `tests/e2e_review.rs`

- [ ] **Step 1: Write failing migration and persistence tests**

Assert `review_requests.error_code/error` and `review_task_runs.error_code` exist. Test successful completion clears stale values, failed task finalization writes both values, and a top-level GitLab failure is stored on the Review request.

- [ ] **Step 2: Verify RED**

Run: `cargo test storage::tests && cargo test app::server::tests`

Expected: schema and persistence assertions fail.

- [ ] **Step 3: Add nullable columns and DTO fields**

Extend migrations with `ensure_column`. Add optional `ReviewFailure` references to request/task finish DTOs or dedicated finish methods. Reset stale fields on start and clear them on successful finish.

- [ ] **Step 4: Wire service and server finalization**

Task failures use `err.review_failure()` and top-level Review failures persist the same metadata before existing notification behavior continues.

- [ ] **Step 5: Verify GREEN**

Run: `cargo test storage::tests && cargo test app::server::tests && cargo test --test e2e_review`

Expected: persistence and behavior tests pass.

### Task 4: Dashboard Detail API and UI

**Files:**
- Modify: `src/dashboard/queries.rs`
- Modify: `src/dashboard/views.rs`
- Modify: `src/dashboard/server.rs`
- Test: `src/dashboard/server.rs`
- Test: `src/dashboard/views.rs`

- [ ] **Step 1: Write failing Dashboard tests**

Create run-level, task-level, legacy unclassified, unknown-code, HTML-special-character, and multibyte messages. Assert detail API returns at most 4096 UTF-8 bytes and the HTML template contains localized mappings and escaped rendering paths.

- [ ] **Step 2: Verify RED**

Run: `cargo test dashboard::`

Expected: detail fields and rendering assertions fail.

- [ ] **Step 3: Extend detail-only queries**

Add `error_code/error` to `DashboardRun` only for detail or introduce `DashboardRunDetailFailure`; extend tasks with `error_code`. Truncate in Rust with a shared UTF-8 boundary helper after fetching full SQLite values. Do not add messages to list queries.

- [ ] **Step 4: Render failures before Coverage**

Add localized labels for known codes, generic labels for unknown codes, and `未分类错误` for legacy rows. Show escaped run error and task error previews; keep task status visible and render Coverage below task failures.

- [ ] **Step 5: Verify GREEN**

Run: `cargo test dashboard::`

Expected: API and view tests pass.

### Task 5: Documentation and Full Verification

**Files:**
- Modify: `README.md`
- Modify: `README.en.md`

- [ ] **Step 1: Document structured errors**

Explain that Dashboard run details show stable failure codes and bounded message previews, while full messages remain in SQLite/logs and GitLab comment behavior is unchanged.

- [ ] **Step 2: Run exact CI commands**

Run: `cargo fmt --all -- --check`

Run: `cargo clippy --all-targets --all-features -- -D warnings`

Run: `cargo test --all-features --locked`

Expected: all commands pass with no warnings or failures.

- [ ] **Step 3: Verify scope and commit**

Run: `git diff --check && git status --short`

Expected: no whitespace errors and only intended files changed.

Commit implementation and documentation with a focused feature message.
