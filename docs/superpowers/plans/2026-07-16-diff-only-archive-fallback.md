# Diff-Only Archive Fallback and Script Support Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Continue AI reviews from MR diffs when repository archive limits are exceeded, while completely removing script-task support.

**Architecture:** Move archive limit/extraction code out of the script runner into a focused AI-context archive module. Treat only structured `ArchiveLimitExceeded` failures as a successful no-context outcome; all other context preparation errors remain fatal. Remove script selection and execution from the runtime while retaining legacy database columns/rows for non-destructive historical compatibility.

**Tech Stack:** Rust 2021, Tokio, serde/TOML, ureq, zip, SQLx/SQLite, Axum, Cargo tests.

---

## File Structure

- Create `src/review/archive.rs`: archive limits, safe ZIP extraction, and their focused unit tests.
- Delete `src/review/scripts.rs`: remove subprocess script execution and its tests after archive code is moved.
- Modify `src/review/service.rs`: remove script orchestration and add structured diff-only context fallback.
- Modify `src/review/rules.rs`: remove script rule models/selectors and reject unknown top-level rule keys.
- Modify `src/review/mod.rs`, `src/lib.rs`, `src/app/config.rs`, `src/dashboard/config.rs`: replace script-module archive imports and remove the public script API.
- Modify `src/app/error.rs`: remove the script-only error variant/code.
- Modify `src/app/server.rs`, `src/app/server_tests.rs`, `tests/e2e_review.rs`: make manual triggers AI-only and verify fallback behavior.
- Modify `src/storage/mod.rs`, `src/dashboard/queries.rs`, `src/dashboard/server.rs`, `src/dashboard/views.rs`: stop creating/displaying script selection state while retaining legacy database storage.
- Modify `src/review/work_cleanup.rs`: remove cleanup for script work directories.
- Delete `examples/scripts/check_todo_tbd.py`; modify `rules.example.toml`, `README.md`, `README.en.md`, `docs/design.md`, `docs/gitlab-webhook.md`, and `CHANGELOG.md` to describe the breaking removal and archive fallback.

### Task 1: Isolate Archive Handling From the Script Runner

**Files:**
- Create: `src/review/archive.rs`
- Modify: `src/review/mod.rs`
- Modify: `src/app/config.rs`
- Modify: `src/dashboard/config.rs`
- Modify: `src/review/service.rs`
- Test: `src/review/archive.rs`

- [ ] **Step 1: Add failing archive-module tests**

Create `src/review/archive.rs` initially with tests that describe the public boundary. Move the existing ZIP fixture helpers from `src/review/scripts.rs` and add these assertions:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn archive_with_entry(name: &str, content: &[u8]) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut bytes);
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content).unwrap();
            zip.finish().unwrap();
        }
        bytes.into_inner()
    }

    fn permissive_limits() -> ArchiveLimits {
        ArchiveLimits {
            max_archive_bytes: usize::MAX,
            max_extracted_files: usize::MAX,
            max_extracted_bytes: usize::MAX,
            max_single_file_bytes: usize::MAX,
            max_entry_path_bytes: usize::MAX,
        }
    }

    #[test]
    fn extraction_reports_structured_archive_limit_error() {
        let archive = archive_with_entry("repo/src/lib.rs", b"12345");
        let limits = ArchiveLimits {
            max_extracted_bytes: 4,
            ..permissive_limits()
        };
        let temp = tempfile::tempdir().unwrap();

        let err = extract_zip_archive(&archive, temp.path(), &limits).unwrap_err();

        assert_eq!(
            err.review_failure().map(|failure| failure.code),
            Some(ReviewErrorCode::ArchiveLimitExceeded)
        );
    }
}
```

- [ ] **Step 2: Run the focused test and verify it fails**

Run: `cargo test review::archive::tests::extraction_reports_structured_archive_limit_error`

Expected: FAIL to compile because `review::archive`, `ArchiveLimits`, and `extract_zip_archive` are not defined at the new boundary.

- [ ] **Step 3: Move archive-only code into the new module**

Move `ArchiveLimits`, its serde defaults, `extract_zip_archive`, `copy_zip_file_with_limits`, and archive-only helpers from `src/review/scripts.rs` into `src/review/archive.rs`. Preserve the existing limit checks exactly. Export the module and update imports:

```rust
// src/review/mod.rs
pub(crate) mod archive;
pub mod ai;
// ...existing modules...
pub mod rules;
pub mod scripts;
```

```rust
// src/app/config.rs and src/dashboard/config.rs
use crate::review::archive::ArchiveLimits;
```

```rust
// src/review/service.rs
use crate::review::archive::{extract_zip_archive, ArchiveLimits};
use crate::review::scripts::{
    ScriptTaskContext, ScriptTaskResult, ScriptTaskRunner, ScriptTaskStatus,
};
```

Leave script execution temporarily compiling against `crate::review::archive` so this commit is behavior-neutral.

- [ ] **Step 4: Run archive and full tests**

Run: `cargo test review::archive && cargo test`

Expected: all archive tests and the full suite PASS; no runtime behavior changes.

- [ ] **Step 5: Commit the module extraction**

```bash
git add src/review/archive.rs src/review/scripts.rs src/review/mod.rs src/review/service.rs src/app/config.rs src/dashboard/config.rs
git commit -m "refactor: isolate repository archive handling"
```

### Task 2: Fall Back to Diff-Only AI Review on Archive Limits

**Files:**
- Modify: `src/review/service.rs`
- Test: `src/review/service.rs`
- Test: `tests/e2e_review.rs`

- [ ] **Step 1: Add failing structured-fallback unit tests**

Extract the classification into a small pure helper and test both sides before implementing it:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_limit_is_eligible_for_diff_only_fallback() {
        let err = AppError::archive(
            ReviewErrorCode::ArchiveLimitExceeded,
            "repository archive download exceeded max_archive_bytes 10",
        );
        assert!(is_archive_limit_error(&err));
    }

    #[test]
    fn archive_timeout_is_not_eligible_for_diff_only_fallback() {
        let err = AppError::gitlab(
            ReviewErrorCode::ArchiveDownloadTimeout,
            "archive download timed out",
        );
        assert!(!is_archive_limit_error(&err));
    }
}
```

- [ ] **Step 2: Run the focused tests and verify they fail**

Run: `cargo test review::service::tests::archive_limit_`

Expected: FAIL because `is_archive_limit_error` does not exist.

- [ ] **Step 3: Implement classification and fallback at context preparation**

Add the pure classifier:

```rust
fn is_archive_limit_error(error: &AppError) -> bool {
    error
        .review_failure()
        .is_some_and(|failure| failure.code == ReviewErrorCode::ArchiveLimitExceeded)
}
```

Change `prepare_ai_review_context` so the existing archive download/extract sequence lives in an inner method, while the outer method converts only limit errors to `Ok(None)`:

```rust
async fn prepare_ai_review_context(
    &self,
    review: &AiReviewConfig,
    changes: &crate::gitlab::MergeRequestChanges,
    event: &MergeRequestEvent,
) -> AppResult<Option<AiReviewContextWorkDir>> {
    match self
        .prepare_ai_review_context_inner(review, changes, event)
        .await
    {
        Ok(context) => Ok(Some(context)),
        Err(err) if is_archive_limit_error(&err) => {
            warn!(
                project_id = event.project_id,
                mr_iid = event.mr_iid,
                commit_sha = %event.commit_sha,
                ai_review_id = %review.id,
                error_code = ReviewErrorCode::ArchiveLimitExceeded.as_str(),
                error = %err,
                "repository archive exceeded a safety limit; continuing AI review in diff-only mode"
            );
            Ok(None)
        }
        Err(err) => Err(err),
    }
}
```

Keep `run_ai_review_with_optional_clean_second_pass` preparing context once and passing the same `source_dir` to both calls. Do not catch errors around AI execution itself.

- [ ] **Step 4: Add an end-to-end fallback test**

Reuse the existing mock GitLab and AI servers in `tests/e2e_review.rs`. Configure `ArchiveLimits { max_archive_bytes: 1, ..ArchiveLimits::default() }`, return a ZIP larger than one byte, and capture the AI request. Assert:

```rust
assert_eq!(summary.findings, 0);
assert_eq!(ai_requests.load(Ordering::SeqCst), 1);
assert!(!request_body.contains("read_file"));
assert!(!request_body.contains("search_code"));
assert!(!request_body.contains("list_files"));
assert!(posted_notes.iter().all(|note| !note.contains("archive_limit_exceeded")));
```

Add a paired test returning an archive timeout/error and assert the review still fails with `archive_download_timeout` or `archive_download_failed`.

- [ ] **Step 5: Run fallback tests and the AI review suite**

Run: `cargo test archive_limit -- --nocapture && cargo test ai_review -- --nocapture`

Expected: PASS. The limit case completes diff-only; the non-limit case remains failed.

- [ ] **Step 6: Commit the fallback**

```bash
git add src/review/service.rs tests/e2e_review.rs
git commit -m "feat: fall back to diff-only review on archive limits"
```

### Task 3: Remove Script Rules, Triggers, and Execution

**Files:**
- Modify: `src/review/rules.rs`
- Modify: `src/review/service.rs`
- Modify: `src/app/server.rs`
- Modify: `src/app/error.rs`
- Modify: `src/review/mod.rs`
- Modify: `src/lib.rs`
- Delete: `src/review/scripts.rs`
- Test: `src/review/rules.rs`
- Test: `src/app/server_tests.rs`
- Test: `tests/e2e_review.rs`

- [ ] **Step 1: Replace script-selection tests with strict configuration tests**

Add `#[serde(deny_unknown_fields)]` to `RulesFile`, delete script selection tests, and first add this failing test:

```rust
#[test]
fn removed_script_tasks_are_rejected() {
    let err = Ruleset::from_toml(
        r#"
[[script_tasks]]
id = "legacy-script"
title = "Legacy"
command = "python check.py"
"#,
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown field `script_tasks`"));
}
```

- [ ] **Step 2: Run the strict configuration test and verify it fails**

Run: `cargo test review::rules::tests::removed_script_tasks_are_rejected`

Expected: FAIL because `script_tasks` is still a valid field.

- [ ] **Step 3: Remove script rule and error APIs**

Delete `ScriptTaskConfig`, `CompiledScriptTask`, script fields and selectors from `RulesFile`/`Ruleset`, and `default_script_timeout_seconds`. The resulting top-level shape is:

```rust
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesFile {
    #[serde(default)]
    pub ai_review: AiReviewPromptConfig,
    #[serde(default)]
    pub ai_reviews: Vec<AiReviewConfig>,
}
```

Delete `ReviewErrorCode::ScriptTaskFailed`, `AppError::ScriptTask`, and `AppError::script_task`; keep `review_failure` matching the remaining structured variants.

- [ ] **Step 4: Make manual review selection AI-only**

Rename `manual_script_task_ids` to `manual_review_ids` because it parses generic standalone `@id` tokens. In `src/app/server.rs`, select only `ruleset.ai_reviews_by_ids(&requested_ids)`. In `src/review/service.rs`, remove the `tasks` parameter and all `ScriptTaskRunSummary` combination logic:

```rust
let requested_ids = manual_review_ids(&event.note);
let ai_reviews = self.ruleset.ai_reviews_by_ids(&requested_ids);
if ai_reviews.is_empty() {
    return Ok(ReviewSummary {
        skipped: true,
        findings: 0,
        comments: 0,
    });
}
```

The inner execution becomes AI-only:

```rust
let ai_result = self
    .run_selected_ai_reviews(&mr_event, &changes, ai_reviews, review_request.as_deref())
    .await?;
let summary_comments = self
    .publish_manual_review_summary(
        &mr_event,
        &changes,
        &ai_result,
        review_request.as_deref(),
        started.elapsed(),
    )
    .await?;
let findings = ai_result.findings;
let comments = ai_result.comments + summary_comments;
```

Delete script result parsing/publishing helpers and their tests.

- [ ] **Step 5: Delete the script module and public export**

Delete `src/review/scripts.rs`, remove `pub mod scripts` from `src/review/mod.rs`, and delete this compatibility export from `src/lib.rs`:

```rust
pub mod script_tasks {
    pub use crate::review::scripts::*;
}
```

- [ ] **Step 6: Update server and E2E tests**

Remove script-only fixtures/tests. Add a server test proving a comment containing only `@legacy-script` is accepted but does not enter the review queue when no AI review has that ID. Retain parser tests for punctuation and standalone-token behavior under the new `manual_review_ids` name.

- [ ] **Step 7: Run focused and full tests**

Run: `cargo test review::rules && cargo test app::server && cargo test --test e2e_review && cargo test`

Expected: PASS, with no script subprocess executed and strict rules rejection covered.

- [ ] **Step 8: Commit runtime script removal**

```bash
git add src/review/rules.rs src/review/service.rs src/app/server.rs src/app/error.rs src/review/mod.rs src/lib.rs src/app/server_tests.rs tests/e2e_review.rs
git add -u src/review/scripts.rs
git commit -m "refactor: remove script task runtime support"
```

### Task 4: Remove Script State From Current Storage and Dashboard Surfaces

**Files:**
- Modify: `src/storage/mod.rs`
- Modify: `src/dashboard/queries.rs`
- Modify: `src/dashboard/server.rs`
- Modify: `src/dashboard/views.rs`
- Modify: `src/review/work_cleanup.rs`
- Test: `src/storage/mod.rs`
- Test: `src/dashboard/queries.rs`
- Test: `src/dashboard/server.rs`
- Test: `src/review/work_cleanup.rs`

- [ ] **Step 1: Add a historical compatibility test**

In a `src/dashboard/queries.rs` module test, create a temporary database with the current schema, insert a legacy review row with `selected_script_tasks = 2` directly through SQL, construct `DashboardStore { pool }`, then call its existing run-detail API and assert the run remains readable after the Rust dashboard model drops that field:

```rust
sqlx::query(
    "insert into review_requests
     (review_run_id, trigger_type, project_id, mr_iid, commit_sha,
      requested_ids_json, selected_ai_reviews, selected_script_tasks,
      status, findings, comments, timezone, started_at)
     values ('legacy-script-run', 'manual_note', 1, 2, 'abc', '[]', 0, 2,
             'completed', 0, 0, 'UTC', '2026-07-16T00:00:00Z')",
)
.execute(&pool)
.await
.unwrap();

let store = DashboardStore { pool };
let run = store
    .run_detail("legacy-script-run")
    .await
    .unwrap()
    .unwrap();
assert_eq!(run.run.review_run_id, "legacy-script-run");
```

- [ ] **Step 2: Run the compatibility test before model cleanup**

Run: `cargo test legacy_script_run_remains_readable`

Expected: PASS as a baseline, proving the persisted schema can remain untouched.

- [ ] **Step 3: Stop exposing current script selection state**

Remove `selected_script_tasks` from `ReviewRequestStart` and dashboard response structs/SQL projections. Keep the SQLite `review_runs.selected_script_tasks integer not null` column, and hardcode zero only inside the insert statement:

```sql
insert into review_runs
(review_run_id, trigger_type, project_id, project_name,
 project_path_with_namespace, mr_iid, commit_sha, note_id,
 requested_ids_json, selected_ai_reviews, selected_script_tasks,
 status, findings, comments, timezone, started_at)
values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 'running', 0, 0, ?, ?)
```

Remove the script failure localization and change the findings label from `AI 与脚本解析出的结果` to `AI Review 解析出的结果`. Do not drop database columns or delete historical task rows.

- [ ] **Step 4: Remove script work cleanup**

Delete `SCRIPT_TASK_ROOT`, `cleanup_stale_script_sources`, `is_script_task_source_leaf`, and their tests. Simplify startup cleanup:

```rust
pub(crate) fn cleanup_stale_review_work() -> AppResult<usize> {
    let removed =
        cleanup_stale_ai_context_work(Path::new(AI_CONTEXT_ROOT), DEFAULT_STALE_WORK_TTL)?;
    if removed > 0 {
        info!(removed_ai_context_dirs = removed, "stale review work directories cleaned");
    }
    Ok(removed)
}
```

- [ ] **Step 5: Run storage, dashboard, and cleanup tests**

Run: `cargo test storage && cargo test dashboard && cargo test work_cleanup`

Expected: PASS. The API no longer advertises script selections, and the legacy row remains readable.

- [ ] **Step 6: Commit state and UI cleanup**

```bash
git add src/storage/mod.rs src/dashboard/queries.rs src/dashboard/server.rs src/dashboard/views.rs src/review/work_cleanup.rs
git commit -m "refactor: remove script task product surfaces"
```

### Task 5: Remove Script Assets and Update Current Documentation

**Files:**
- Delete: `examples/scripts/check_todo_tbd.py`
- Modify: `rules.example.toml`
- Modify: `README.md`
- Modify: `README.en.md`
- Modify: `docs/design.md`
- Modify: `docs/gitlab-webhook.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Remove active script documentation and example assets**

Delete the `[[script_tasks]]` block from `rules.example.toml` and delete `examples/scripts/check_todo_tbd.py`. Remove current-capability script sections, flowchart nodes, work-directory descriptions, trigger instructions, and links from both READMEs and current docs. Historical changelog entries remain as history; add a new top entry instead of rewriting old releases.

- [ ] **Step 2: Document the breaking removal and fallback**

Add an unreleased changelog section:

```markdown
## Unreleased

- 移除本地脚本任务支持；`[[script_tasks]]` 配置现在会被拒绝，请从 `rules.toml` 删除。
- Repository archive 触发任一 `[archive]` 安全限制时，AI Review 会自动降级为仅使用 MR diff，不再让整次 Review 因 `archive_limit_exceeded` 失败。
- Archive timeout、权限、HTTP、ZIP 损坏和文件系统错误仍会让对应 AI Review 失败。
```

Update the archive-limit paragraph in both READMEs. The Chinese version must state:

```markdown
AI Review 下载或解压 repository archive 时如果触发任一 `[archive]` 限制，会记录 WARN 并自动降级为仅使用 MR diff；本次 Review 不提供 `read_file`、`search_code` 或 `list_files`。其他 Archive 错误仍按失败处理。
```

Provide the equivalent statement in `README.en.md`.

- [ ] **Step 3: Verify no active script references remain**

Run:

```bash
rg -n "script_task|ScriptTask|\[\[script_tasks\]\]|work/script_tasks|脚本任务" src tests README.md README.en.md config.example.toml rules.example.toml docs/design.md docs/gitlab-webhook.md examples
```

Expected: no matches. Matches in historical `CHANGELOG.md` and archived `docs/superpowers/` design/plan files are allowed.

- [ ] **Step 4: Run formatting and the complete verification suite**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`

Expected: all commands exit 0 with no warnings or failed tests.

- [ ] **Step 5: Commit documentation and assets**

```bash
git add rules.example.toml README.md README.en.md docs/design.md docs/gitlab-webhook.md CHANGELOG.md
git add -u examples/scripts/check_todo_tbd.py
git commit -m "docs: describe diff-only archive fallback"
```

### Task 6: Final Requirement Verification

**Files:**
- Verify only; modify the responsible file if a check exposes a defect.

- [ ] **Step 1: Verify the complete spec-to-code behavior**

Run: `cargo test --all-targets --all-features`

Expected: PASS.

- [ ] **Step 2: Verify production-quality static checks**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS with no formatting diff and no Clippy warnings.

- [ ] **Step 3: Verify removals and repository state**

Run:

```bash
rg -n "pub mod script_tasks|ScriptTaskFailed|AppError::ScriptTask|\[\[script_tasks\]\]|work/script_tasks" src tests README.md README.en.md config.example.toml rules.example.toml docs/design.md docs/gitlab-webhook.md examples
git status --short
git log -6 --oneline
```

Expected: `rg` has no matches; `git status --short` is empty; the recent log contains the focused implementation commits from Tasks 1–5.

- [ ] **Step 4: Review runtime semantics manually**

Confirm from the tests and code that:

- only `ReviewErrorCode::ArchiveLimitExceeded` becomes `Ok(None)` context;
- successful archives still provide context tools;
- the second pass reuses the first context outcome;
- no GitLab failure notification is produced solely for fallback;
- legacy database rows are not deleted or migrated destructively;
- no new review can configure, select, trigger, or execute a script task.
