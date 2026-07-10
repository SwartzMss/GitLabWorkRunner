use crate::error::AppResult;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Clone, Debug, Deserialize)]
pub struct RulesFile {
    #[serde(default)]
    pub ai_review: AiReviewPromptConfig,
    #[serde(default)]
    pub script_tasks: Vec<ScriptTaskConfig>,
    #[serde(default)]
    pub ai_reviews: Vec<AiReviewConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScriptTaskConfig {
    pub id: String,
    pub title: String,
    pub command: String,
    #[serde(default = "default_script_timeout_seconds")]
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AiReviewConfig {
    pub id: String,
    pub title: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_ai_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub request_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub second_pass_on_clean: bool,
    #[serde(default = "default_ai_max_batch_diff_bytes")]
    pub max_batch_diff_bytes: usize,
    #[serde(default = "default_ai_max_batches")]
    pub max_batches: usize,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub extra_instructions: String,
    #[serde(default = "default_ai_max_tool_calls")]
    pub max_tool_calls: usize,
    #[serde(default = "default_ai_max_tool_result_bytes")]
    pub max_tool_result_bytes: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AiReviewPromptConfig {
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub extra_instructions: String,
    #[serde(default = "default_ai_max_tool_calls")]
    pub max_tool_calls: usize,
    #[serde(default = "default_ai_max_tool_result_bytes")]
    pub max_tool_result_bytes: usize,
}

impl Default for AiReviewPromptConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            extra_instructions: String::new(),
            max_tool_calls: default_ai_max_tool_calls(),
            max_tool_result_bytes: default_ai_max_tool_result_bytes(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub rule_id: String,
    pub severity: Severity,
    pub path: String,
    pub new_line: Option<u32>,
    pub title: String,
    pub message: String,
}

#[derive(Clone)]
pub struct CompiledScriptTask {
    pub config: ScriptTaskConfig,
}

#[derive(Clone)]
pub struct CompiledAiReview {
    pub config: AiReviewConfig,
}

pub struct Ruleset {
    hash: String,
    script_tasks: Vec<CompiledScriptTask>,
    ai_reviews: Vec<CompiledAiReview>,
}

impl Ruleset {
    pub fn from_path(path: impl AsRef<Path>) -> AppResult<Self> {
        let text = fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    pub fn from_toml(text: &str) -> AppResult<Self> {
        let parsed: RulesFile = toml::from_str(text)?;
        let mut script_tasks = Vec::new();
        for config in parsed.script_tasks {
            script_tasks.push(CompiledScriptTask { config });
        }
        let mut ai_reviews = Vec::new();
        for mut config in parsed.ai_reviews {
            config.system_prompt = parsed.ai_review.system_prompt.clone();
            config.extra_instructions = parsed.ai_review.extra_instructions.clone();
            config.max_tool_calls = parsed.ai_review.max_tool_calls;
            config.max_tool_result_bytes = parsed.ai_review.max_tool_result_bytes;
            ai_reviews.push(CompiledAiReview { config });
        }
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        Ok(Self {
            hash,
            script_tasks,
            ai_reviews,
        })
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn script_task_count(&self) -> usize {
        self.script_tasks.len()
    }

    pub fn ai_review_count(&self) -> usize {
        self.ai_reviews.len()
    }

    pub fn script_tasks_by_ids(&self, requested_ids: &[String]) -> Vec<ScriptTaskConfig> {
        self.script_tasks
            .iter()
            .filter(|task| requested_ids.iter().any(|id| id == &task.config.id))
            .map(|task| task.config.clone())
            .collect()
    }

    pub fn ai_reviews_by_ids(&self, requested_ids: &[String]) -> Vec<AiReviewConfig> {
        self.ai_reviews
            .iter()
            .filter(|review| requested_ids.iter().any(|id| id == &review.config.id))
            .map(|review| review.config.clone())
            .collect()
    }
}

fn default_script_timeout_seconds() -> u64 {
    60
}

fn default_ai_timeout_seconds() -> u64 {
    60
}

fn default_ai_max_batch_diff_bytes() -> usize {
    30_000
}

fn default_ai_max_batches() -> usize {
    6
}

fn default_ai_max_tool_calls() -> usize {
    30
}

fn default_ai_max_tool_result_bytes() -> usize {
    60_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_when_rules_file_text_changes() {
        let first = Ruleset::from_toml(
            r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "https://api.openai.com/v1"
api_key = "test-api-key"
model = "gpt-4.1-mini"
"#,
        )
        .unwrap();
        let second = Ruleset::from_toml(
            r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "https://api.openai.com/v1"
api_key = "other-test-api-key"
model = "gpt-4.1-mini"
"#,
        )
        .unwrap();

        assert_ne!(first.hash(), second.hash());
    }

    #[test]
    fn script_tasks_are_selected_by_manual_ids() {
        let rules = Ruleset::from_toml(
            r#"
[[script_tasks]]
id = "manual-check"
title = "Manual check"
command = "python check.py"
"#,
        )
        .unwrap();

        assert_eq!(rules.script_tasks_by_ids(&["manual-check".into()]).len(), 1);
        assert!(rules.script_tasks_by_ids(&["other".into()]).is_empty());
    }

    #[test]
    fn parses_ai_review_defaults_and_selects_by_manual_ids() {
        let rules = Ruleset::from_toml(
            r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "https://api.openai.com/v1"
api_key = "test-api-key"
model = "gpt-4.1-mini"
"#,
        )
        .unwrap();

        let reviews = rules.ai_reviews_by_ids(&["ai-review".into()]);

        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].id, "ai-review");
        assert_eq!(reviews[0].api_key, "test-api-key");
        assert_eq!(reviews[0].timeout_seconds, 60);
        assert_eq!(reviews[0].request_timeout_seconds, None);
        assert!(!reviews[0].second_pass_on_clean);
        assert_eq!(reviews[0].max_batch_diff_bytes, 30_000);
        assert_eq!(reviews[0].max_batches, 6);
        assert_eq!(reviews[0].system_prompt, None);
        assert!(reviews[0].extra_instructions.is_empty());
        assert_eq!(reviews[0].max_tool_calls, 30);
        assert_eq!(reviews[0].max_tool_result_bytes, 60_000);
    }

    #[test]
    fn parses_ai_review_request_timeout_seconds() {
        let rules = Ruleset::from_toml(
            r#"
[ai_review]
system_prompt = "Custom system prompt"
extra_instructions = "Focus on C++ lifetime bugs."
max_tool_calls = 4
max_tool_result_bytes = 12000

[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "https://api.openai.com/v1"
api_key = "test-api-key"
model = "gpt-4.1-mini"
timeout_seconds = 180
request_timeout_seconds = 90
max_batch_diff_bytes = 30000
max_batches = 6
"#,
        )
        .unwrap();

        let reviews = rules.ai_reviews_by_ids(&["ai-review".into()]);

        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].timeout_seconds, 180);
        assert_eq!(reviews[0].request_timeout_seconds, Some(90));
        assert_eq!(reviews[0].max_batch_diff_bytes, 30_000);
        assert_eq!(reviews[0].max_batches, 6);
        assert_eq!(
            reviews[0].system_prompt.as_deref(),
            Some("Custom system prompt")
        );
        assert_eq!(reviews[0].extra_instructions, "Focus on C++ lifetime bugs.");
        assert_eq!(reviews[0].max_tool_calls, 4);
        assert_eq!(reviews[0].max_tool_result_bytes, 12_000);
    }

    #[test]
    fn unknown_manual_ai_review_id_is_not_selected() {
        let rules = Ruleset::from_toml(
            r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "https://api.openai.com/v1"
api_key = "test-api-key"
model = "gpt-4.1-mini"
"#,
        )
        .unwrap();

        assert!(rules.ai_reviews_by_ids(&["other".into()]).is_empty());
        assert_eq!(rules.ai_reviews_by_ids(&["ai-review".into()]).len(), 1);
    }
}
