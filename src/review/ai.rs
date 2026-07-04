use crate::{
    diff::{parse_unified_diff, DiffLineKind},
    error::{AppError, AppResult},
    gitlab::GitLabChange,
    rules::{AiReviewConfig, Finding, Severity},
};
use std::{
    collections::BTreeSet,
    path::Path,
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};

use super::{
    ai_http::{
        is_retryable_ai_error, perform_ai_review_http_attempt, shared_ai_http_client,
        AiReviewHttpResponse,
    },
    ai_prompt::{build_review_prompt_with_limit, change_diff_payload, initial_chat_messages},
    ai_schema::{
        AiFindingsResponse, ChatMessage, OpenAiChatRequest, OpenAiChatResponse, OpenAiMessage,
        OpenAiToolCall, ResponseFormat,
    },
    ai_tools::{
        enabled_context_tools, is_context_tool_call, non_empty_tool_call_id, review_findings_tool,
        AiReviewToolContext,
    },
};

const AI_RESPONSE_PREVIEW_CHARS: usize = 1000;
const AI_HTTP_ATTEMPTS: usize = 2;

pub async fn run_ai_review(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
) -> AppResult<Vec<Finding>> {
    run_ai_review_with_context(config, changes, None).await
}

pub async fn run_ai_review_with_context(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
    source_dir: Option<&Path>,
) -> AppResult<Vec<Finding>> {
    if config.batch_review {
        return run_batched_ai_review(config, changes, source_dir).await;
    }
    run_ai_review_single(config, changes, config.max_diff_bytes, None, source_dir).await
}

async fn run_ai_review_single(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
    max_diff_bytes: usize,
    batch: Option<(usize, usize)>,
    source_dir: Option<&Path>,
) -> AppResult<Vec<Finding>> {
    let api_key = config.api_key.trim();
    if api_key.is_empty() {
        return Err(AppError::AiReview(format!(
            "api_key is empty for AI review {}",
            config.id
        )));
    }
    let (prompt, diff_payload_bytes, diff_payload_truncated) =
        build_review_prompt_with_limit(config, changes, max_diff_bytes);
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let request_timeout_seconds = effective_request_timeout_seconds(config);
    let request_timeout = Duration::from_secs(request_timeout_seconds);
    let request_guard_timeout = request_timeout + Duration::from_secs(5);
    let client = shared_ai_http_client()?;
    let mut use_tool_calls = true;
    let tool_context = AiReviewToolContext::new(config, source_dir);
    let context_tools_enabled = tool_context.enabled();
    let context_tool_names = tool_context.enabled_tool_names();
    info!(
        ai_review_id = %config.id,
        model = %config.model,
        timeout_seconds = config.timeout_seconds,
        request_timeout_seconds,
        request_guard_timeout_seconds = request_guard_timeout.as_secs(),
        max_diff_bytes = config.max_diff_bytes,
        change_count = changes.len(),
        diff_payload_bytes,
        diff_payload_truncated,
        prompt_bytes = prompt.len(),
        context_tools_enabled,
        context_tool_names = %context_tool_names,
        context_source_available = tool_context.source_available(),
        max_tool_calls = config.max_tool_calls,
        max_tool_result_bytes = config.max_tool_result_bytes,
        batch_index = batch.map(|(index, _)| index),
        batch_count = batch.map(|(_, count)| count),
        "calling AI review API"
    );
    let mut last_error = None;
    for attempt in 1..=AI_HTTP_ATTEMPTS {
        let mut messages = initial_chat_messages(config, &prompt);
        let request_body = serialize_review_request_body(config, &messages, use_tool_calls)?;
        let attempt_started = Instant::now();
        let request_body_bytes = request_body.len();
        info!(
            ai_review_id = %config.id,
            model = %config.model,
            attempt,
            request_timeout_seconds,
            request_guard_timeout_seconds = request_guard_timeout.as_secs(),
            request_body_bytes,
            use_tool_calls,
            batch_index = batch.map(|(index, _)| index),
            batch_count = batch.map(|(_, count)| count),
            "sending AI review API request"
        );

        match tokio::time::timeout(
            request_guard_timeout,
            perform_ai_review_http_attempt(
                client,
                config,
                &url,
                api_key,
                request_body,
                attempt,
                request_timeout,
            ),
        )
        .await
        {
            Ok(Ok(response)) => {
                let AiReviewHttpResponse { status, body } = response;
                let response_body_preview = preview_log_text(&body, AI_RESPONSE_PREVIEW_CHARS);
                info!(
                    ai_review_id = %config.id,
                    model = %config.model,
                    attempt,
                    status,
                    elapsed_ms = attempt_started.elapsed().as_millis(),
                    response_bytes = body.len(),
                    "AI review raw response body received"
                );
                debug!(
                    ai_review_id = %config.id,
                    model = %config.model,
                    attempt,
                    response_body_preview = %response_body_preview,
                    "AI review raw response body preview"
                );
                if !(200..300).contains(&status) {
                    if use_tool_calls && is_tool_call_rejection(status, &response_body_preview) {
                        warn!(
                            ai_review_id = %config.id,
                            model = %config.model,
                            attempt,
                            status,
                            "AI review API rejected tool calls, falling back to JSON content"
                        );
                        use_tool_calls = false;
                        last_error = Some(AppError::AiReview(format!(
                            "AI review API rejected tool calls: {response_body_preview}"
                        )));
                        continue;
                    }
                    return Err(AppError::AiReview(format!(
                        "AI review API returned HTTP status {}: {}",
                        status, response_body_preview
                    )));
                }
                let findings = complete_ai_review_response(AiReviewCompletion {
                    client,
                    config,
                    url: &url,
                    api_key,
                    messages: &mut messages,
                    use_tool_calls,
                    tool_context: &tool_context,
                    first_body: &body,
                    attempt,
                    request_timeout,
                    request_guard_timeout,
                })
                .await?;
                let raw_finding_count = findings.len();
                let filtered = filter_findings_to_added_lines(changes, findings)?;
                info!(
                    ai_review_id = %config.id,
                    model = %config.model,
                    attempt,
                    raw_findings = raw_finding_count,
                    findings = filtered.len(),
                    filtered_findings = raw_finding_count.saturating_sub(filtered.len()),
                    elapsed_ms = attempt_started.elapsed().as_millis(),
                    "AI review API completed"
                );
                return Ok(filtered);
            }
            Ok(Err(err)) => {
                let retryable = attempt < AI_HTTP_ATTEMPTS && is_retryable_ai_error(&err);
                if retryable {
                    warn!(
                        ai_review_id = %config.id,
                        model = %config.model,
                        attempt,
                        elapsed_ms = attempt_started.elapsed().as_millis(),
                        error = %err,
                        "AI review API request failed, retrying"
                    );
                    last_error = Some(err);
                    continue;
                }
                return Err(err);
            }
            Err(_) => {
                let err = AppError::AiReview(format!(
                    "AI review {} timed out after {} seconds",
                    config.id,
                    request_timeout.as_secs()
                ));
                if attempt < AI_HTTP_ATTEMPTS {
                    warn!(
                        ai_review_id = %config.id,
                        model = %config.model,
                        attempt,
                        elapsed_ms = attempt_started.elapsed().as_millis(),
                        timeout_seconds = request_guard_timeout.as_secs(),
                        "AI review API request timed out, retrying"
                    );
                    last_error = Some(err);
                    continue;
                }
                return Err(err);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        AppError::AiReview(format!(
            "AI review {} failed without an explicit error",
            config.id
        ))
    }))
}

async fn run_batched_ai_review(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
    source_dir: Option<&Path>,
) -> AppResult<Vec<Finding>> {
    let batches = split_ai_review_batches(
        changes,
        config.max_batch_diff_bytes.max(1),
        config.max_batches.max(1),
    );
    info!(
        ai_review_id = %config.id,
        model = %config.model,
        batch_count = batches.len(),
        max_batch_diff_bytes = config.max_batch_diff_bytes,
        max_batches = config.max_batches,
        "AI review batching enabled"
    );

    let mut all_findings = Vec::new();
    let batch_count = batches.len();
    for (index, batch) in batches.iter().enumerate() {
        let batch_index = index + 1;
        let mut findings = run_ai_review_single(
            config,
            batch,
            config.max_batch_diff_bytes.max(1),
            Some((batch_index, batch_count)),
            source_dir,
        )
        .await?;
        all_findings.append(&mut findings);
    }
    Ok(all_findings)
}

fn serialize_review_request_body(
    config: &AiReviewConfig,
    messages: &[ChatMessage],
    use_tool_calls: bool,
) -> AppResult<Vec<u8>> {
    let (response_format, tools, tool_choice) = if use_tool_calls {
        let mut tools = enabled_context_tools(config);
        tools.push(review_findings_tool());
        (None, Some(tools), None)
    } else {
        (
            Some(ResponseFormat {
                response_type: "json_object",
            }),
            None,
            None,
        )
    };
    let request = OpenAiChatRequest {
        model: &config.model,
        temperature: 0.2,
        response_format,
        tools,
        tool_choice,
        messages,
    };
    Ok(serde_json::to_vec(&request)?)
}

fn is_tool_call_rejection(status: u16, body: &str) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let normalized = body.to_ascii_lowercase();
    normalized.contains("tool")
        || normalized.contains("tools")
        || normalized.contains("tool_choice")
        || normalized.contains("function")
}

struct AiReviewCompletion<'a> {
    client: &'a ureq::Agent,
    config: &'a AiReviewConfig,
    url: &'a str,
    api_key: &'a str,
    messages: &'a mut Vec<ChatMessage>,
    use_tool_calls: bool,
    tool_context: &'a AiReviewToolContext,
    first_body: &'a str,
    attempt: usize,
    request_timeout: Duration,
    request_guard_timeout: Duration,
}

async fn complete_ai_review_response(context: AiReviewCompletion<'_>) -> AppResult<Vec<Finding>> {
    let AiReviewCompletion {
        client,
        config,
        url,
        api_key,
        messages,
        use_tool_calls,
        tool_context,
        first_body,
        attempt,
        request_timeout,
        request_guard_timeout,
    } = context;
    let mut body = first_body.to_string();
    let mut tool_calls_used = 0_usize;
    loop {
        let response: OpenAiChatResponse = serde_json::from_str(&body)?;
        let message = response
            .choices
            .first()
            .map(|choice| &choice.message)
            .ok_or_else(|| AppError::AiReview("AI review API returned no choices".into()))?;
        if has_submit_review_findings(message) || !use_tool_calls {
            return parse_openai_message(&config.id, &config.title, message);
        }

        let context_tool_calls: Vec<OpenAiToolCall> = message
            .tool_calls
            .iter()
            .filter(|tool_call| is_context_tool_call(tool_call))
            .cloned()
            .collect();
        if context_tool_calls.is_empty() {
            return parse_openai_message(&config.id, &config.title, message);
        }
        let tool_call_names = context_tool_calls
            .iter()
            .map(|tool_call| tool_call.function.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        info!(
            ai_review_id = %config.id,
            model = %config.model,
            attempt,
            requested_tool_calls = context_tool_calls.len(),
            tool_call_names = %tool_call_names,
            tool_calls_used,
            max_tool_calls = config.max_tool_calls,
            "AI review context tool calls requested"
        );
        let remaining_tool_calls = config.max_tool_calls.saturating_sub(tool_calls_used);
        if remaining_tool_calls == 0 || context_tool_calls.len() > remaining_tool_calls {
            warn!(
                ai_review_id = %config.id,
                model = %config.model,
                requested_tool_calls = context_tool_calls.len(),
                remaining_tool_calls,
                tool_calls_used,
                max_tool_calls = config.max_tool_calls,
                "AI review context tool call limit reached"
            );
            return Err(AppError::AiReview(format!(
                "AI review {} exhausted context tool calls before submitting findings",
                config.id
            )));
        }

        messages.push(ChatMessage {
            role: "assistant".into(),
            content: message.content.clone(),
            tool_call_id: None,
            tool_calls: Some(context_tool_calls.clone()),
        });
        for tool_call in context_tool_calls {
            let tool_call_id = non_empty_tool_call_id(&tool_call);
            let result = tool_context.call(&tool_call);
            tool_calls_used += 1;
            info!(
                ai_review_id = %config.id,
                model = %config.model,
                tool_name = %tool_call.function.name,
                tool_call_id = %tool_call_id,
                result_bytes = result.len(),
                tool_calls_used,
                max_tool_calls = config.max_tool_calls,
                "AI review context tool result returned"
            );
            messages.push(ChatMessage {
                role: "tool".into(),
                content: Some(result),
                tool_call_id: Some(tool_call_id),
                tool_calls: None,
            });
        }

        let request_body = serialize_review_request_body(config, messages, true)?;
        let response = tokio::time::timeout(
            request_guard_timeout,
            perform_ai_review_http_attempt(
                client,
                config,
                url,
                api_key,
                request_body,
                attempt,
                request_timeout,
            ),
        )
        .await
        .map_err(|_| {
            AppError::AiReview(format!(
                "AI review {} timed out after {} seconds",
                config.id,
                request_timeout.as_secs()
            ))
        })??;
        if !(200..300).contains(&response.status) {
            return Err(AppError::AiReview(format!(
                "AI review API returned HTTP status {}: {}",
                response.status,
                preview_log_text(&response.body, AI_RESPONSE_PREVIEW_CHARS)
            )));
        }
        body = response.body;
    }
}

fn has_submit_review_findings(message: &OpenAiMessage) -> bool {
    message
        .tool_calls
        .iter()
        .any(|tool_call| tool_call.function.name == "submit_review_findings")
}

fn effective_request_timeout_seconds(config: &AiReviewConfig) -> u64 {
    config
        .request_timeout_seconds
        .unwrap_or_else(|| config.timeout_seconds.max(1).div_ceil(2))
        .max(1)
}

fn split_ai_review_batches(
    changes: &[GitLabChange],
    max_batch_diff_bytes: usize,
    max_batches: usize,
) -> Vec<Vec<GitLabChange>> {
    let max_batch_diff_bytes = max_batch_diff_bytes.max(1);
    let max_batches = max_batches.max(1);
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut current_bytes = 0_usize;

    for change in changes {
        let change_bytes = change_diff_payload(change).len();
        if !current.is_empty() && current_bytes + change_bytes > max_batch_diff_bytes {
            batches.push(current);
            if batches.len() >= max_batches {
                return batches;
            }
            current = Vec::new();
            current_bytes = 0;
        }
        current.push(change.clone());
        current_bytes += change_bytes;
    }

    if !current.is_empty() && batches.len() < max_batches {
        batches.push(current);
    }
    batches
}

#[cfg(test)]
fn parse_openai_response(review_id: &str, title: &str, text: &str) -> AppResult<Vec<Finding>> {
    let response: OpenAiChatResponse = serde_json::from_str(text)?;
    let message = response
        .choices
        .first()
        .map(|choice| &choice.message)
        .ok_or_else(|| AppError::AiReview("AI review API returned no choices".into()))?;
    parse_openai_message(review_id, title, message)
}

fn parse_openai_message(
    review_id: &str,
    title: &str,
    message: &OpenAiMessage,
) -> AppResult<Vec<Finding>> {
    let content = tool_call_arguments(message).or_else(|_| {
        message
            .content
            .as_deref()
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .ok_or_else(|| AppError::AiReview("AI review API returned no content".into()))
    })?;
    info!(
        ai_review_id = %review_id,
        assistant_content_bytes = content.len(),
        assistant_content_preview = %preview_log_text(content, AI_RESPONSE_PREVIEW_CHARS),
        "AI review assistant content received"
    );
    let parsed: AiFindingsResponse = serde_json::from_str(content)?;
    info!(
        ai_review_id = %review_id,
        parsed_findings = parsed.findings.len(),
        "AI review assistant findings parsed"
    );
    Ok(parsed
        .findings
        .into_iter()
        .filter(|finding| !finding.path.trim().is_empty() && !finding.message.trim().is_empty())
        .map(|finding| Finding {
            rule_id: format!("ai:{review_id}"),
            severity: parse_severity(&finding.severity),
            path: finding.path.trim().replace('\\', "/"),
            new_line: Some(finding.line),
            title: non_empty_or(finding.title, title),
            message: finding.message.trim().to_string(),
        })
        .collect())
}

fn tool_call_arguments(message: &OpenAiMessage) -> AppResult<&str> {
    message
        .tool_calls
        .iter()
        .find(|tool_call| tool_call.function.name == "submit_review_findings")
        .map(|tool_call| tool_call.function.arguments.as_str())
        .ok_or_else(|| {
            AppError::AiReview("AI review API returned no submit_review_findings tool call".into())
        })
}

fn parse_severity(value: &str) -> Severity {
    match value.trim().to_ascii_lowercase().as_str() {
        "error" => Severity::Error,
        _ => Severity::Error,
    }
}

fn non_empty_or(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn preview_log_text(value: &str, max_chars: usize) -> String {
    let mut preview = String::new();
    let mut truncated = false;
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            truncated = true;
            break;
        }
        match ch {
            '\n' => preview.push_str("\\n"),
            '\r' => preview.push_str("\\r"),
            '\t' => preview.push_str("\\t"),
            _ => preview.push(ch),
        }
    }
    if truncated {
        preview.push_str("...");
    }
    preview
}

fn filter_findings_to_added_lines(
    changes: &[GitLabChange],
    findings: Vec<Finding>,
) -> AppResult<Vec<Finding>> {
    let mut added_lines = BTreeSet::new();
    for change in changes {
        if change.deleted_file || change.diff.trim().is_empty() {
            continue;
        }
        let diff_file = parse_unified_diff(&change.old_path, &change.new_path, &change.diff)?;
        for hunk in diff_file.hunks {
            for line in hunk.lines {
                if line.kind == DiffLineKind::Added {
                    if let Some(new_line) = line.new_line {
                        added_lines.insert((diff_file.new_path.clone(), new_line));
                    }
                }
            }
        }
    }

    Ok(findings
        .into_iter()
        .filter(|finding| {
            let Some(new_line) = finding.new_line else {
                return false;
            };
            let keep = added_lines.contains(&(finding.path.clone(), new_line));
            if !keep {
                warn!(
                    rule_id = %finding.rule_id,
                    path = %finding.path,
                    new_line,
                    "AI review finding ignored because it is not on an added diff line"
                );
            }
            keep
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use crate::{
        gitlab::GitLabChange,
        review::{
            ai_prompt::{build_review_prompt, limited_diff_payload},
            ai_tools::{list_files_tool, read_file_tool, search_code_tool},
        },
        rules::AiReviewContextTools,
        rules::Severity,
    };
    use serde_json::Value;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::sleep,
    };

    use super::*;

    fn test_ai_review_config() -> AiReviewConfig {
        AiReviewConfig {
            auto_enabled: true,
            id: "ai-review".into(),
            title: "AI Review".into(),
            base_url: "https://ai.example.com".into(),
            api_key: "test-key".into(),
            model: "test-model".into(),
            timeout_seconds: 60,
            request_timeout_seconds: None,
            max_diff_bytes: 60_000,
            second_pass_on_clean: false,
            batch_review: false,
            max_batch_diff_bytes: 30_000,
            max_batches: 6,
            system_prompt: None,
            extra_instructions: String::new(),
            max_tool_calls: 8,
            max_tool_result_bytes: 60_000,
            context_tools: AiReviewContextTools::default(),
            when_changed: vec![],
        }
    }

    #[test]
    fn parses_openai_compatible_findings_from_assistant_content() {
        let response = r#"
{
  "choices": [{
    "message": {
      "content": "{\"findings\":[{\"path\":\"src/lib.rs\",\"line\":12,\"severity\":\"error\",\"title\":\"Possible panic\",\"message\":\"Avoid unwrap here.\"}]}"
    }
  }]
}
"#;

        let findings = parse_openai_response("ai-review", "AI Review", response).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "ai:ai-review");
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].path, "src/lib.rs");
        assert_eq!(findings[0].new_line, Some(12));
        assert_eq!(findings[0].title, "Possible panic");
        assert_eq!(findings[0].message, "Avoid unwrap here.");
    }

    #[test]
    fn treats_unknown_ai_severity_as_error() {
        let response = r#"
{
  "choices": [{
    "message": {
      "content": "{\"findings\":[{\"path\":\"src/lib.rs\",\"line\":12,\"severity\":\"warning\",\"title\":\"Bug\",\"message\":\"This is a bug.\"}]}"
    }
  }]
}
"#;

        let findings = parse_openai_response("ai-review", "AI Review", response).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn filters_findings_to_added_lines_in_current_diff() {
        let changes = vec![GitLabChange {
            old_path: "src/lib.rs".into(),
            new_path: "src/lib.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -10,2 +10,3 @@\n context\n-old();\n+new();\n+added();\n".into(),
        }];
        let findings = vec![
            crate::rules::Finding {
                rule_id: "ai:ai-review".into(),
                severity: Severity::Warning,
                path: "src/lib.rs".into(),
                new_line: Some(11),
                title: "Keep".into(),
                message: "Line 11 is added.".into(),
            },
            crate::rules::Finding {
                rule_id: "ai:ai-review".into(),
                severity: Severity::Warning,
                path: "src/lib.rs".into(),
                new_line: Some(10),
                title: "Drop".into(),
                message: "Line 10 is context.".into(),
            },
        ];

        let filtered = filter_findings_to_added_lines(&changes, findings).unwrap();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].title, "Keep");
    }

    #[test]
    fn truncates_diff_payload_at_utf8_boundary() {
        let changes = vec![GitLabChange {
            old_path: "src/lib.rs".into(),
            new_path: "src/lib.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+let text = \"中文\";\n".into(),
        }];

        let (payload, truncated) = limited_diff_payload(&changes, 5);

        assert!(truncated);
        assert!(std::str::from_utf8(payload.as_bytes()).is_ok());
    }

    #[test]
    fn prompt_requires_findings_in_final_json_content() {
        let config = test_ai_review_config();
        let changes = vec![GitLabChange {
            old_path: "src/lib.rs".into(),
            new_path: "src/lib.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+panic!();\n".into(),
        }];

        let (prompt, _, _) = build_review_prompt(&config, &changes);

        assert!(prompt.contains("submit_review_findings"));
        assert!(prompt.contains("不要把最终结果只写在 reasoning_content"));
    }

    #[test]
    fn prompt_includes_extra_ai_review_instructions() {
        let config = AiReviewConfig {
            system_prompt: Some("Custom system prompt".into()),
            extra_instructions: "Focus on C++ lifetime bugs.".into(),
            ..test_ai_review_config()
        };
        let changes = vec![GitLabChange {
            old_path: "src/lib.rs".into(),
            new_path: "src/lib.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+panic!();\n".into(),
        }];

        let (prompt, _, _) = build_review_prompt(&config, &changes);
        let messages = initial_chat_messages(&config, &prompt);
        let body = serialize_review_request_body(&config, &messages, true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert!(prompt.contains("Focus on C++ lifetime bugs."));
        assert_eq!(json["messages"][0]["content"], "Custom system prompt");
    }

    #[test]
    fn prompt_guides_context_tool_usage_when_enabled() {
        let config = AiReviewConfig {
            context_tools: AiReviewContextTools {
                read_file: true,
                search_code: true,
                list_files: true,
            },
            ..test_ai_review_config()
        };
        let changes = vec![GitLabChange {
            old_path: "src/lib.rs".into(),
            new_path: "src/lib.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+panic!();\n".into(),
        }];

        let (prompt, _, _) = build_review_prompt(&config, &changes);

        assert!(prompt.contains("上下文工具已启用"));
        assert!(prompt.contains("list_files/search_code"));
        assert!(prompt.contains("不要为了风格"));
        assert!(prompt.contains("最终仍然只提交高置信度"));
    }

    #[test]
    fn serializes_review_request_body_with_prompt() {
        let config = test_ai_review_config();
        let changes = vec![GitLabChange {
            old_path: "src/lib.rs".into(),
            new_path: "src/lib.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+panic!();\n".into(),
        }];

        let (prompt, _, _) = build_review_prompt(&config, &changes);
        let messages = initial_chat_messages(&config, &prompt);
        let body = serialize_review_request_body(&config, &messages, true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["model"], "test-model");
        assert_eq!(json["messages"][1]["content"], prompt);
        assert!(json.get("response_format").is_none());
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(
            json["tools"][0]["function"]["name"],
            "submit_review_findings"
        );
        assert!(json.get("tool_choice").is_none());
    }

    #[test]
    fn serializes_enabled_context_tools() {
        let config = AiReviewConfig {
            context_tools: AiReviewContextTools {
                read_file: true,
                search_code: true,
                list_files: true,
            },
            ..test_ai_review_config()
        };
        let messages = initial_chat_messages(&config, "prompt");
        let body = serialize_review_request_body(&config, &messages, true).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        let names: Vec<_> = json["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();

        assert_eq!(
            names,
            vec![
                "read_file",
                "search_code",
                "list_files",
                "submit_review_findings"
            ]
        );
    }

    #[test]
    fn context_tool_schemas_describe_arguments_and_results() {
        let read_file = read_file_tool();
        let search_code = search_code_tool();
        let list_files = list_files_tool();

        assert!(read_file.function.description.contains("Returns JSON"));
        assert!(read_file
            .function
            .description
            .contains("repository-relative"));
        assert!(
            read_file.function.parameters["properties"]["path"]["description"]
                .as_str()
                .unwrap()
                .contains("Repository-relative")
        );
        assert!(search_code.function.description.contains("plain substring"));
        assert!(search_code
            .function
            .description
            .contains("At most 50 total matches"));
        assert!(search_code
            .function
            .description
            .contains("5 matches per file"));
        assert!(search_code.function.description.contains("before"));
        assert!(
            search_code.function.parameters["properties"]["query"]["description"]
                .as_str()
                .unwrap()
                .contains("not a regex")
        );
        assert!(list_files
            .function
            .description
            .contains("At most 200 files"));
        assert!(
            list_files.function.parameters["properties"]["glob"]["description"]
                .as_str()
                .unwrap()
                .contains("Optional glob")
        );
    }

    #[test]
    fn builtin_read_file_rejects_unsafe_paths() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("lib.rs"), "pub fn value() {}\n").unwrap();
        std::fs::write(temp.path().join(".env"), "SECRET=1\n").unwrap();
        let config = AiReviewConfig {
            context_tools: AiReviewContextTools {
                read_file: true,
                search_code: false,
                list_files: false,
            },
            ..test_ai_review_config()
        };
        let context = AiReviewToolContext::new(&config, Some(temp.path()));

        let parent_result = context.read_file(r#"{"path":"../lib.rs"}"#);
        let env_result = context.read_file(r#"{"path":".env"}"#);
        let ok_result = context.read_file(r#"{"path":"lib.rs"}"#);

        assert_eq!(parent_result["ok"], false);
        assert_eq!(env_result["ok"], false);
        assert_eq!(ok_result["ok"], true);
        assert!(ok_result["content"].as_str().unwrap().contains("value"));
    }

    #[test]
    fn serializes_json_content_fallback_review_request_body() {
        let config = test_ai_review_config();
        let messages = initial_chat_messages(&config, "prompt");
        let body = serialize_review_request_body(&config, &messages, false).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["response_format"]["type"], "json_object");
        assert!(json.get("tools").is_none());
        assert!(json.get("tool_choice").is_none());
    }

    #[test]
    fn parses_findings_from_tool_call_arguments() {
        let response = r#"
{
  "choices": [{
    "message": {
      "content": "",
      "tool_calls": [{
        "type": "function",
        "function": {
          "name": "submit_review_findings",
          "arguments": "{\"findings\":[{\"path\":\"src/lib.rs\",\"line\":12,\"severity\":\"error\",\"title\":\"Possible panic\",\"message\":\"Avoid unwrap here.\"}]}"
        }
      }]
    }
  }]
}
"#;

        let findings = parse_openai_response("ai-review", "AI Review", response).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "src/lib.rs");
        assert_eq!(findings[0].new_line, Some(12));
    }

    #[test]
    fn reuses_shared_ai_http_client() {
        let client_one = shared_ai_http_client().unwrap() as *const ureq::Agent;
        let client_two = shared_ai_http_client().unwrap() as *const ureq::Agent;

        assert_eq!(client_one, client_two);
    }

    #[tokio::test]
    async fn retries_ai_review_http_timeout_once_before_succeeding() {
        let attempt_count = Arc::new(AtomicUsize::new(0));
        let attempt_count_for_server = Arc::clone(&attempt_count);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let attempt_count = Arc::clone(&attempt_count_for_server);
                tokio::spawn(async move {
                    let attempt = attempt_count.fetch_add(1, Ordering::SeqCst) + 1;
                    let mut buffer = [0_u8; 4096];
                    let _ = stream.read(&mut buffer).await.unwrap();
                    if attempt == 1 {
                        sleep(Duration::from_secs(2)).await;
                        return;
                    }

                    let body =
                        b"{\"choices\":[{\"message\":{\"content\":\"{\\\"findings\\\":[]}\"}}]}";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                    stream.write_all(body).await.unwrap();
                });
            }
        });

        let config = AiReviewConfig {
            base_url: format!("http://{}", addr),
            timeout_seconds: 2,
            ..test_ai_review_config()
        };
        let changes = vec![GitLabChange {
            old_path: "src/lib.rs".into(),
            new_path: "src/lib.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+panic!();\n".into(),
        }];

        let findings = run_ai_review(&config, &changes).await.unwrap();

        assert!(findings.is_empty());
        assert_eq!(attempt_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn uses_configured_ai_review_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 4096];
                    let _ = stream.read(&mut buffer).await.unwrap();
                    sleep(Duration::from_secs(2)).await;
                    let body =
                        b"{\"choices\":[{\"message\":{\"content\":\"{\\\"findings\\\":[]}\"}}]}";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                    stream.write_all(body).await.unwrap();
                });
            }
        });

        let config = AiReviewConfig {
            base_url: format!("http://{}", addr),
            timeout_seconds: 2,
            request_timeout_seconds: Some(3),
            ..test_ai_review_config()
        };
        let changes = vec![GitLabChange {
            old_path: "src/lib.rs".into(),
            new_path: "src/lib.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "@@ -1 +1 @@\n+panic!();\n".into(),
        }];

        let findings = run_ai_review(&config, &changes).await.unwrap();

        assert!(findings.is_empty());
    }

    #[test]
    fn previews_ai_response_without_splitting_utf8() {
        let preview = preview_log_text("中文\nabcdef", 5);

        assert_eq!(preview, "中文\\nab...");
    }
}
