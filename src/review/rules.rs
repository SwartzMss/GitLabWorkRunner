use crate::error::AppResult;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesFile {
    #[serde(default)]
    pub ai_review: AiReviewPromptConfig,
    #[serde(default)]
    pub ai_reviews: Vec<AiReviewConfig>,
    #[serde(default)]
    pub script_tasks: Vec<ScriptTaskConfig>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptTaskConfig {
    #[serde(default)]
    pub enabled: bool,
    pub id: String,
    pub title: String,
    pub command: String,
    #[serde(default = "default_script_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_script_max_concurrent")]
    pub max_concurrent: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AiReviewConfig {
    #[serde(default)]
    pub enabled: bool,
    pub id: String,
    pub title: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_ai_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub request_timeout_seconds: Option<u64>,
    #[serde(default = "default_ai_max_batch_diff_bytes")]
    pub max_batch_diff_bytes: usize,
    #[serde(default = "default_ai_max_batches")]
    pub max_batches: usize,
    #[serde(default)]
    pub extra_instructions: String,
    #[serde(default = "default_ai_max_tool_calls")]
    pub max_tool_calls: usize,
    #[serde(default = "default_ai_max_tool_rounds")]
    pub max_tool_rounds: usize,
    #[serde(default = "default_ai_max_tool_result_bytes")]
    pub max_tool_result_bytes: usize,
    #[serde(default = "default_ai_max_tool_total_bytes")]
    pub max_tool_total_bytes: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AiReviewPromptConfig {
    #[serde(default = "default_max_concurrent_reviews")]
    pub max_concurrent_reviews: usize,
    #[serde(default)]
    pub extra_instructions: String,
    #[serde(default = "default_ai_max_tool_calls")]
    pub max_tool_calls: usize,
    #[serde(default = "default_ai_max_tool_rounds")]
    pub max_tool_rounds: usize,
    #[serde(default = "default_ai_max_tool_result_bytes")]
    pub max_tool_result_bytes: usize,
    #[serde(default = "default_ai_max_tool_total_bytes")]
    pub max_tool_total_bytes: usize,
}

impl Default for AiReviewPromptConfig {
    fn default() -> Self {
        Self {
            max_concurrent_reviews: default_max_concurrent_reviews(),
            extra_instructions: String::new(),
            max_tool_calls: default_ai_max_tool_calls(),
            max_tool_rounds: default_ai_max_tool_rounds(),
            max_tool_result_bytes: default_ai_max_tool_result_bytes(),
            max_tool_total_bytes: default_ai_max_tool_total_bytes(),
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
pub struct CompiledAiReview {
    pub config: AiReviewConfig,
}

pub struct Ruleset {
    hash: String,
    ai_reviews: Vec<CompiledAiReview>,
    script_tasks: Vec<ScriptTaskConfig>,
    max_concurrent_reviews: usize,
}

impl Ruleset {
    pub fn from_path(path: impl AsRef<Path>) -> AppResult<Self> {
        let text = fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    pub fn from_toml(text: &str) -> AppResult<Self> {
        let parsed: RulesFile = toml::from_str(text)?;
        let mut ids = std::collections::HashSet::new();
        for id in parsed
            .ai_reviews
            .iter()
            .map(|review| review.id.as_str())
            .chain(parsed.script_tasks.iter().map(|task| task.id.as_str()))
        {
            if !ids.insert(id) {
                return Err(crate::error::AppError::Config(format!(
                    "duplicate task id `{id}` across ai_reviews and script_tasks"
                )));
            }
        }
        for task in &parsed.script_tasks {
            if task.id.trim().is_empty()
                || task.title.trim().is_empty()
                || task.command.trim().is_empty()
            {
                return Err(crate::error::AppError::Config(
                    "script task id, title, and command must not be empty".into(),
                ));
            }
            if task.id == "."
                || task.id == ".."
                || task.id.ends_with('.')
                || !task
                    .id
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
            {
                return Err(crate::error::AppError::Config(format!(
                    "script task id `{}` may contain only ASCII letters, digits, hyphens, underscores, and dots",
                    task.id
                )));
            }
            if task.max_concurrent == 0 {
                return Err(crate::error::AppError::Config(format!(
                    "script task `{}` max_concurrent must be at least 1",
                    task.id
                )));
            }
            if task.timeout_seconds == 0 {
                return Err(crate::error::AppError::Config(format!(
                    "script task `{}` timeout_seconds must be at least 1",
                    task.id
                )));
            }
        }
        let mut ai_reviews = Vec::new();
        for mut config in parsed
            .ai_reviews
            .into_iter()
            .filter(|config| config.enabled)
        {
            config.extra_instructions = parsed.ai_review.extra_instructions.clone();
            config.max_tool_calls = parsed.ai_review.max_tool_calls;
            config.max_tool_rounds = parsed.ai_review.max_tool_rounds;
            config.max_tool_result_bytes = parsed.ai_review.max_tool_result_bytes;
            config.max_tool_total_bytes = parsed.ai_review.max_tool_total_bytes;
            ai_reviews.push(CompiledAiReview { config });
        }
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        Ok(Self {
            hash,
            ai_reviews,
            script_tasks: parsed
                .script_tasks
                .into_iter()
                .filter(|task| task.enabled)
                .collect(),
            max_concurrent_reviews: parsed.ai_review.max_concurrent_reviews.max(1),
        })
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn ai_review_count(&self) -> usize {
        self.ai_reviews.len()
    }

    pub fn script_task_count(&self) -> usize {
        self.script_tasks.len()
    }

    pub fn max_concurrent_reviews(&self) -> usize {
        self.max_concurrent_reviews
    }

    pub fn script_tasks_by_ids(&self, requested_ids: &[String]) -> Vec<ScriptTaskConfig> {
        self.script_tasks
            .iter()
            .filter(|task| requested_ids.iter().any(|id| id == &task.id))
            .cloned()
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
fn default_script_max_concurrent() -> usize {
    1
}
fn default_max_concurrent_reviews() -> usize {
    4
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

fn default_ai_max_tool_rounds() -> usize {
    0
}

fn default_ai_max_tool_result_bytes() -> usize {
    60_000
}

fn default_ai_max_tool_total_bytes() -> usize {
    40_000
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
    fn parses_multiple_script_tasks_with_independent_defaults_and_limits() {
        let rules = Ruleset::from_toml(
            r#"
[[script_tasks]]
enabled = true
id = "manual-check"
title = "Manual check"
command = "python check.py"

[[script_tasks]]
enabled = true
id = "security-scan"
title = "Security scan"
command = "./scan.sh"
timeout_seconds = 120
max_concurrent = 3
"#,
        )
        .unwrap();

        assert_eq!(rules.script_task_count(), 2);
        let first = rules.script_tasks_by_ids(&["manual-check".into()]);
        assert_eq!(first[0].timeout_seconds, 60);
        assert_eq!(first[0].max_concurrent, 1);
        let second = rules.script_tasks_by_ids(&["security-scan".into()]);
        assert_eq!(second[0].timeout_seconds, 120);
        assert_eq!(second[0].max_concurrent, 3);
    }

    #[test]
    fn ai_reviews_and_script_tasks_require_explicit_enablement() {
        let disabled = Ruleset::from_toml(
            r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "https://api.openai.com/v1"
api_key = "test-api-key"
model = "gpt-4.1-mini"

[[script_tasks]]
id = "lint"
title = "Lint"
command = "./lint.sh"
"#,
        )
        .unwrap();

        assert_eq!(disabled.ai_review_count(), 0);
        assert_eq!(disabled.script_task_count(), 0);
        assert!(disabled.ai_reviews_by_ids(&["ai-review".into()]).is_empty());
        assert!(disabled.script_tasks_by_ids(&["lint".into()]).is_empty());

        let enabled = Ruleset::from_toml(
            r#"
[[ai_reviews]]
enabled = true
id = "ai-review"
title = "AI Review"
base_url = "https://api.openai.com/v1"
api_key = "test-api-key"
model = "gpt-4.1-mini"

[[script_tasks]]
enabled = true
id = "lint"
title = "Lint"
command = "./lint.sh"
"#,
        )
        .unwrap();

        assert_eq!(enabled.ai_review_count(), 1);
        assert_eq!(enabled.script_task_count(), 1);
        assert_eq!(enabled.ai_reviews_by_ids(&["ai-review".into()]).len(), 1);
        assert_eq!(enabled.script_tasks_by_ids(&["lint".into()]).len(), 1);
    }

    #[test]
    fn script_and_ai_task_ids_must_be_unique() {
        let error = Ruleset::from_toml(
            r#"
[[ai_reviews]]
id = "check"
title = "AI check"
base_url = "https://example.com/v1"
api_key = "key"
model = "model"

[[script_tasks]]
id = "check"
title = "Script check"
command = "./check.sh"
"#,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("duplicate task id `check`"));
    }

    #[test]
    fn script_task_rejects_zero_concurrency() {
        let error = Ruleset::from_toml(
            r#"
[[script_tasks]]
id = "check"
title = "Check"
command = "./check.sh"
max_concurrent = 0
"#,
        )
        .err()
        .unwrap();
        assert!(error
            .to_string()
            .contains("max_concurrent must be at least 1"));
    }

    #[test]
    fn script_task_rejects_zero_timeout_and_unsafe_ids() {
        for config in [
            "id = \"check\"\ntitle = \"Check\"\ncommand = \"./check.sh\"\ntimeout_seconds = 0",
            "id = \"../check\"\ntitle = \"Check\"\ncommand = \"./check.sh\"",
            "id = \"check.\"\ntitle = \"Check\"\ncommand = \"./check.sh\"",
        ] {
            let error = Ruleset::from_toml(&format!("[[script_tasks]]\n{config}\n"))
                .err()
                .unwrap();
            assert!(error.to_string().contains("script task"));
        }
    }

    #[test]
    fn second_pass_on_clean_is_rejected_as_unknown_field() {
        let error = match Ruleset::from_toml(
            r#"
[[ai_reviews]]
id = "ai-review"
title = "AI Review"
base_url = "https://api.openai.com/v1"
api_key = "test-api-key"
model = "gpt-4.1-mini"
second_pass_on_clean = true
"#,
        ) {
            Ok(_) => panic!("removed second_pass_on_clean unexpectedly parsed"),
            Err(error) => error,
        };

        assert!(error
            .to_string()
            .contains("unknown field `second_pass_on_clean`"));
    }

    #[test]
    fn parses_ai_review_defaults_and_selects_by_manual_ids() {
        let rules = Ruleset::from_toml(
            r#"
[[ai_reviews]]
enabled = true
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
        assert_eq!(reviews[0].max_batch_diff_bytes, 30_000);
        assert_eq!(reviews[0].max_batches, 6);
        assert!(reviews[0].extra_instructions.is_empty());
        assert_eq!(reviews[0].max_tool_calls, 30);
        assert_eq!(reviews[0].max_tool_rounds, 0);
        assert_eq!(reviews[0].max_tool_result_bytes, 60_000);
        assert_eq!(reviews[0].max_tool_total_bytes, 40_000);
    }

    #[test]
    fn parses_ai_review_request_timeout_seconds() {
        let rules = Ruleset::from_toml(
            r#"
[ai_review]
max_concurrent_reviews = 2
extra_instructions = "Focus on C++ lifetime bugs."
max_tool_calls = 4
max_tool_rounds = 2
max_tool_result_bytes = 12000
max_tool_total_bytes = 12345

[[ai_reviews]]
enabled = true
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

        assert_eq!(rules.max_concurrent_reviews(), 2);
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].timeout_seconds, 180);
        assert_eq!(reviews[0].request_timeout_seconds, Some(90));
        assert_eq!(reviews[0].max_batch_diff_bytes, 30_000);
        assert_eq!(reviews[0].max_batches, 6);
        assert_eq!(reviews[0].extra_instructions, "Focus on C++ lifetime bugs.");
        assert_eq!(reviews[0].max_tool_calls, 4);
        assert_eq!(reviews[0].max_tool_rounds, 2);
        assert_eq!(reviews[0].max_tool_result_bytes, 12_000);
        assert_eq!(reviews[0].max_tool_total_bytes, 12_345);
    }

    #[test]
    fn parses_checked_in_rules_example() {
        let rules = Ruleset::from_toml(include_str!("../../rules.example.toml")).unwrap();

        assert_eq!(rules.ai_review_count(), 1);
        assert_eq!(rules.max_concurrent_reviews(), 4);
        let reviews = rules.ai_reviews_by_ids(&["ai-review".into()]);
        assert_eq!(reviews.len(), 1);
        assert!(reviews[0].extra_instructions.is_empty());
        assert_eq!(reviews[0].max_tool_calls, 35);
        assert_eq!(reviews[0].max_tool_rounds, 0);
        assert_eq!(reviews[0].max_tool_result_bytes, 60_000);
        assert_eq!(reviews[0].max_tool_total_bytes, 40_000);
    }

    #[test]
    fn unknown_manual_ai_review_id_is_not_selected() {
        let rules = Ruleset::from_toml(
            r#"
[[ai_reviews]]
enabled = true
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
