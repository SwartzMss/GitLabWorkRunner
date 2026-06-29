use crate::{
    diff::{parse_unified_diff, DiffLineKind},
    error::{AppError, AppResult},
    gitlab::GitLabChange,
    rules::{AiReviewConfig, Finding, Severity},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
use tracing::{info, warn};

const AI_RESPONSE_PREVIEW_CHARS: usize = 1000;

pub async fn run_ai_review(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
) -> AppResult<Vec<Finding>> {
    let api_key = config.api_key.trim();
    if api_key.is_empty() {
        return Err(AppError::AiReview(format!(
            "api_key is empty for AI review {}",
            config.id
        )));
    }
    let (prompt, diff_payload_bytes, diff_payload_truncated) = build_review_prompt(config, changes);
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
                content: &prompt,
            },
        ],
    };
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    info!(
        ai_review_id = %config.id,
        model = %config.model,
        timeout_seconds = config.timeout_seconds,
        max_diff_bytes = config.max_diff_bytes,
        diff_payload_bytes,
        diff_payload_truncated,
        "calling AI review API"
    );
    let started = Instant::now();
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.timeout_seconds.max(1)))
        .build()?
        .post(url)
        .bearer_auth(api_key)
        .json(&request)
        .send()
        .await?;
    let status = response.status();
    info!(
        ai_review_id = %config.id,
        model = %config.model,
        status = status.as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        "AI review API response received"
    );
    let body = response.text().await?;
    let response_body_preview = preview_log_text(&body, AI_RESPONSE_PREVIEW_CHARS);
    info!(
        ai_review_id = %config.id,
        model = %config.model,
        response_bytes = body.len(),
        response_body_preview = %response_body_preview,
        "AI review raw response body received"
    );
    if !status.is_success() {
        return Err(AppError::AiReview(format!(
            "AI review API returned HTTP status {}: {}",
            status.as_u16(),
            response_body_preview
        )));
    }
    let findings = parse_openai_response(&config.id, &config.title, &body)?;
    let raw_finding_count = findings.len();
    let filtered = filter_findings_to_added_lines(changes, findings)?;
    info!(
        ai_review_id = %config.id,
        raw_findings = raw_finding_count,
        findings = filtered.len(),
        filtered_findings = raw_finding_count.saturating_sub(filtered.len()),
        "AI review API completed"
    );
    Ok(filtered)
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

fn build_review_prompt(config: &AiReviewConfig, changes: &[GitLabChange]) -> (String, usize, bool) {
    let (diff_text, truncated) = limited_diff_payload(changes, config.max_diff_bytes);
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

fn limited_diff_payload(changes: &[GitLabChange], max_bytes: usize) -> (String, bool) {
    let mut output = String::new();
    for change in changes {
        output.push_str(&format!(
            "File: {}\nOld path: {}\nNew file: {}\nRenamed: {}\nDeleted: {}\n```diff\n{}\n```\n\n",
            change.new_path,
            change.old_path,
            change.new_file,
            change.renamed_file,
            change.deleted_file,
            change.diff
        ));
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
            max_diff_bytes: 60_000,
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
    fn previews_ai_response_without_splitting_utf8() {
        let preview = preview_log_text("中文\nabcdef", 5);

        assert_eq!(preview, "中文\\nab...");
    }
}
