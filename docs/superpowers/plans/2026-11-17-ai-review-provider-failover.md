# AI Review Primary/Backup Provider Failover Implementation Plan

> Design: [AI Review Primary/Backup Provider Failover](../specs/2026-11-17-ai-review-provider-failover-design.md)
> Issue: #50
> Status: Planned; implementation has not started in this design PR.

## Goal

Implement one optional Backup provider per existing AI Review task. Preserve the current review task id, sequential batch plan, Context tools, Diff-only timeout fallback, coverage reporting, and GitLab comment publishing.

## Architecture

Add an optional nested provider configuration to `AiReviewConfig`. Keep the current top-level `base_url`, `api_key`, and `model` as Primary for backward compatibility. Keep all review-level limits shared by Primary and Backup.

The active provider is execution-local. The batch executor starts on Primary, retries a failed batch once on Backup after the existing per-provider retry policy is exhausted, and pins later batches to Backup. The service-level Context → Diff-only fallback must reuse the active provider returned by the Context execution.

Provider failure classification is typed at the HTTP/error boundary. Do not infer eligibility from rendered error strings.

## Work breakdown

### Task 1 — Configuration and validation

Files:
- Modify `src/review/rules.rs`
- Modify `rules.example.toml`
- Update every `AiReviewConfig` struct literal found by repository-wide search.

Steps:
- [ ] Add `AiReviewProviderConfig { base_url, api_key, model }`.
- [ ] Add `fallback: Option<AiReviewProviderConfig>` with serde defaults and strict unknown-field rejection.
- [ ] Validate all configured Backup fields are non-empty after trimming.
- [ ] Add TOML tests for legacy config, valid nested config, invalid empty fields, and unknown nested keys.
- [ ] Verify Backup remains part of one `AiReviewConfig` and does not appear as a second selected review task.

### Task 2 — Typed provider failure classification

Files:
- Modify `src/app/error.rs`
- Modify `src/review/ai_http.rs`
- Modify `src/review/ai.rs`

Steps:
- [ ] Define a typed provider failure class (or dedicated review error code) for transient HTTP/transport failures.
- [ ] Preserve distinct request-timeout and permission-denied classification.
- [ ] Classify 408/429/500/502/503/504 and connect/DNS/transport/body-read failures as failover-eligible after bounded retries.
- [ ] Classify 401/403 as failover-eligible without retrying the same rejected credential.
- [ ] Keep 400/422, response parse errors, local errors, and overall review deadline non-eligible.
- [ ] Add focused unit tests for each class; avoid message-string parsing.

### Task 3 — Execution-local provider state and per-batch switching

Files:
- Modify `src/review/ai.rs`
- Modify `src/review/ai_http.rs` if request structures need provider-specific fields.

Steps:
- [ ] Create execution-local Primary/Backup/Active provider state.
- [ ] Keep the current batch plan and one shared remaining review deadline.
- [ ] Wrap the current batch execution so the first eligible Primary failure switches to Backup and retries only that batch with a fresh conversation.
- [ ] Retain successful earlier batch findings and coverage; do not replay them.
- [ ] Pin all later batches to Backup.
- [ ] Prevent Backup → Primary failback and any third-provider attempt.
- [ ] Return provider role/model/failover reason as non-secret execution metadata.
- [ ] Test primary success, first-batch failover, mid-run failover, sticky backup, backup failure, and deadline behavior.

### Task 4 — Preserve active provider through Context → Diff-only fallback

Files:
- Modify `src/review/service.rs`
- Modify `src/review/ai.rs`

Steps:
- [ ] Carry the active provider role out of the Context execution.
- [ ] When `timeout_fallback_for_execution()` starts the one-shot Diff-only fallback, construct an effective config for the active provider and disable further provider switching for that fallback attempt.
- [ ] If Backup was active, Diff-only must use Backup.
- [ ] Keep archive-limit fallback on Primary unless the Diff-only provider request itself fails.
- [ ] Keep no-backup behavior unchanged.
- [ ] Add service-level tests proving Backup Context timeout falls back to Backup Diff-only and never calls a third provider.

### Task 5 — Persist provider metadata and display it

Files:
- Modify `src/review/ai.rs` metadata types
- Modify `src/storage/mod.rs`
- Modify `src/dashboard/queries.rs` and Dashboard detail rendering
- Update storage/dashboard tests

Steps:
- [ ] Add nullable `primary_model`, `actual_model`, and `provider_fallback_reason` fields to task execution metadata.
- [ ] Add these columns through the existing additive/idempotent `StateStore::migrate()` path.
- [ ] Persist metadata on both successful and failed task completion.
- [ ] Update Dashboard query projections, row mapping, schema checks, and task detail rendering.
- [ ] Verify old databases and historical null metadata remain readable.
- [ ] Verify credentials never enter the database.

### Task 6 — Documentation and regression verification

Files:
- Modify `rules.example.toml`
- Modify `README.md` and `README.en.md` where AI Review configuration is documented
- Add/update unit and e2e tests

Steps:
- [ ] Document the optional nested Backup table and shared timeout/budget behavior.
- [ ] Document failover-eligible and non-eligible failures.
- [ ] Add mock-provider e2e coverage for primary success, provider failover, and Context/Diff-only interaction.
- [ ] Run `cargo fmt --check`.
- [ ] Run `cargo test`.
- [ ] Run the repository's CI workflow and fix regressions before marking the implementation PR ready.

## Review checklist

- [ ] Existing config without Backup is backward-compatible.
- [ ] One review task remains one task; Backup is not a separate `[[ai_reviews]]`.
- [ ] Only the failed batch is replayed.
- [ ] Active provider is sticky for the rest of one review.
- [ ] Diff-only fallback uses the active provider.
- [ ] No provider credentials are logged or persisted.
- [ ] Task details expose actual model and failover reason.
- [ ] Existing comment publishing and coverage semantics are unchanged.
