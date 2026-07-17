# AI Context Tool Budget Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound AI review context exploration so large merge requests finish with verified findings instead of growing the conversation until the upstream gateway times out.

**Architecture:** Keep the existing per-batch review engine, but add a cumulative byte budget and an exact-call cache to each `complete_ai_review_response` invocation. Extend file reads with bounded line ranges, then make the completion loop transition into a one-way finalization state that advertises only `submit_review_findings`; timeout retries use the same finalization path instead of replaying an exploratory request.

**Tech Stack:** Rust, Tokio, Serde/serde_json, TOML, ureq, existing unit and end-to-end mock HTTP tests.

---

## File map

- `src/review/rules.rs`: parse and default the new `max_tool_total_bytes` setting.
- `src/review/ai_tools.rs`: ranged reads, per-call dynamic result cap, canonical cache keys.
- `src/review/ai_prompt.rs`: verification-first tool policy.
- `src/review/ai.rs`: cumulative budget, cache, finalization state, and timeout-aware retry.
- `tests/e2e_review.rs`: observable multi-request behavior through the mock AI endpoint.
- `rules.example.toml`, `README.md`, `README.en.md`: configuration and operational semantics.

### Task 1: Add cumulative tool-byte configuration

**Files:**
- Modify: `src/review/rules.rs`
- Modify: every `AiReviewConfig` literal reported by `rg -n 'AiReviewConfig \\{' src tests`
- Modify: `rules.example.toml`

- [ ] **Step 1: Write failing parser tests**

In `src/review/rules.rs`, extend the existing default and explicit-value tests to assert:

```rust
assert_eq!(reviews[0].max_tool_total_bytes, 40_000);
```

and for a TOML fixture containing `max_tool_total_bytes = 12345`:

```rust
assert_eq!(reviews[0].max_tool_total_bytes, 12_345);
```

- [ ] **Step 2: Run the focused test and verify failure**

Run: `cargo test review::rules::tests --lib`

Expected: compilation fails because `AiReviewConfig` has no `max_tool_total_bytes` field.

- [ ] **Step 3: Implement the setting**

Add to both `AiReviewConfig` and `AiReviewPromptConfig`:

```rust
#[serde(default = "default_ai_max_tool_total_bytes")]
pub max_tool_total_bytes: usize,
```

Add:

```rust
fn default_ai_max_tool_total_bytes() -> usize {
    40_000
}
```

Propagate the global `[ai_review]` value in `Ruleset::from_toml`, include it in `AiReviewPromptConfig::default`, and add `max_tool_total_bytes: 40_000` to test literals. Add this example beside the existing tool settings:

```toml
max_tool_total_bytes = 40000
```

- [ ] **Step 4: Run focused tests**

Run: `cargo test review::rules::tests --lib`

Expected: all rules tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/review/rules.rs src/review/ai.rs src/review/ai_tools.rs src/review/service.rs tests/e2e_review.rs rules.example.toml
git commit -m "feat: configure cumulative AI tool result budget"
```

### Task 2: Make context lookup verification-first

**Files:**
- Modify: `src/review/ai_prompt.rs`

- [ ] **Step 1: Write a failing prompt test**

Add a test that builds `configured_system_prompt` and asserts it contains the concrete-candidate policy:

```rust
assert!(prompt.contains("先根据当前 diff 形成具体的候选缺陷"));
assert!(prompt.contains("没有具体候选缺陷时，不得为了探索仓库而调用上下文工具"));
assert!(prompt.contains("证据仍不足时，放弃该候选缺陷"));
```

- [ ] **Step 2: Verify the test fails**

Run: `cargo test review::ai_prompt::tests --lib`

Expected: the new assertions fail against the current broad mandatory-tool wording.

- [ ] **Step 3: Replace the context-tool policy**

Rewrite section 4 of `SYSTEM_PROMPT` to preserve high-confidence verification while adding these exact rules:

```text
先根据当前 diff 形成具体的候选缺陷，再判断是否缺少确认该候选所必需的仓库上下文。
没有具体候选缺陷时，不得为了探索仓库而调用上下文工具。
当前 diff 已足以确认或排除候选缺陷时，不得调用工具。
优先进行一次精确 search_code，再对命中位置进行一次窄范围 read_file；不得穷举搜索整个仓库来证明保护逻辑不存在。
达到工具预算或证据仍不足时，放弃该候选缺陷，并提交其他已确认 Findings；没有已确认问题时提交空 findings。
```

- [ ] **Step 4: Run prompt and AI unit tests**

Run: `cargo test review::ai_prompt::tests review::ai::tests --lib`

Expected: all selected tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/review/ai_prompt.rs
git commit -m "fix: require a candidate before AI context lookup"
```

### Task 3: Add bounded line-range reads

**Files:**
- Modify: `src/review/ai_tools.rs`

- [ ] **Step 1: Write failing ranged-read tests**

Create a temporary five-line UTF-8 file and test:

```rust
let result = context.read_file(r#"{"path":"src/example.rs","start_line":2,"end_line":4}"#);
assert_eq!(result["start_line"], 2);
assert_eq!(result["end_line"], 4);
assert_eq!(result["content"], "two\nthree\nfour\n");
```

Add tests asserting `ok == false` for line zero, `start_line > end_line`, only one bound supplied, and a range wider than 250 lines.

- [ ] **Step 2: Verify the tests fail**

Run: `cargo test review::ai_tools::tests --lib`

Expected: range metadata is absent and invalid ranges are not rejected.

- [ ] **Step 3: Implement range arguments and reader**

Extend the schema and deserialization struct with:

```rust
#[serde(default)]
start_line: Option<usize>,
#[serde(default)]
end_line: Option<usize>,
```

Add `const READ_MAX_LINES: usize = 250;`. Validate that both bounds are absent or both are present, bounds are one-based and ordered, and the inclusive width is at most 250. Add a helper that reads UTF-8 text, selects the requested inclusive lines, preserves newline separators, applies the byte cap safely at a UTF-8 boundary, and returns actual range metadata.

- [ ] **Step 4: Update tool guidance**

Change `read_file_tool()` so `start_line` and `end_line` are optional integer properties with minimum `1`, and state that narrow line ranges are preferred after `search_code` returns a line number.

- [ ] **Step 5: Run tests**

Run: `cargo test review::ai_tools::tests --lib`

Expected: all tool tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/review/ai_tools.rs
git commit -m "feat: support bounded AI file reads"
```

### Task 4: Add exact-call caching and cumulative byte enforcement

**Files:**
- Modify: `src/review/ai_tools.rs`
- Modify: `src/review/ai.rs`

- [ ] **Step 1: Write failing cache-key tests**

Add a pure helper test proving JSON key order and path separators normalize to one key:

```rust
assert_eq!(
    context_tool_cache_key("read_file", r#"{"path":"src\\lib.rs","end_line":20,"start_line":10}"#),
    context_tool_cache_key("read_file", r#"{"start_line":10,"path":"src/lib.rs","end_line":20}"#),
);
```

- [ ] **Step 2: Write failing completion-loop tests**

Using the existing mock-response helpers in `src/review/ai.rs`, add tests where:

- the model requests the identical call twice and the second response contains `"cached":true` while `tool_calls_used` remains `1`;
- two unique results exceed `max_tool_total_bytes`, the later call receives a structured byte-limit result, and no extra real tool call is counted.

- [ ] **Step 3: Verify focused failure**

Run: `cargo test review::ai::tests review::ai_tools::tests --lib`

Expected: tests fail because no cache or cumulative budget exists.

- [ ] **Step 4: Implement normalized keys**

In `ai_tools.rs`, add a crate-visible helper that parses argument JSON, recursively sorts object keys, normalizes `read_file.path`, and serializes `(tool_name, normalized_arguments)`. Invalid JSON falls back to the original argument string so the real tool can return its normal validation error.

- [ ] **Step 5: Implement per-batch state**

In `complete_ai_review_response`, create:

```rust
let mut tool_cache = std::collections::HashMap::<String, String>::new();
let mut tool_result_bytes_used = 0_usize;
let unlimited_tool_bytes = config.max_tool_total_bytes == 0;
```

For exact cache hits return:

```json
{"ok":true,"cached":true,"message":"Identical context result was already provided earlier in this review batch; reuse that evidence."}
```

For unique calls, cap the result to the smaller of `max_tool_result_bytes` and the remaining cumulative bytes, increment the byte count by the returned string length, and cache the original result. Log `cache_hit`, `tool_result_bytes_used`, and `max_tool_total_bytes`.

- [ ] **Step 6: Run focused tests**

Run: `cargo test review::ai::tests review::ai_tools::tests --lib`

Expected: all selected tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/review/ai.rs src/review/ai_tools.rs
git commit -m "feat: bound and deduplicate AI context results"
```

### Task 5: Introduce deterministic finalization mode

**Files:**
- Modify: `src/review/ai.rs`

- [ ] **Step 1: Write failing request-shape tests**

Add a test that exhausts the call budget and captures the next request body. Deserialize it as `OpenAiChatRequest` or `serde_json::Value` and assert:

```rust
assert_eq!(tool_names, vec!["submit_review_findings"]);
assert!(last_user_message.contains("立即提交最终审查结果"));
```

Add a success case where the next response submits empty findings, and a failure case where it requests context again or omits structured findings.

- [ ] **Step 2: Verify the tests fail**

Run: `cargo test review::ai::tests --lib`

Expected: context tools remain advertised after exhaustion.

- [ ] **Step 3: Parameterize request serialization**

Replace the boolean context-tools argument with an explicit request mode such as:

```rust
enum ContextToolMode {
    Enabled,
    FinalizeOnly,
    Unavailable,
}
```

`FinalizeOnly` and `Unavailable` expose only `submit_review_findings`; only `Enabled` adds the three context tools.

- [ ] **Step 4: Implement one-way finalization**

When count or cumulative bytes are exhausted, append one trusted message:

```text
上下文工具预算已用尽。放弃任何仍缺少证据的候选问题，不得继续请求 read_file、search_code 或 list_files。请立即调用 submit_review_findings 提交最终审查结果；没有已确认问题时提交空 findings。
```

Serialize the next request in `FinalizeOnly` mode. Permit exactly one finalization response; if it does not submit findings, return an explicit `AiRequestFailed` error without another tool loop.

- [ ] **Step 5: Run tests**

Run: `cargo test review::ai::tests --lib`

Expected: finalization success and refusal behavior both pass.

- [ ] **Step 6: Commit**

```bash
git add src/review/ai.rs
git commit -m "fix: force AI review finalization at tool limits"
```

### Task 6: Make context timeout retries finalize

**Files:**
- Modify: `src/review/ai.rs`
- Modify: `tests/e2e_review.rs`

- [ ] **Step 1: Write a failing end-to-end test**

Extend the mock server so a context follow-up returns 504 and captures the retry. Assert the first follow-up exposes context tools, while the retry:

```rust
assert_eq!(retry_tool_names, vec!["submit_review_findings"]);
assert!(retry_last_message.contains("立即提交最终审查结果"));
```

Return `submit_review_findings({"findings":[]})` from the retry and assert the batch succeeds without entering the outer diff-only fallback.

- [ ] **Step 2: Verify the E2E test fails**

Run: `cargo test --test e2e_review <new_test_name> -- --nocapture`

Expected: the retry body is identical to the exploratory request and still advertises context tools.

- [ ] **Step 3: Implement timeout-aware retry bodies**

For a context follow-up HTTP 504 or `AiToolLoopTimeout`, rebuild the retry request in `FinalizeOnly` mode and append the finalization instruction once. Keep unchanged retry behavior for 408, 429, 502, and 503. Ensure the finalization retry cannot recursively retry as another exploratory request.

- [ ] **Step 4: Run focused E2E and unit tests**

Run: `cargo test --test e2e_review <new_test_name> -- --nocapture`

Run: `cargo test review::ai::tests --lib`

Expected: both commands pass.

- [ ] **Step 5: Commit**

```bash
git add src/review/ai.rs tests/e2e_review.rs
git commit -m "fix: finalize AI review after context timeout"
```

### Task 7: Document and verify the complete feature

**Files:**
- Modify: `README.md`
- Modify: `README.en.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Update documentation**

Document that:

- `max_tool_total_bytes` defaults to `40000` and `0` means unlimited;
- it is a per-batch cumulative returned-result budget;
- exact duplicate calls are compactly acknowledged and do not consume budget;
- count or byte exhaustion forces one final response with context tools removed;
- a 504 or context request timeout retries once in finalization mode;
- operators should configure request timeout below their upstream gateway timeout.

- [ ] **Step 2: Run formatting and static verification**

Run: `cargo fmt --check`

Run: `cargo clippy --all-targets --all-features -- -D warnings`

Expected: both commands exit successfully.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`

Expected: all unit and integration tests pass.

- [ ] **Step 4: Check the patch**

Run: `git diff --check`

Run: `git status --short`

Expected: no whitespace errors; only intended documentation/code changes are present.

- [ ] **Step 5: Commit documentation**

```bash
git add README.md README.en.md CHANGELOG.md
git commit -m "docs: explain bounded AI context exploration"
```
