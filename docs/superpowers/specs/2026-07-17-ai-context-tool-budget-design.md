# AI Review Context Tool Budget Design

## Goal

Reduce unnecessary context-tool calls and prevent large merge-request reviews from timing out, while preserving the ability to verify high-confidence findings against repository context.

This first phase deliberately excludes file-risk scoring and adaptive batch sizing. It focuses on making the existing per-batch tool loop bounded, compact, and able to finish reliably.

## Current problems

The current system prompt requires context verification for a broad set of conclusions and recommends a `search/list -> read` sequence. The model can therefore enter long exploratory chains before it has identified a concrete candidate finding.

`read_file` returns file content from the beginning and has no line-range parameters. A small verification can consequently add tens of kilobytes to the conversation. `max_tool_result_bytes` applies independently to every call; there is no cumulative result budget. Every follow-up request then resends the complete tool history.

When the tool-call limit is reached, context tools remain present in the next request. The runner relies on an error result asking the model to stop, which some models may ignore. Retryable gateway failures also resend the same large request body, making deterministic gateway timeouts likely to repeat.

## Design

### 1. Verification-first prompt policy

Change the built-in context-tool guidance so the model first reviews the diff and forms a concrete candidate defect. Context tools are used only when evidence missing from the diff is necessary to confirm or reject that candidate.

The prompt will state:

- do not browse the repository without a concrete candidate defect;
- do not call tools when the diff already proves or disproves the candidate;
- prefer one precise search followed by one narrow read;
- do not attempt an exhaustive repository-wide proof that no guard exists;
- if the available evidence is insufficient after the tool budget, abandon the candidate and submit the remaining findings, including an empty result when appropriate.

### 2. Bounded file reads

Extend `read_file` with optional one-based inclusive `start_line` and `end_line` parameters.

- Without a range, retain the current whole-file behavior for compatibility, but apply the configured per-result byte cap.
- With a range, return only that range and include the actual start/end line metadata.
- Reject zero, reversed, or excessively large ranges.
- Cap one request to 250 lines so a model cannot bypass the byte budget with a huge range.
- Update the tool description to prefer narrow ranges and recommend using search-result line numbers.

Whole-file compatibility is retained in phase one to avoid breaking providers that have learned the existing schema. A later phase may change the default to a bounded preview.

### 3. Per-batch cumulative result budget

Add `max_tool_total_bytes` to AI review configuration. It is a per-batch limit over actual context-tool result strings returned to the model.

- Default: `40000` bytes.
- `0` means unlimited, matching the existing convention for count limits.
- Before executing a call, compute the remaining byte budget.
- Execute the call with a result cap equal to the smaller of the per-call and remaining budgets.
- Once exhausted, return a structured limit result for outstanding calls and enter finalization mode.
- Record cumulative bytes in logs and review coverage metadata where practical; persistence/schema expansion is excluded from phase one unless required by existing structures.

### 4. Deterministic finalization mode

After either `max_tool_calls` or `max_tool_total_bytes` is exhausted, the next follow-up request will expose only `submit_review_findings`; `read_file`, `search_code`, and `list_files` will be removed from the tool definitions.

The runner will append a trusted message instructing the model to abandon unverified candidates and submit final findings using existing evidence. If the model still fails to submit structured findings, the existing parse/failure behavior applies. No additional exploratory round is permitted.

This replaces the current pattern where disabled tools remain advertised and the runner waits for the model to obey an error embedded in tool output.

### 5. Duplicate-call suppression

Within a batch, cache context-tool results by normalized tool name and canonicalized arguments.

- Identical calls return a compact structured response referring to the earlier result, without consuming another tool-call count or result-byte budget.
- The compact response satisfies the chat protocol without appending the same large content to the conversation again.
- Semantically overlapping but non-identical ranges are not merged in phase one.
- Cache hits are logged explicitly.

This is intentionally exact-match caching; fuzzy query or overlapping-range consolidation would add complexity and risk incorrect evidence reuse.

### 6. Timeout-aware retry finalization

Keep ordinary retry behavior for transient failures that occur quickly. For an HTTP 504 or request timeout during a context follow-up, do not resend the same exploratory request unchanged.

The retry request will enter finalization mode:

- remove context tools;
- retain the diff and evidence already obtained;
- append the trusted finalization instruction;
- request `submit_review_findings` only.

The existing outer diff-only fallback remains the last resort when the finalization retry also fails. This phase does not automatically alter configured request timeouts because deployments may have different gateway limits; documentation will recommend keeping the request timeout below the upstream gateway timeout.

## Configuration

Example:

```toml
max_tool_calls = 8
max_tool_result_bytes = 12000
max_tool_total_bytes = 40000
```

Existing configurations remain valid. The new field uses its default when absent.

## Error handling

- Invalid line ranges return normal structured tool errors and consume one real tool call.
- Exact cache hits do not consume additional budgets.
- Exhausted budgets do not immediately fail the batch; they force one final structured-submission round.
- Failure to submit after finalization fails the batch with an explicit AI review error.
- Timeout finalization is attempted once and must not recurse.

## Testing

Add unit and integration tests covering:

- prompt guidance requires a concrete candidate before context lookup;
- ranged reads return correct lines and reject invalid or oversized ranges;
- exact duplicate calls use cached results without consuming budgets;
- cumulative byte exhaustion skips remaining calls;
- finalization requests contain only `submit_review_findings`;
- a model that submits after finalization succeeds;
- a model that refuses to submit after finalization fails clearly;
- a 504 follow-up retry changes to finalization rather than resending the same request;
- existing configurations without `max_tool_total_bytes` still parse.

## Deferred work

- risk scoring and adaptive per-file tool budgets;
- symbol-aware reads and language-server integration;
- cross-batch evidence caching;
- overlapping line-range merging;
- persistent Dashboard metrics for cumulative tool bytes;
- automatic gateway-timeout discovery.
