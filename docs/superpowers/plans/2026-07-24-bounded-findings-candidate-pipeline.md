# Bounded Findings Candidate Pipeline Implementation Plan

> **For Codex:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Prevent false clean reviews and unbounded response processing by parsing every bounded final-findings candidate through one strict pipeline, retrying only genuinely malformed candidates, and preserving trusted recovery instructions.

**Architecture:** Replace the current “tool calls plus one content payload” parser with a candidate collector that shares one eight-candidate budget across tool arguments and independently discovered JSON objects in assistant content. Each candidate is structurally parsed, semantically validated, and normalized before an order-insensitive consistency check. Return an explicit retry classification so missing payloads, candidate overflow, and protocol failures do not consume malformed recovery.

**Tech Stack:** Rust, serde/serde_json, ureq, Tokio tests, existing mock HTTP server helpers.

---

### Task 1: Lock down strict finding semantics

**Files:**
- Modify: `src/review/ai_schema.rs:102-110`
- Modify: `src/review/ai.rs:2160-2200`
- Test: `src/review/ai.rs:2860-2910`

**Step 1: Write the failing tests**

Replace the existing unknown-severity coercion test and add table-driven cases asserting `AiResponseParseFailed` for:

```rust
[
    r#"{"findings":[{"path":"src/lib.rs","line":10,"title":"Bug","message":"Issue"}]}"#,
    r#"{"findings":[{"path":"src/lib.rs","line":10,"severity":"","title":"Bug","message":"Issue"}]}"#,
    r#"{"findings":[{"path":"src/lib.rs","line":10,"severity":"warning","title":"Bug","message":"Issue"}]}"#,
    r#"{"findings":[{"path":"src/lib.rs","line":10,"severity":"garbage","title":"Bug","message":"Issue"}]}"#,
]
```

Keep the existing empty path/message/title and zero-line semantic cases.

**Step 2: Run the focused tests to verify they fail**

Run:

```bash
cargo test review::ai::tests::rejects_invalid_ai_finding
cargo test review::ai::tests::rejects_missing_or_unsupported_ai_severity
```

Expected: missing/unsupported severity cases are accepted or coerced before the implementation.

**Step 3: Implement strict severity parsing**

- Remove `#[serde(default)]` from `AiFinding::severity`.
- Reject any severity that is not equal to `error` ignoring ASCII case.
- Remove `parse_severity`; construct `Severity::Error` only after validation succeeds.
- Keep title validation strict rather than falling back to the configured review title.

**Step 4: Run focused tests**

Run the two focused commands from Step 2.

Expected: PASS.

**Step 5: Commit**

```bash
git add src/review/ai.rs src/review/ai_schema.rs
git commit -m "fix: validate final finding severity"
```

### Task 2: Parse all bounded tool and content candidates

**Files:**
- Modify: `src/review/ai.rs:2098-2260`
- Test: `src/review/ai.rs:2780-2900`
- Test: `src/review/ai.rs:3500-3650`

**Step 1: Write failing parser tests**

Add tests covering:

1. Assistant content containing `{"findings":[]}` followed by a non-empty valid findings object returns a conflicting-payload error.
2. A semantically invalid object followed by a valid object returns the valid findings.
3. Two identical valid content objects succeed.
4. An unfinished object prefix followed by valid JSON succeeds.
5. An unfinished quote outside any candidate followed by valid JSON succeeds.
6. Two tool candidates with the same findings in opposite order succeed and preserve the first candidate’s output order.
7. A valid empty tool candidate followed by a non-empty tool candidate returns a conflicting-payload error.

**Step 2: Run focused parser tests to verify they fail**

Run:

```bash
cargo test review::ai::tests::content_candidates
cargo test review::ai::tests::tool_candidates
cargo test review::ai::tests::candidate_order
```

Expected: at least the multi-object, malformed-prefix, and order-independent cases fail.

**Step 3: Introduce a unified candidate pipeline**

Implement:

```rust
const MAX_FINAL_FINDINGS_CANDIDATES: usize = 8;

enum FinalFindingsParseFailure {
    Malformed(AppError),
    Protocol(AppError),
}
```

Then:

- Count matching `submit_review_findings` calls against the shared budget.
- Discover assistant-content JSON candidates independently from each `{` using a bounded serde deserializer attempt, so an earlier unclosed object or quote cannot own global parser state.
- Count every syntactically complete content object that is submitted to structural parsing against the same budget.
- Parse and semantically validate every candidate independently.
- Keep the last malformed error only when no valid candidate exists.
- Return `Protocol` immediately if total candidates exceeds eight.
- Return `Malformed` for candidate JSON/semantic failures and conflicts.
- Return `Protocol` for no candidate.

Avoid returning the first structurally valid `AiFindingsResponse`; every discovered object must reach the outer consistency decision.

**Step 4: Canonicalize only for comparison**

Build a canonical clone for each valid `Vec<Finding>`:

```rust
canonical.sort_by(|left, right| {
    (
        &left.path,
        left.new_line,
        &left.title,
        &left.message,
        &left.severity,
    )
        .cmp(&(
            &right.path,
            right.new_line,
            &right.title,
            &right.message,
            &right.severity,
        ))
});
canonical.dedup();
```

If `Severity` lacks `Ord`, use a stable explicit severity rank or omit it because accepted AI candidates can only contain `Error`. Compare canonical clones, but return the first valid candidate unchanged.

**Step 5: Run focused parser tests**

Run the commands from Step 2.

Expected: PASS.

**Step 6: Commit**

```bash
git add src/review/ai.rs
git commit -m "fix: unify bounded findings candidates"
```

### Task 3: Restrict malformed retry to retryable parse failures

**Files:**
- Modify: `src/review/ai.rs:940-995`
- Test: `src/review/ai.rs:3600-3750`

**Step 1: Write failing request-count tests**

Using the existing mock HTTP request counter, add:

- No content, blank content, or unknown tools without submit content: one request and `AiResponseParseFailed`.
- Nine submit tool candidates: one request and protocol failure.
- A combined total of nine tool/content candidates: one request and protocol failure.
- One malformed candidate followed by a valid recovery response: two requests and success.

**Step 2: Run focused completion tests to verify they fail**

Run:

```bash
cargo test review::ai::tests::does_not_retry_missing_findings_payload
cargo test review::ai::tests::does_not_retry_excess_final_findings_candidates
cargo test review::ai::tests::retries_malformed_findings_candidate_once
```

Expected: overflow is not yet classified and malformed/protocol errors are not yet separated.

**Step 3: Wire explicit failure classification into the completion loop**

- Remove the separate `has_final_findings_candidate` precheck.
- Retry exactly once only for `FinalFindingsParseFailure::Malformed`.
- Return the contained `AppError` immediately for `Protocol`.
- On a second malformed result, return the error without another request.
- Keep warning logs specific to malformed candidate recovery.

**Step 4: Run focused tests**

Run the commands from Step 2.

Expected: PASS and request counts match.

**Step 5: Commit**

```bash
git add src/review/ai.rs
git commit -m "fix: bound malformed findings recovery"
```

### Task 4: Make malformed recovery instructions trusted

**Files:**
- Modify: `src/review/ai.rs:970-985`
- Modify: `src/review/ai.rs:1479-1520`
- Test: `src/review/ai.rs:3650-3750`
- Test: `src/review/ai.rs:5270-5330`

**Step 1: Write failing role tests**

Assert that:

- The initial malformed recovery instruction uses `role == "system"`.
- Timeout reconstruction uses `system` for both `MALFORMED_FINALIZATION_INSTRUCTION` and `MALFORMED_DIFF_ONLY_FINALIZATION_INSTRUCTION`.
- Normal, JSON-content, and ordinary diff-only finalization roles remain unchanged.

**Step 2: Run focused tests to verify they fail**

Run:

```bash
cargo test review::ai::tests::malformed_recovery_instruction_uses_system_role
cargo test review::ai::tests::timeout_finalization_preserves_recovery_role
```

Expected: malformed recovery currently uses `user`.

**Step 3: Implement role selection**

- Push the direct malformed-recovery `ChatMessage` with `role: "system"`.
- In `request_timeout_finalization`, select both instruction and role. Use `system` exactly when `malformed_retry` is true, including the diff-only combination.
- Do not change compacted tool-evidence messages or ordinary finalization roles.

**Step 4: Run focused tests**

Run the commands from Step 2.

Expected: PASS.

**Step 5: Commit**

```bash
git add src/review/ai.rs
git commit -m "fix: trust malformed recovery instructions"
```

### Task 5: Limit HTTP response bodies to 4 MiB

**Files:**
- Modify: `src/review/ai_http.rs:1-12`
- Modify: `src/review/ai_http.rs:175-220`
- Test: `src/review/ai_http.rs`

**Step 1: Write failing reader tests**

Extract a reader-level helper and test with `std::io::Cursor`:

- Exactly 4 MiB succeeds.
- 4 MiB plus one byte returns `AiRequestFailed` with a response-size message.
- A reader error still maps through the existing timeout/non-timeout handling.

**Step 2: Run focused HTTP tests to verify they fail**

Run:

```bash
cargo test review::ai_http::tests::accepts_response_body_at_limit
cargo test review::ai_http::tests::rejects_response_body_over_limit
```

Expected: helper/limit does not exist.

**Step 3: Implement bounded body reading**

Add:

```rust
const MAX_AI_RESPONSE_BODY_BYTES: u64 = 4 * 1024 * 1024;
```

Read from `response.into_reader().take(MAX_AI_RESPONSE_BODY_BYTES + 1)`, reject if the collected buffer exceeds the limit, and convert with `String::from_utf8`. Preserve timeout mapping for I/O errors and classify oversize/invalid UTF-8 as `AiRequestFailed`. Ensure `is_retryable_ai_error` does not treat the deterministic oversize error as retryable.

**Step 4: Run focused HTTP tests**

Run the commands from Step 2.

Expected: PASS.

**Step 5: Commit**

```bash
git add src/review/ai_http.rs
git commit -m "fix: cap AI response body size"
```

### Task 6: Full verification and PR update

**Files:**
- Modify if needed: `docs/superpowers/specs/2026-07-23-ai-malformed-json-retry-design.md`
- Verify: all changed Rust files

**Step 1: Format and run static checks**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

Expected: PASS.

**Step 2: Run the full test suite**

Run:

```bash
cargo test
```

Expected: all unit, binary, and end-to-end tests pass. If sandbox networking prevents local mock-server binds, rerun the same command with the already-approved escalated Cargo test permission.

**Step 3: Review the complete branch diff**

Run:

```bash
git status --short
git diff origin/main...HEAD --stat
git diff origin/main...HEAD -- src/review/ai.rs src/review/ai_schema.rs src/review/ai_http.rs
```

Confirm:

- No false clean path remains for missing/invalid/conflicting findings.
- Candidate count and response size are bounded.
- Only malformed candidates consume recovery.
- Malformed recovery is a system instruction.

**Step 4: Commit any verification-only corrections**

```bash
git add src/review/ai.rs src/review/ai_schema.rs src/review/ai_http.rs docs/superpowers/specs/2026-07-23-ai-malformed-json-retry-design.md
git commit -m "test: cover bounded findings recovery"
```

Skip this commit if the worktree is already clean.

**Step 5: Push and update the PR**

```bash
git push
```

Update PR #42’s summary and test evidence to mention strict severity, unified eight-candidate processing, the 4 MiB body cap, and trusted malformed-recovery instructions.
