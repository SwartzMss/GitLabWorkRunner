use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub(crate) struct OpenAiChatRequest<'a> {
    pub(crate) model: &'a str,
    pub(crate) temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_format: Option<ResponseFormat<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_choice: Option<OpenAiToolChoice>,
    pub(crate) messages: &'a [ChatMessage],
}

#[derive(Serialize)]
pub(crate) struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    pub(crate) response_type: &'a str,
}

#[derive(Serialize)]
pub(crate) struct OpenAiTool {
    #[serde(rename = "type")]
    pub(crate) tool_type: &'static str,
    pub(crate) function: OpenAiToolFunction,
}

#[derive(Serialize)]
pub(crate) struct OpenAiToolFunction {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) parameters: serde_json::Value,
}

#[derive(Serialize)]
pub(crate) struct OpenAiToolChoice {
    #[serde(rename = "type")]
    pub(crate) choice_type: &'static str,
    pub(crate) function: OpenAiToolChoiceFunction,
}

#[derive(Serialize)]
pub(crate) struct OpenAiToolChoiceFunction {
    pub(crate) name: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChatMessage {
    pub(crate) role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_calls: Option<Vec<OpenAiToolCall>>,
}

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

#[derive(Deserialize)]
pub(crate) struct OpenAiMessage {
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<OpenAiToolCall>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OpenAiToolCall {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(rename = "type", default = "default_tool_call_type")]
    pub(crate) call_type: String,
    pub(crate) function: OpenAiToolCallFunction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OpenAiToolCallFunction {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Deserialize)]
pub(crate) struct AiFindingsResponse {
    #[serde(default)]
    pub(crate) findings: Vec<AiFinding>,
}

#[derive(Deserialize)]
pub(crate) struct AiFinding {
    pub(crate) path: String,
    pub(crate) line: u32,
    #[serde(default)]
    pub(crate) severity: String,
    #[serde(default)]
    pub(crate) title: String,
    pub(crate) message: String,
}

fn default_tool_call_type() -> String {
    "function".into()
}
