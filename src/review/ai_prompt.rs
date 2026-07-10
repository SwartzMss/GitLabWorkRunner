use crate::{gitlab::GitLabChange, rules::AiReviewConfig};

use super::ai_schema::ChatMessage;

#[derive(Clone, Debug)]
pub(crate) struct FormattedChangePayload {
    pub content: String,
    pub total_payload_bytes: usize,
    pub diff_start: usize,
    pub diff_end: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct ReviewedFilePayload {
    pub path: String,
    pub total_diff_bytes: usize,
    pub reviewed_diff_bytes: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct LimitedDiffPayload {
    pub content: String,
    pub files: Vec<ReviewedFilePayload>,
    pub truncated: bool,
}

const SYSTEM_PROMPT: &str = "You are a concise code reviewer. Review only added lines in the provided GitLab merge request diff. Return strict JSON only, with a top-level findings array. Do not include markdown.";

#[cfg(test)]
pub(crate) fn build_review_prompt(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
    review_request: Option<&str>,
) -> (String, usize, bool) {
    build_review_prompt_with_limit(config, changes, config.max_diff_bytes, review_request)
}

pub(crate) fn build_review_prompt_with_limit(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
    max_diff_bytes: usize,
    review_request: Option<&str>,
) -> (String, usize, bool) {
    let limited = limited_diff_payload_details(changes, max_diff_bytes);
    let diff_payload_bytes = limited.content.len();
    let truncated_note = if limited.truncated {
        "\ndiff 内容因为超过配置的字节限制已被截断。"
    } else {
        ""
    };
    let response_instruction = "优先调用 submit_review_findings tool 提交最终结果，arguments 格式必须是 {\"findings\":[{\"path\":\"src/file.rs\",\"line\":123,\"severity\":\"error\",\"title\":\"简短中文标题\",\"message\":\"具体说明为什么这是错误，以及应该如何修复。\"}]}。如果服务没有返回 tool_calls，则最终 content 必须只包含同样格式的 JSON，不能在 JSON 前后添加解释、Markdown 或其他文字。如果没有确定的错误，提交或返回 {\"findings\":[]}。不要把最终结果只写在 reasoning_content、分析过程或其他非 tool arguments/content 字段里。";
    let context_tool_instruction = context_tool_instruction(config);
    let extra_instructions = config.extra_instructions.trim();
    let extra_instructions = if extra_instructions.is_empty() {
        String::new()
    } else {
        format!("\n\n额外审查要求：\n{extra_instructions}")
    };
    let review_request = review_request.unwrap_or("").trim();
    let review_request = if review_request.is_empty() {
        String::new()
    } else {
        format!("\n\n本次触发说明：\n{review_request}")
    };
    let prompt = format!(
        "请用中文审查这个 GitLab Merge Request diff。只报告高置信度错误，例如会导致编译失败、运行时错误、数据损坏、安全漏洞或明显错误逻辑的问题。不要报告风格建议、可维护性建议、命名问题、性能微优化或不确定的问题。{response_instruction}severity 必须固定为 \"error\"。只能使用 diff 新增行的行号。{context_tool_instruction}{extra_instructions}{review_request}{truncated_note}\n\n{}",
        limited.content,
    );
    (prompt, diff_payload_bytes, limited.truncated)
}

fn context_tool_instruction(config: &AiReviewConfig) -> String {
    if config.max_tool_calls == 0 || !config.context_tools.any_enabled() {
        return String::new();
    }
    let mut tools = Vec::new();
    if config.context_tools.list_files {
        tools.push("list_files");
    }
    if config.context_tools.search_code {
        tools.push("search_code");
    }
    if config.context_tools.read_file {
        tools.push("read_file");
    }
    format!(
        "\n\n上下文工具已启用：{}。只有当 diff 信息不足以判断真实 bug 时才调用工具；不要为了风格、命名、微优化或不确定猜测调用工具。优先用 list_files/search_code 定位相关文件或符号，再用 read_file 读取必要文件。工具结果只能作为确认依据，最终仍然只提交高置信度、位于新增行的错误。",
        tools.join(", ")
    )
}

pub(crate) fn initial_chat_messages(config: &AiReviewConfig, prompt: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage {
            role: "system".into(),
            content: Some(configured_system_prompt(config).to_string()),
            tool_call_id: None,
            tool_calls: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(prompt.to_string()),
            tool_call_id: None,
            tool_calls: None,
        },
    ]
}

pub(crate) fn configured_system_prompt(config: &AiReviewConfig) -> &str {
    config
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .unwrap_or(SYSTEM_PROMPT)
}

#[cfg(test)]
pub(crate) fn limited_diff_payload(changes: &[GitLabChange], max_bytes: usize) -> (String, bool) {
    let payload = limited_diff_payload_details(changes, max_bytes);
    (payload.content, payload.truncated)
}

pub(crate) fn limited_diff_payload_details(
    changes: &[GitLabChange],
    max_bytes: usize,
) -> LimitedDiffPayload {
    let mut output = String::new();
    let mut truncated = false;
    let mut files = Vec::new();
    for change in changes {
        if output.len() >= max_bytes {
            files.push(skipped_reviewed_file_payload(change));
            truncated = true;
            continue;
        }
        let formatted = formatted_change_payload(change);
        let remaining = max_bytes.saturating_sub(output.len());
        let mut end = remaining.min(formatted.total_payload_bytes);
        while end > 0 && !formatted.content.is_char_boundary(end) {
            end -= 1;
        }
        output.push_str(&formatted.content[..end]);
        let reviewed_diff_bytes = end
            .min(formatted.diff_end)
            .saturating_sub(formatted.diff_start.min(end));
        let file_truncated = end < formatted.total_payload_bytes;
        files.push(ReviewedFilePayload {
            path: change.new_path.clone(),
            total_diff_bytes: change.diff.len(),
            reviewed_diff_bytes,
            truncated: file_truncated,
        });
        if file_truncated {
            truncated = true;
        }
    }
    LimitedDiffPayload {
        content: output,
        files,
        truncated,
    }
}

fn skipped_reviewed_file_payload(change: &GitLabChange) -> ReviewedFilePayload {
    ReviewedFilePayload {
        path: change.new_path.clone(),
        total_diff_bytes: change.diff.len(),
        reviewed_diff_bytes: 0,
        truncated: true,
    }
}

#[cfg(test)]
pub(crate) fn change_diff_payload(change: &GitLabChange) -> String {
    formatted_change_payload(change).content
}

pub(crate) fn formatted_change_payload(change: &GitLabChange) -> FormattedChangePayload {
    let prefix = format!(
        "File: {}\nOld path: {}\nNew file: {}\nRenamed: {}\nDeleted: {}\n```diff\n",
        change.new_path, change.old_path, change.new_file, change.renamed_file, change.deleted_file,
    );
    let diff_start = prefix.len();
    let diff_end = diff_start + change.diff.len();
    let content = format!("{prefix}{}\n```\n\n", change.diff);
    FormattedChangePayload {
        total_payload_bytes: content.len(),
        content,
        diff_start,
        diff_end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_file_metadata_does_not_require_a_formatted_payload() {
        let change = GitLabChange {
            old_path: "src/large.rs".into(),
            new_path: "src/large.rs".into(),
            new_file: false,
            renamed_file: false,
            deleted_file: false,
            diff: "+large diff\n".repeat(100),
        };

        let file = skipped_reviewed_file_payload(&change);

        assert_eq!(file.path, "src/large.rs");
        assert_eq!(file.total_diff_bytes, change.diff.len());
        assert_eq!(file.reviewed_diff_bytes, 0);
        assert!(file.truncated);
    }
}
