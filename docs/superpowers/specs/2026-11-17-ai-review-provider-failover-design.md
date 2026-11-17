# AI Review Primary/Backup Provider Failover Design

> Status: Proposed design
> Related issue: #50
> Scope: one optional backup provider for each existing AI Review task

## 1. Problem statement

Each current `[[ai_reviews]]` entry has one OpenAI-compatible endpoint, token, and model. The HTTP layer retries selected failures, but all retries still target the same endpoint. The service also has a one-shot Context → Diff-only timeout fallback, which likewise uses the same configured provider.

A second `[[ai_reviews]]` entry must not be used as the backup: `ReviewService::run_selected_ai_reviews()` treats each entry as an independent task, with separate task state and comment publication.

The intended change is provider failover inside one existing AI Review task. It must not change the task id, review selection, finding format, or GitLab publishing contract.

## 2. Goals

- Keep the existing provider as Primary and preserve current behavior when no backup is configured.
- Allow one optional Backup with an independent base URL, API token, and model name.
- Switch only after a classified provider failure, not after arbitrary review errors.
- Retry only the failed batch, then pin the remaining batches to Backup for that review.
- Preserve Context tools, Diff-only JSON mode, batch coverage, and the existing one-shot timeout fallback.
- Expose the model actually used and the failover reason in logs and task details.
- Keep credentials out of logs and persisted metadata.

## 3. Non-goals

- Multiple backup providers or a general provider registry.
- Load balancing, model quality routing, or per-batch model selection.
- Background health checks, automatic primary recovery probes, or cross-review circuit breakers.
- Changing the current OpenAI-compatible Chat Completions protocol.
- Changing the existing review timeout, batch limits, prompt policy, or tool budgets.
- Replacing the current Context → Diff-only fallback.

Each new review starts with Primary. Provider selection is sticky only for the lifetime of that review execution.

## 4. Configuration

Keep the current top-level Primary fields unchanged. Add an optional nested `fallback` table:

```toml
[[ai_reviews]]
enabled = true
id = "ai-review"
title = "AI Review"

# Primary
base_url = "https://primary-provider.example/v1"
api_key = "<primary-token>"
model = "primary-model"

# These remain review-level settings and are shared by both providers.
timeout_seconds = 4800
request_timeout_seconds = 420
max_batch_diff_bytes = 25000
max_batches = 0

# Optional Backup. Omitting this table disables provider failover.
[ai_reviews.fallback]
base_url = "https://backup-provider.example/v1"
api_key = "<backup-token>"
model = "backup-model"
```

### Configuration model

- Add an `AiReviewProviderConfig` containing only `base_url`, `api_key`, and `model`.
- Add `fallback: Option<AiReviewProviderConfig>` to `AiReviewConfig`.
- The existing `base_url`, `api_key`, and `model` fields remain the Primary configuration, avoiding a breaking rewrite of current `rules.toml` files.
- The fallback table is optional; its presence enables one Backup. No second review task or separate task id is created.
- Both provider configurations use the existing `api_key` convention. Environment-variable secret resolution is not introduced in this change. Neither token may appear in logs, errors, task metadata, or dashboard output.
- Validate a configured fallback at ruleset load time: base URL, API key, and model must be non-empty after trimming. Unknown fallback keys remain rejected.
- Review-level limits and timeouts are shared. A provider-specific timeout override is deliberately out of scope for v1.

## 5. Runtime architecture

### 5.1 Provider state

At the beginning of `run_batched_ai_review()`, construct an execution-local provider state:

- `primary`: immutable configured Primary.
- `backup`: optional configured Backup.
- `active`: initially Primary.
- `failover_used`: initially false.
- `failover_reason`: initially absent.

This state is local to one invocation. It is not shared across tasks, workers, or later MR reviews.

The request-building and HTTP layers receive the active provider's endpoint, token, and model while continuing to receive the parent review configuration for prompt policy, tool budgets, batching, and task identity. A derived effective config may be used to minimize changes to existing call sites, but it must not recursively retain an enabled fallback when executing the Backup.

### 5.2 Per-batch failover

Keep the current sequential batch plan and the current per-provider HTTP retry count.

For each batch:

1. Execute the batch with the active provider.
2. If it succeeds, append its findings and continue.
3. If it fails with a failover-eligible provider error, Backup exists, and failover has not already been used:
   - mark Backup as active;
   - record the typed failure reason;
   - rebuild a fresh conversation from the same batch prompt;
   - retry that same batch with Backup, using the same source directory, context tools, runtime instruction, output mode, and remaining overall review deadline.
4. If Backup succeeds, append only Backup's findings and continue all remaining batches with Backup.
5. If Backup fails, return the failure. Do not switch back to Primary and do not attempt a third provider.

The primary attempt and backup attempt must be wrapped by the same remaining review deadline. Failover must not reset the overall Context execution deadline. The existing per-request timeout and retry behavior remains in force independently for each provider.

A fresh conversation is required for the retried batch. Do not transfer a partial tool-call transcript from Primary to Backup because providers may differ in tool-call behavior and response compatibility.

This per-batch placement is important: if Primary completes batches 1–3 and fails on batch 4, only batch 4 is repeated. Batches 1–3 must not be sent to Backup again, and their findings remain in the in-memory result exactly once.

### 5.3 Interaction with Context and Diff-only fallback

The existing service-level fallback remains responsible for repository archive limits and timeout-driven Context → Diff-only degradation.

- Archive preparation failure due to the existing archive-limit condition is not a provider failure. Continue to Diff-only with Primary, as today. The Diff-only execution can still switch to Backup if its own provider request fails.
- If Primary fails at the provider layer during Context execution, retry the failed batch on Backup. If successful, later Context batches remain on Backup.
- If Backup is active and the Context execution later reaches the existing timeout fallback condition, the service must run Diff-only with Backup. It must not silently restart Diff-only on Primary.
- If no Backup exists, the current Context → Diff-only path remains unchanged.
- The Diff-only fallback remains one-shot and non-recursive. A failed Backup Diff-only attempt ends the AI Review task; there is no third provider or second Diff-only attempt.

Carry the active provider role/model out of `AiReviewExecution` (or an equivalent internal result wrapper) so `timeout_fallback_for_execution()` can select the same provider for the Diff-only retry. Do not carry credentials into persisted metadata.

## 6. Failure classification

Failover decisions must use typed/structured failure information. Do not parse rendered error messages to infer HTTP status or retryability.

### Eligible for the one-time provider switch

| Failure | Behavior |
|---|---|
| Request timeout / HTTP 408 / HTTP 504 | Keep current same-provider retry policy, then switch to Backup |
| HTTP 429 | Keep current same-provider retry policy, then switch to Backup |
| HTTP 500 / 502 / 503 | Keep bounded same-provider retry policy, then switch to Backup |
| Connect, DNS, transport, or response-body read failure | Keep bounded same-provider retry policy, then switch to Backup |
| HTTP 401 / 403 | Switch to Backup without retrying the rejected Primary credential |
| Context tool follow-up request timeout | Switch to Backup and restart the failed batch with a fresh context conversation |

Introduce an explicit provider-failure classification at the HTTP/error boundary. A stable review error code such as `ai_provider_unavailable` may represent transient HTTP/transport failures; existing `permission_denied` and `ai_request_timeout` codes can represent auth and timeout classes. The exact internal enum should avoid changing unrelated GitLab/script error semantics.

### Not eligible

- HTTP 400 / 422 or unsupported request parameters after the existing tool-call-to-JSON compatibility path.
- Model response JSON/schema parse failures or invalid findings.
- Prompt construction or request serialization failures.
- Local repository-context tool implementation failures, archive extraction/limit failures, storage failures, or GitLab failures.
- The overall `ReviewRunTimeout` by itself; it is a review deadline, not evidence that the provider is unavailable.
- Any failure after Backup has already become active.

If a failure is not classified as provider-related, return it through the existing error and timeout-fallback paths without changing providers.

## 7. Metadata, logs, and Dashboard

Keep the existing Context/Diff-only metadata separate from provider-failover metadata. Do not overload `fallback_reason`, which currently describes Context → Diff-only behavior.

Extend `AiReviewExecutionMetadata` and `review_task_runs` with nullable provider fields:

- `primary_model`: configured Primary model.
- `actual_model`: model used by the final successful execution, or active model on failure.
- `provider_fallback_reason`: typed reason for switching to Backup; null means no provider switch.

The Dashboard task detail should show the actual model and, when applicable, that Backup was used with a concise reason. Historical rows remain valid with null provider metadata. Add the fields through the existing additive `StateStore::migrate()` path and update Dashboard projections/schema checks accordingly.

Logs should include:
- provider role (`primary` or `backup`);
- model;
- batch index/count;
- HTTP status or typed failure class;
- failover transition and reason.

Never log `api_key`, authorization headers, or raw request headers. Avoid including provider credentials in error messages.

## 8. Compatibility

- Existing `rules.toml` files without `fallback` deserialize and execute exactly as before.
- Existing AI Review ids and task rows are unchanged.
- The same finding filtering, coverage accounting, comment marker, and GitLab publishing path are used.
- The SQLite migration is additive and idempotent; old rows receive null provider metadata.
- No new dependency or provider SDK is required.

## 9. Test strategy

### Configuration
- Parse a valid nested fallback provider.
- Parse legacy configuration without fallback.
- Reject empty fallback URL, token, or model.
- Reject unknown nested fallback fields.
- Verify that the backup does not create a second AI Review task.

### HTTP classification
- Verify eligible statuses (408, 429, 500, 502, 503, 504) and transport failures.
- Verify 401/403 switch without repeating the same rejected credential.
- Verify 400/422 and malformed model responses do not switch.
- Verify secrets are absent from logs and metadata.

### Batch behavior
- Primary success: Backup receives zero requests.
- Primary failure on batch 1: Backup retries batch 1 and handles all remaining batches.
- Primary success on batches 1–3 then failure on batch 4: Backup receives batch 4 onward only.
- Backup failure: no switch back to Primary and no third attempt.
- The same overall Context deadline applies across Primary and Backup attempts.

### Context / Diff-only behavior
- Context failover to Backup retains the extracted repository context.
- Backup Context timeout enters one-shot Diff-only using Backup.
- Archive-limit fallback still starts Diff-only on Primary and can fail over if the provider itself fails.
- No-backup behavior passes all existing Context/Diff-only regression tests.

### Persistence / Dashboard
- Provider metadata is stored on both successful and failed task completion.
- Legacy database migration is additive and idempotent.
- Dashboard detail renders primary/actual model and provider fallback reason; null historical values render normally.

## 10. Implementation boundaries

Expected code areas:

- `src/review/rules.rs`: nested fallback provider configuration and validation.
- `src/review/ai.rs`: active provider state, typed failover at the batch boundary, active provider result metadata.
- `src/review/ai_http.rs`: typed HTTP/transport failure classification and provider-safe logging.
- `src/review/service.rs`: carry active provider into the existing Diff-only timeout fallback.
- `src/storage/mod.rs`: additive task metadata migration and persistence.
- `src/dashboard/queries.rs` and Dashboard rendering: expose and display provider metadata.
- `rules.example.toml`, README/docs, and unit/e2e tests.

This document is the design contract for #50. Implementation should be reviewed against the acceptance criteria before merge.
