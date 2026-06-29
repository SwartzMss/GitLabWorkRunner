use crate::{
    diff::{parse_unified_diff, DiffLineKind},
    error::{AppError, AppResult},
    gitlab::GitLabChange,
    rules::{AiReviewConfig, Finding, Severity},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};

const AI_RESPONSE_PREVIEW_CHARS: usize = 1000;
const AI_HTTP_ATTEMPTS: usize = 2;
static AI_HTTP_CLIENT: OnceLock<Result<ureq::Agent, String>> = OnceLock::new();

pub async fn run_ai_review(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
) -> AppResult<Vec<Finding>> {
    if config.batch_review {
        return run_batched_ai_review(config, changes).await;
    }
    run_ai_review_single(config, changes, config.max_diff_bytes, None).await
}

async fn run_ai_review_single(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
    max_diff_bytes: usize,
    batch: Option<(usize, usize)>,
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
        batch_index = batch.map(|(index, _)| index),
        batch_count = batch.map(|(_, count)| count),
        "calling AI review API"
    );
    let mut last_error = None;
    for attempt in 1..=AI_HTTP_ATTEMPTS {
        let request_body = serialize_review_request_body(config, &prompt)?;
        let attempt_started = Instant::now();
        let request_body_bytes = request_body.len();
        info!(
            ai_review_id = %config.id,
            model = %config.model,
            attempt,
            request_timeout_seconds,
            request_guard_timeout_seconds = request_guard_timeout.as_secs(),
            request_body_bytes,
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
                    return Err(AppError::AiReview(format!(
                        "AI review API returned HTTP status {}: {}",
                        status, response_body_preview
                    )));
                }
                let findings = parse_openai_response(&config.id, &config.title, &body)?;
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
        )
        .await?;
        all_findings.append(&mut findings);
    }
    Ok(all_findings)
}

const SYSTEM_PROMPT: &str = "You are a concise code reviewer. Review only added lines in the provided GitLab merge request diff. Return strict JSON only, with a top-level findings array. Do not include markdown.";

#[derive(Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    temperature: f32,
    response_format: ResponseFormat<'a>,
    messages: Vec<ChatMessage<'a>>,
}

#[derive(Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    response_type: &'a str,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[derive(Deserialize)]
struct AiFindingsResponse {
    #[serde(default)]
    findings: Vec<AiFinding>,
}

#[derive(Deserialize)]
struct AiFinding {
    path: String,
    line: u32,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    title: String,
    message: String,
}

#[cfg(test)]
fn build_review_prompt(config: &AiReviewConfig, changes: &[GitLabChange]) -> (String, usize, bool) {
    build_review_prompt_with_limit(config, changes, config.max_diff_bytes)
}

fn build_review_prompt_with_limit(
    _config: &AiReviewConfig,
    changes: &[GitLabChange],
    max_diff_bytes: usize,
) -> (String, usize, bool) {
    let (diff_text, truncated) = limited_diff_payload(changes, max_diff_bytes);
    let diff_payload_bytes = diff_text.len();
    let truncated_note = if truncated {
        "\ndiff 内容因为超过配置的字节限制已被截断。"
    } else {
        ""
    };
    let prompt = format!(
        "请用中文审查这个 GitLab Merge Request diff。只报告高置信度错误，例如会导致编译失败、运行时错误、数据损坏、安全漏洞或明显错误逻辑的问题。不要报告风格建议、可维护性建议、命名问题、性能微优化或不确定的问题。最终返回的 JSON 是唯一有效输出，格式必须是 {{\"findings\":[{{\"path\":\"src/file.rs\",\"line\":123,\"severity\":\"error\",\"title\":\"简短中文标题\",\"message\":\"具体说明为什么这是错误，以及应该如何修复。\"}}]}}。如果你在推理中发现问题，必须把问题写进最终 JSON 的 findings 数组；不要把问题只写在 reasoning_content、分析过程或其他非 content 字段里。severity 必须固定为 \"error\"。只能使用 diff 新增行的行号。如果没有确定的错误，返回 {{\"findings\":[]}}。{truncated_note}\n\n{diff_text}",
    );
    (prompt, diff_payload_bytes, truncated)
}

fn serialize_review_request_body(config: &AiReviewConfig, prompt: &str) -> AppResult<Vec<u8>> {
    let request = OpenAiChatRequest {
        model: &config.model,
        temperature: 0.2,
        response_format: ResponseFormat {
            response_type: "json_object",
        },
        messages: vec![
            ChatMessage {
                role: "system",
                content: SYSTEM_PROMPT,
            },
            ChatMessage {
                role: "user",
                content: prompt,
            },
        ],
    };
    Ok(serde_json::to_vec(&request)?)
}

struct AiReviewHttpResponse {
    status: u16,
    body: String,
}

async fn perform_ai_review_http_attempt(
    client: &ureq::Agent,
    config: &AiReviewConfig,
    url: &str,
    api_key: &str,
    request_body: Vec<u8>,
    attempt: usize,
    request_timeout: Duration,
) -> AppResult<AiReviewHttpResponse> {
    let client = client.clone();
    let review_id = config.id.clone();
    let model = config.model.clone();
    let worker_review_id = review_id.clone();
    let worker_model = model.clone();
    let url = url.to_string();
    let api_key = api_key.to_string();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    thread::Builder::new()
        .name(format!("ai-review-http-{attempt}"))
        .spawn(move || {
            let result = perform_ai_review_http_attempt_blocking(AiReviewHttpAttempt {
                client,
                review_id: worker_review_id.clone(),
                model: worker_model.clone(),
                url,
                api_key,
                request_body,
                attempt,
                request_timeout,
            });
            let result_sent = sender.send(result).is_ok();
            info!(
                ai_review_id = %worker_review_id,
                model = %worker_model,
                attempt,
                result_sent,
                "AI review blocking HTTP worker result sent"
            );
        })
        .map_err(|err| {
            AppError::AiReview(format!(
                "AI review {} failed to spawn blocking HTTP worker: {err}",
                config.id
            ))
        })?;

    let response = receiver.await.map_err(|err| {
        AppError::AiReview(format!(
            "AI review {} blocking HTTP worker dropped result channel: {err}",
            config.id
        ))
    })??;
    info!(
        ai_review_id = %review_id,
        model = %model,
        attempt,
        status = response.status,
        response_bytes = response.body.len(),
        "AI review blocking HTTP task completed"
    );
    Ok(response)
}

struct AiReviewHttpAttempt {
    client: ureq::Agent,
    review_id: String,
    model: String,
    url: String,
    api_key: String,
    request_body: Vec<u8>,
    attempt: usize,
    request_timeout: Duration,
}

fn perform_ai_review_http_attempt_blocking(
    attempt_context: AiReviewHttpAttempt,
) -> AppResult<AiReviewHttpResponse> {
    let AiReviewHttpAttempt {
        client,
        review_id,
        model,
        url,
        api_key,
        request_body,
        attempt,
        request_timeout,
    } = attempt_context;
    let started = Instant::now();
    let response = match ureq_response_from_result(
        client
            .post(&url)
            .set("authorization", &format!("Bearer {api_key}"))
            .set("content-type", "application/json")
            .timeout(request_timeout)
            .send_bytes(&request_body),
    ) {
        Ok(response) => response,
        Err(err) => {
            warn!(
                ai_review_id = %review_id,
                model = %model,
                attempt,
                elapsed_ms = started.elapsed().as_millis(),
                error = %err,
                "AI review blocking API request failed before response headers"
            );
            return Err(err);
        }
    };

    let status = response.status();
    info!(
        ai_review_id = %review_id,
        model = %model,
        attempt,
        status,
        elapsed_ms = started.elapsed().as_millis(),
        "AI review blocking API response headers received"
    );

    let body_started = Instant::now();
    let body = match response.into_string() {
        Ok(body) => body,
        Err(err) => {
            warn!(
                ai_review_id = %review_id,
                model = %model,
                attempt,
                status,
                elapsed_ms = started.elapsed().as_millis(),
                error = %err,
                "AI review blocking API response body read failed"
            );
            return Err(AppError::AiReview(format!(
                "AI review blocking API response body read failed: {err}"
            )));
        }
    };
    info!(
        ai_review_id = %review_id,
        model = %model,
        attempt,
        response_bytes = body.len(),
        elapsed_ms = body_started.elapsed().as_millis(),
        "AI review blocking API response body received"
    );

    Ok(AiReviewHttpResponse { status, body })
}

fn shared_ai_http_client() -> AppResult<&'static ureq::Agent> {
    AI_HTTP_CLIENT
        .get_or_init(|| {
            Ok(ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(120))
                .timeout_write(Duration::from_secs(120))
                .build())
        })
        .as_ref()
        .map_err(|err| AppError::AiReview(format!("failed to build shared AI HTTP client: {err}")))
}

fn effective_request_timeout_seconds(config: &AiReviewConfig) -> u64 {
    config
        .request_timeout_seconds
        .unwrap_or_else(|| config.timeout_seconds.max(1).div_ceil(2))
        .max(1)
}

fn ureq_response_from_result(
    result: Result<ureq::Response, ureq::Error>,
) -> AppResult<ureq::Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(_, response)) => Ok(response),
        Err(err) => Err(AppError::AiReview(format!(
            "AI review blocking API request failed before response headers: {err}"
        ))),
    }
}

fn is_retryable_ai_error(err: &AppError) -> bool {
    match err {
        AppError::Reqwest(err) => {
            err.is_timeout() || err.is_connect() || err.is_request() || err.is_body()
        }
        AppError::AiReview(message) => {
            message.contains("blocking API request failed")
                || message.contains("blocking API response body read failed")
                || message.contains("blocking HTTP task failed")
        }
        _ => false,
    }
}

fn limited_diff_payload(changes: &[GitLabChange], max_bytes: usize) -> (String, bool) {
    let mut output = String::new();
    for change in changes {
        output.push_str(&change_diff_payload(change));
    }
    let limit = max_bytes.max(1);
    if output.len() <= limit {
        return (output, false);
    }
    let mut end = limit;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    (output[..end].to_string(), true)
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

fn change_diff_payload(change: &GitLabChange) -> String {
    format!(
        "File: {}\nOld path: {}\nNew file: {}\nRenamed: {}\nDeleted: {}\n```diff\n{}\n```\n\n",
        change.new_path,
        change.old_path,
        change.new_file,
        change.renamed_file,
        change.deleted_file,
        change.diff
    )
}

fn parse_openai_response(review_id: &str, title: &str, text: &str) -> AppResult<Vec<Finding>> {
    let response: OpenAiChatResponse = serde_json::from_str(text)?;
    let content = response
        .choices
        .first()
        .map(|choice| choice.message.content.trim())
        .ok_or_else(|| AppError::AiReview("AI review API returned no choices".into()))?;
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
    use crate::{gitlab::GitLabChange, rules::Severity};
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
        let config = AiReviewConfig {
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
            when_changed: vec![],
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

        assert!(prompt.contains("最终返回的 JSON"));
        assert!(prompt.contains("不要把问题只写在 reasoning_content"));
    }

    #[test]
    fn serializes_review_request_body_with_prompt() {
        let config = AiReviewConfig {
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
            when_changed: vec![],
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
        let body = serialize_review_request_body(&config, &prompt).unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["model"], "test-model");
        assert_eq!(json["messages"][1]["content"], prompt);
        assert_eq!(json["response_format"]["type"], "json_object");
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
            auto_enabled: true,
            id: "ai-review".into(),
            title: "AI Review".into(),
            base_url: format!("http://{}", addr),
            api_key: "test-key".into(),
            model: "test-model".into(),
            timeout_seconds: 2,
            request_timeout_seconds: None,
            max_diff_bytes: 60_000,
            second_pass_on_clean: false,
            batch_review: false,
            max_batch_diff_bytes: 30_000,
            max_batches: 6,
            when_changed: vec![],
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
            auto_enabled: true,
            id: "ai-review".into(),
            title: "AI Review".into(),
            base_url: format!("http://{}", addr),
            api_key: "test-key".into(),
            model: "test-model".into(),
            timeout_seconds: 2,
            request_timeout_seconds: Some(3),
            max_diff_bytes: 60_000,
            second_pass_on_clean: false,
            batch_review: false,
            max_batch_diff_bytes: 30_000,
            max_batches: 6,
            when_changed: vec![],
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
