use crate::{
    diff::{DiffFile, DiffLineKind},
    error::{AppError, AppResult},
};
use globset::{Glob, GlobMatcher, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Clone, Debug, Deserialize)]
pub struct RulesFile {
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
    #[serde(default)]
    pub script_tasks: Vec<ScriptTaskConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RuleConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub id: String,
    pub title: String,
    pub severity: Severity,
    pub path: String,
    pub pattern: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScriptTaskConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub id: String,
    pub title: String,
    pub command: String,
    #[serde(default = "default_script_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub when_changed: Vec<String>,
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

struct CompiledRule {
    config: RuleConfig,
    matcher: GlobMatcher,
    regex: Regex,
}

#[derive(Clone)]
pub struct CompiledScriptTask {
    pub config: ScriptTaskConfig,
    changed_matcher: Option<GlobSet>,
}

pub struct Ruleset {
    hash: String,
    rules: Vec<CompiledRule>,
    script_tasks: Vec<CompiledScriptTask>,
}

impl Ruleset {
    pub fn from_path(path: impl AsRef<Path>) -> AppResult<Self> {
        let text = fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    pub fn from_toml(text: &str) -> AppResult<Self> {
        let parsed: RulesFile = toml::from_str(text)?;
        let mut rules = Vec::new();
        for config in parsed.rules.into_iter().filter(|config| config.enabled) {
            let matcher = Glob::new(&config.path)
                .map_err(|err| AppError::Rule(format!("invalid glob {}: {err}", config.path)))?
                .compile_matcher();
            let regex = Regex::new(&config.pattern).map_err(|err| {
                AppError::Rule(format!("invalid regex for rule {}: {err}", config.id))
            })?;
            rules.push(CompiledRule {
                config,
                matcher,
                regex,
            });
        }
        let mut script_tasks = Vec::new();
        for config in parsed.script_tasks {
            let changed_matcher = if config.when_changed.is_empty() {
                None
            } else {
                let mut builder = GlobSetBuilder::new();
                for pattern in &config.when_changed {
                    builder.add(Glob::new(pattern).map_err(|err| {
                        AppError::Rule(format!(
                            "invalid when_changed glob for script task {}: {err}",
                            config.id
                        ))
                    })?);
                }
                Some(builder.build().map_err(|err| {
                    AppError::Rule(format!(
                        "invalid when_changed glob set for script task {}: {err}",
                        config.id
                    ))
                })?)
            };
            script_tasks.push(CompiledScriptTask {
                config,
                changed_matcher,
            });
        }
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        Ok(Self {
            hash,
            rules,
            script_tasks,
        })
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn has_line_rules(&self) -> bool {
        !self.rules.is_empty()
    }

    pub fn script_tasks_for_changes(&self, changed_paths: &[String]) -> Vec<ScriptTaskConfig> {
        self.script_tasks
            .iter()
            .filter(|task| {
                task.config.enabled
                    && task.changed_matcher.as_ref().is_none_or(|matcher| {
                        changed_paths.iter().any(|path| matcher.is_match(path))
                    })
            })
            .map(|task| task.config.clone())
            .collect()
    }

    pub fn script_tasks_by_ids(&self, requested_ids: &[String]) -> Vec<ScriptTaskConfig> {
        self.script_tasks
            .iter()
            .filter(|task| requested_ids.iter().any(|id| id == &task.config.id))
            .map(|task| task.config.clone())
            .collect()
    }

    pub fn evaluate(&self, file: &DiffFile) -> Vec<Finding> {
        let mut findings = Vec::new();
        for rule in &self.rules {
            if !rule.matcher.is_match(&file.new_path) {
                continue;
            }
            for hunk in &file.hunks {
                for line in &hunk.lines {
                    if line.kind == DiffLineKind::Added && rule.regex.is_match(&line.content) {
                        findings.push(Finding {
                            rule_id: rule.config.id.clone(),
                            severity: rule.config.severity.clone(),
                            path: file.new_path.clone(),
                            new_line: line.new_line,
                            title: rule.config.title.clone(),
                            message: rule.config.message.clone(),
                        });
                    }
                }
            }
        }
        findings
    }
}

fn default_enabled() -> bool {
    true
}

fn default_script_timeout_seconds() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified_diff;

    #[test]
    fn matches_added_lines_only() {
        let rules = Ruleset::from_toml(
            r#"
[[rules]]
id = "forbid-unwrap"
title = "Avoid unwrap"
severity = "warning"
path = "**/*.rs"
pattern = "\\.unwrap\\(\\)"
message = "Do not unwrap."
"#,
        )
        .unwrap();
        let diff = "@@ -1,2 +1,2 @@\n-let a = old.unwrap();\n+let a = new.unwrap();\n";
        let file = parse_unified_diff("src/lib.rs", "src/lib.rs", diff).unwrap();

        let findings = rules.evaluate(&file);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "forbid-unwrap");
        assert_eq!(findings[0].new_line, Some(1));
    }

    #[test]
    fn ignores_non_matching_paths() {
        let rules = Ruleset::from_toml(
            r#"
[[rules]]
id = "forbid-unwrap"
title = "Avoid unwrap"
severity = "warning"
path = "**/*.rs"
pattern = "\\.unwrap\\(\\)"
message = "Do not unwrap."
"#,
        )
        .unwrap();
        let diff = "@@ -1 +1 @@\n+value.unwrap()\n";
        let file = parse_unified_diff("README.md", "README.md", diff).unwrap();

        assert!(rules.evaluate(&file).is_empty());
    }

    #[test]
    fn hash_changes_when_rule_text_changes() {
        let first = Ruleset::from_toml(
            r#"
[[rules]]
id = "a"
title = "A"
severity = "info"
path = "**/*"
pattern = "a"
message = "a"
"#,
        )
        .unwrap();
        let second = Ruleset::from_toml(
            r#"
[[rules]]
id = "a"
title = "A"
severity = "info"
path = "**/*"
pattern = "b"
message = "a"
"#,
        )
        .unwrap();

        assert_ne!(first.hash(), second.hash());
    }

    #[test]
    fn disabled_script_tasks_are_manual_only() {
        let rules = Ruleset::from_toml(
            r#"
[[script_tasks]]
enabled = false
id = "manual-check"
title = "Manual check"
command = "python check.py"
when_changed = ["src/**"]
"#,
        )
        .unwrap();

        assert!(rules
            .script_tasks_for_changes(&["src/lib.rs".into()])
            .is_empty());
        assert_eq!(rules.script_tasks_by_ids(&["manual-check".into()]).len(), 1);
    }
}
