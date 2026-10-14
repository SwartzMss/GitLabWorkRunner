# AI Review Malformed JSON Retry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make final AI review JSON failures observable and recover once without repeating context work or looping indefinitely.

**Architecture:** Extend the OpenAI-compatible response DTO with optional completion metadata, then log that metadata at the response-processing boundary. Refactor final findings payload selection away from JSON decoding so the completion loop can distinguish a present-but-malformed payload from a missing payload and issue exactly one finalization-only retry.

**Tech Stack:** Rust, Serde, Tokio, tracing, the existing local TCP HTTP test harness.

---

### Task 1: Preserve and expose completion metadata

**Files:**
- Modify: `src/review/ai_schema.rs`
- Modify: `src/review/ai.rs`

- [ ] **Step 1: Write failing response compatibility tests**

Add tests beside the existing response parsing tests in `src/review/ai.rs`:

```rust
#[test]
fn parses_openai_completion_metadata_when_present() {
    let response: OpenAiChatResponse = serde_json::from_str(
        r#"{
          "choices":[{
            "finish_reason":"length",
            "message":{"content":"{\"findings\":[]}"}
          }],
          "usage":{
            "prompt_tokens":120,
            "completion_tokens":80,
            "total_tokens":200
          }
        }"#,
    )
    .unwrap();

    assert_eq!(response.choices[0].finish_reason.as_deref(), Some("length"));
    let usage = response.usage.unwrap();
    assert_eq!(usage.prompt_tokens, Some(120));
    assert_eq!(usage.completion_tokens, Some(80));
    assert_eq!(usage.total_tokens, Some(200));
}

#[test]
fn parses_openai_response_without_completion_metadata() {
    let response: OpenAiChatResponse = serde_json::from_str(
        r#"{"choices":[{"message":{"content":"{\"findings\":[]}"}}]}"#,
    )
    .unwrap();

    assert_eq!(response.choices[0].finish_reason, None);
    assert!(response.usage.is_none());
}
```

- [ ] **Step 2: Run the metadata tests and verify RED**

Run:

```bash
cargo test review::ai::tests::parses_openai_completion_metadata_when_present
cargo test review::ai::tests::parses_openai_response_without_completion_metadata
```

Expected: compilation fails because `finish_reason` and `usage` do not exist.

- [ ] **Step 3: Implement optional response fields**

Update `src/review/ai_schema.rs`:

```rust
#[derive(Deserialize)]
pub(crate) struct OpenAiChatResponse {
    pub(crate) choices: Vec<OpenAiChoice>,
    #[serde(default)]
    pub(crate) usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
pub(crate) struct OpenAiChoice {
    pub(crate) message: OpenAiMessage,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct OpenAiUsage {
    #[serde(default)]
    pub(crate) prompt_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) completion_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) total_tokens: Option<u64>,
}
```

At the start of each iteration of `complete_ai_review_response`, select the full choice instead of only its message and emit an `info!` event containing `finish_reason`, all three token counts, content bytes, tool-call count, attempt, model and batch fields. Use optional fields directly so missing metadata logs as `None`.

- [ ] **Step 4: Run metadata tests and verify GREEN**

Run:

```bash
cargo test review::ai::tests::parses_openai_completion_metadata_when_present
cargo test review::ai::tests::parses_openai_response_without_completion_metadata
```

Expected: both tests pass.

- [ ] **Step 5: Commit metadata support**

```bash
git add src/review/ai_schema.rs src/review/ai.rs
git commit -m "feat: log AI completion metadata"
```

### Task 2: Retry one malformed tool-call finalization

**Files:**
- Modify: `src/review/ai.rs`

- [ ] **Step 1: Write a failing recovery test**

Add an async local-server test following `retries_retryable_tool_loop_http_response_once_before_succeeding`. The server must:

1. Return a `submit_review_findings` call whose arguments end at `{"findings":[`.
2. Inspect the second request and assert its tools are exactly `["submit_review_findings"]`.
3. Assert no assistant message contains the malformed call.
4. Assert a retry instruction mentions incomplete JSON.
5. Return a valid finding on request two.

The final assertions are:

```rust
assert_eq!(findings.len(), 1);
assert_eq!(findings[0].path, "src/lib.rs");
assert_eq!(request_count.load(Ordering::SeqCst), 2);
assert_eq!(malformed_call_leaked.load(Ordering::SeqCst), 0);
assert_eq!(finalization_only_count.load(Ordering::SeqCst), 1);
```

- [ ] **Step 2: Run the recovery test and verify RED**

Run:

```bash
cargo test review::ai::tests::retries_malformed_submit_findings_once_without_replaying_it -- --nocapture
```

Expected: FAIL because the first parse error is returned and only one request reaches the server.

- [ ] **Step 3: Separate payload lookup from payload decoding**

Introduce a payload selector that reports missing output before JSON parsing:

```rust
fn final_findings_payload(message: &OpenAiMessage) -> AppResult<&str> {
    tool_call_arguments(message).or_else(|_| {
        message
            .content
            .as_deref()
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .ok_or_else(|| {
                AppError::ai_review(
                    ReviewErrorCode::AiResponseParseFailed,
                    "AI review API returned no content",
                )
            })
    })
}
```

Make `parse_openai_message` call `final_findings_payload` and then `parse_ai_findings_response`. This keeps absent payload errors outside the malformed-payload retry condition.

- [ ] **Step 4: Implement a single finalization retry**

Add a loop flag:

```rust
let mut malformed_finalization_retry_requested = false;
```

When a final response is reached:

```rust
let payload = final_findings_payload(message)?;
match parse_ai_findings_payload(&config.id, &config.title, payload) {
    Ok(findings) => return Ok(findings),
    Err(err) if !malformed_finalization_retry_requested => {
        malformed_finalization_retry_requested = true;
        finalization_requested = true;
        warn!(error = %err, "AI review final findings payload was malformed; retrying finalization once");
        messages.push(ChatMessage {
            role: "user".into(),
            content: Some(MALFORMED_FINALIZATION_INSTRUCTION.into()),
            tool_call_id: None,
            tool_calls: None,
        });
    }
    Err(err) => return Err(err),
}
```

Define a concise Chinese trusted instruction requiring a complete, compact result and prohibiting context tools. Serialize the follow-up using the current `use_tool_calls` mode and `context_tools_enabled=false`; do not append the malformed assistant response.

- [ ] **Step 5: Run the recovery test and verify GREEN**

Run:

```bash
cargo test review::ai::tests::retries_malformed_submit_findings_once_without_replaying_it -- --nocapture
```

Expected: PASS with two HTTP requests.

- [ ] **Step 6: Commit tool-call recovery**

```bash
git add src/review/ai.rs
git commit -m "fix: retry malformed AI findings once"
```

### Task 3: Bound failure and cover JSON-content fallback

**Files:**
- Modify: `src/review/ai.rs`

- [ ] **Step 1: Write a failing bounded-retry test**

Add a server test that always returns a present but truncated findings payload. Assert:

```rust
assert_eq!(
    execution
        .result
        .unwrap_err()
        .review_failure()
        .map(|failure| failure.code),
    Some(ReviewErrorCode::AiResponseParseFailed)
);
assert_eq!(request_count.load(Ordering::SeqCst), 2);
```

- [ ] **Step 2: Run bounded-retry test**

Run:

```bash
cargo test review::ai::tests::stops_after_one_malformed_findings_retry -- --nocapture
```

Expected after Task 2: PASS. If it fails, adjust only the retry guard so no third request can occur.

- [ ] **Step 3: Write a JSON-content fallback recovery test**

Return HTTP 400 tool rejection for request one, malformed JSON content for request two, and valid JSON content for request three. On request three assert:

```rust
assert_eq!(request["response_format"]["type"], "json_object");
assert!(request.get("tools").is_none());
assert!(request["messages"].as_array().unwrap().iter().any(|message| {
    message["content"]
        .as_str()
        .is_some_and(|content| content.contains("JSON") && content.contains("完整"))
}));
```

Final assertions:

```rust
assert!(findings.is_empty());
assert_eq!(request_count.load(Ordering::SeqCst), 3);
```

- [ ] **Step 4: Run fallback recovery test and verify behavior**

Run:

```bash
cargo test review::ai::tests::retries_malformed_json_content_in_json_mode -- --nocapture
```

Expected: PASS; if it fails because the retry request enables tools, change follow-up serialization to preserve `use_tool_calls=false`.

- [ ] **Step 5: Commit bounded and fallback coverage**

```bash
git add src/review/ai.rs
git commit -m "test: cover malformed findings retry bounds"
```

### Task 4: Full verification

**Files:**
- Modify only if formatting requires it: `src/review/ai.rs`, `src/review/ai_schema.rs`

- [ ] **Step 1: Format and confirm no diff errors**

Run:

```bash
cargo fmt --check
git diff --check
```

Expected: both exit 0. If formatting fails, run `cargo fmt`, then repeat both checks.

- [ ] **Step 2: Run all tests**

Run:

```bash
cargo test
```

Expected: all unit, integration and documentation tests pass with zero failures.

- [ ] **Step 3: Run Clippy**

Run:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: exit 0 with no warnings.

- [ ] **Step 4: Review scope and status**

Run:

```bash
git status --short
git diff main...HEAD --stat
git log --oneline main..HEAD
```

Expected: only the design, plan, response schema, AI completion loop and associated tests differ from `main`; the worktree is clean after the final commit.
