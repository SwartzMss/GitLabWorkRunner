use crate::{gitlab::GitLabChange, rules::AiReviewConfig};

use super::ai_schema::ChatMessage;

const SYSTEM_PROMPT: &str = "You are a concise code reviewer. Review only added lines in the provided GitLab merge request diff. Return strict JSON only, with a top-level findings array. Do not include markdown.";

#[cfg(test)]
pub(crate) fn build_review_prompt(
    config: &AiReviewConfig,
    changes: &[GitLabChange],
) -> (String, usize, bool) {
    build_review_prompt_with_limit(config, changes, config.max_diff_bytes)
}

pub(crate) fn build_review_prompt_with_limit(
    config: &AiReviewConfig,
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
    let response_instruction = "优先调用 submit_review_findings tool 提交最终结果，arguments 格式必须是 {\"findings\":[{\"path\":\"src/file.rs\",\"line\":123,\"severity\":\"error\",\"title\":\"简短中文标题\",\"message\":\"具体说明为什么这是错误，以及应该如何修复。\"}]}。如果服务没有返回 tool_calls，则最终 content 必须是同样格式的 JSON，且 JSON 是唯一有效输出。如果没有确定的错误，提交或返回 {\"findings\":[]}。不要把最终结果只写在 reasoning_content、分析过程或其他非 tool arguments/content 字段里。";
    let context_tool_instruction = context_tool_instruction(config);
    let extra_instructions = config.extra_instructions.trim();
    let extra_instructions = if extra_instructions.is_empty() {
        String::new()
    } else {
        format!("\n\n额外审查要求：\n{extra_instructions}")
    };
    let prompt = format!(
        "请用中文审查这个 GitLab Merge Request diff。只报告高置信度错误，例如会导致编译失败、运行时错误、数据损坏、安全漏洞或明显错误逻辑的问题。不要报告风格建议、可维护性建议、命名问题、性能微优化或不确定的问题。{response_instruction}severity 必须固定为 \"error\"。只能使用 diff 新增行的行号。{context_tool_instruction}{extra_instructions}{truncated_note}\n\n{diff_text}",
    );
    (prompt, diff_payload_bytes, truncated)
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

pub(crate) fn limited_diff_payload(changes: &[GitLabChange], max_bytes: usize) -> (String, bool) {
    let mut output = String::new();
    let mut truncated = false;
    for change in changes {
        let fragment = change_diff_payload(change);
        if output.len() + fragment.len() <= max_bytes {
            output.push_str(&fragment);
            continue;
        }
        let remaining = max_bytes.saturating_sub(output.len());
        if remaining > 0 {
            let mut end = remaining;
            while end > 0 && !fragment.is_char_boundary(end) {
                end -= 1;
            }
            output.push_str(&fragment[..end]);
        }
        truncated = true;
        break;
    }
    (output, truncated)
}

pub(crate) fn change_diff_payload(change: &GitLabChange) -> String {
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
