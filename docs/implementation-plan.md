# GitLab MR Review Service Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Build the first Rust version of GitLabWorkRunner as a GitLab Merge Request automated review service.

**Architecture:** The service receives GitLab MR webhooks, fetches MR diffs through GitLab API, parses added-line positions, applies `rules.toml`, publishes MR discussions, and records processed commits in SQLite. Pure review logic stays independent from HTTP, GitLab, and storage boundaries so it can be tested without external services.

**Tech Stack:** Rust 2021, Tokio, Axum, Reqwest, Serde, SQLx SQLite, Regex, Globset, Thiserror, Tracing, Axum-based local mock servers for API tests.

---

## File Structure

- `Cargo.toml`: crate metadata and dependencies.
- `config.example.toml`: example service configuration.
- `rules.example.toml`: example review rules.
- `src/main.rs`: binary entrypoint, config loading, tracing, server startup.
- `src/lib.rs`: module exports for tests and integration.
- `src/config.rs`: typed `config.toml` loading and environment token resolution.
- `src/error.rs`: shared application error type.
- `src/diff.rs`: unified diff parser and line-position model.
- `src/rules.rs`: `rules.toml` parser, ruleset hash, glob and regex matching.
- `src/comments.rs`: finding grouping and GitLab-safe markdown comment bodies.
- `src/gitlab.rs`: GitLab REST client and request/response DTOs.
- `src/storage.rs`: SQLite schema, processed-review deduplication, comment persistence.
- `src/webhook.rs`: GitLab MR webhook payload parsing and token validation.
- `src/review.rs`: review orchestration from event to published comments.
- `src/server.rs`: Axum routes for `/healthz`, `/readyz`, and `/webhooks/gitlab`.
- `tests/fixtures/gitlab_mr_event.json`: minimal GitLab MR webhook payload.
- `tests/fixtures/mr_changes.json`: minimal GitLab MR changes response.
- `tests/e2e_review.rs`: mocked end-to-end review flow.

## Task 1: Rust Project Skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/error.rs`
- Create: `config.example.toml`
- Create: `rules.example.toml`

- [x] **Step 1: Create crate metadata and dependencies**

Create `Cargo.toml`:

```toml
[package]
name = "gitlab-work-runner"
version = "0.1.0"
edition = "2021"
license = "MIT"

[dependencies]
axum = "0.7"
chrono = { version = "0.4", features = ["serde"] }
globset = "0.4"
regex = "1"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "sqlite", "chrono"] }
thiserror = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal"] }
toml = "0.8"
tower-http = { version = "0.6", features = ["trace"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[dev-dependencies]
tempfile = "3"

```

- [x] **Step 2: Create module exports**

Create `src/lib.rs`:

```rust
pub mod error;
```

- [x] **Step 3: Create shared error type**

Create `src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("invalid webhook payload: {0}")]
    Webhook(String),
    #[error("diff parse error: {0}")]
    Diff(String),
    #[error("rule error: {0}")]
    Rule(String),
    #[error("gitlab api error: {0}")]
    GitLab(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    TomlDe(#[from] toml::de::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

pub type AppResult<T> = Result<T, AppError>;
```

- [x] **Step 4: Create binary entrypoint stub**

Create `src/main.rs`:

```rust
fn main() {
    eprintln!("GitLabWorkRunner is not implemented yet. See docs/design.md.");
}
```

- [x] **Step 5: Add example configs**

Create `config.example.toml`:

```toml
[server]
bind = "0.0.0.0:8080"
webhook_secret = "change-me"

[gitlab]
base_url = "https://gitlab.example.com"
token_env = "GITLAB_TOKEN"

[storage]
database_url = "sqlite://gitlab-work-runner.db"

[rules]
file = "rules.toml"
```

Create `rules.example.toml`:

```toml
[[rules]]
id = "forbid-unwrap"
title = "Avoid unwrap"
severity = "warning"
path = "**/*.rs"
pattern = "\\.unwrap\\(\\)"
message = "Direct unwrap can panic at runtime. Prefer explicit error handling."
```

- [x] **Step 6: Verify skeleton compiles**

Run: `cargo test`

Expected: PASS.

- [x] **Step 7: Commit**

```bash
git add Cargo.toml src/lib.rs src/main.rs src/error.rs config.example.toml rules.example.toml
git commit -m "chore: scaffold rust service"
```

## Task 2: Configuration Loader

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs`
- Test: unit tests in `src/config.rs`

- [x] **Step 1: Write config loading tests**

Add to `src/config.rs`:

```rust
use crate::error::{AppError, AppResult};
use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub gitlab: GitLabConfig,
    pub storage: StorageConfig,
    pub rules: RulesConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    pub bind: String,
    pub webhook_secret: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GitLabConfig {
    pub base_url: String,
    pub token_env: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct StorageConfig {
    pub database_url: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RulesConfig {
    pub file: String,
}

impl AppConfig {
    pub fn from_path(path: impl AsRef<Path>) -> AppResult<Self> {
        let text = fs::read_to_string(path)?;
        let config = toml::from_str(&text)?;
        Ok(config)
    }

    pub fn gitlab_token(&self) -> AppResult<String> {
        std::env::var(&self.gitlab.token_env).map_err(|_| {
            AppError::Config(format!(
                "environment variable {} is not set",
                self.gitlab.token_env
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_config_from_toml() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
[server]
bind = "127.0.0.1:8080"
webhook_secret = "secret"

[gitlab]
base_url = "https://gitlab.example.com"
token_env = "GITLAB_TOKEN"

[storage]
database_url = "sqlite::memory:"

[rules]
file = "rules.toml"
"#
        )
        .unwrap();

        let config = AppConfig::from_path(file.path()).unwrap();

        assert_eq!(config.server.bind, "127.0.0.1:8080");
        assert_eq!(config.server.webhook_secret, "secret");
        assert_eq!(config.gitlab.base_url, "https://gitlab.example.com");
        assert_eq!(config.storage.database_url, "sqlite::memory:");
        assert_eq!(config.rules.file, "rules.toml");
    }

    #[test]
    fn returns_error_when_token_env_is_missing() {
        let config = AppConfig {
            server: ServerConfig {
                bind: "127.0.0.1:8080".into(),
                webhook_secret: "secret".into(),
            },
            gitlab: GitLabConfig {
                base_url: "https://gitlab.example.com".into(),
                token_env: "GITLAB_WORK_RUNNER_MISSING_TOKEN".into(),
            },
            storage: StorageConfig {
                database_url: "sqlite::memory:".into(),
            },
            rules: RulesConfig {
                file: "rules.toml".into(),
            },
        };

        let err = config.gitlab_token().unwrap_err().to_string();
        assert!(err.contains("GITLAB_WORK_RUNNER_MISSING_TOKEN"));
    }
}
```

Add this line to `src/lib.rs`:

```rust
pub mod config;
```

- [x] **Step 2: Run config tests**

Run: `cargo test config::tests -- --nocapture`

Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add src/config.rs
git commit -m "feat: load service configuration"
```

## Task 3: Unified Diff Parser

**Files:**
- Create: `src/diff.rs`
- Modify: `src/lib.rs`
- Test: unit tests in `src/diff.rs`

- [x] **Step 1: Implement diff parser with tests**

Create `src/diff.rs`:

```rust
use crate::error::{AppError, AppResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFile {
    pub old_path: String,
    pub new_path: String,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

pub fn parse_unified_diff(old_path: &str, new_path: &str, diff: &str) -> AppResult<DiffFile> {
    let mut hunks = Vec::new();
    let mut current: Option<DiffHunk> = None;
    let mut old_line = 0_u32;
    let mut new_line = 0_u32;

    for raw in diff.lines() {
        if raw.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            let (old_start, old_lines, new_start, new_lines) = parse_hunk_header(raw)?;
            old_line = old_start;
            new_line = new_start;
            current = Some(DiffHunk {
                old_start,
                old_lines,
                new_start,
                new_lines,
                lines: Vec::new(),
            });
            continue;
        }

        let Some(hunk) = current.as_mut() else {
            continue;
        };

        if let Some(content) = raw.strip_prefix('+') {
            hunk.lines.push(DiffLine {
                kind: DiffLineKind::Added,
                old_line: None,
                new_line: Some(new_line),
                content: content.to_string(),
            });
            new_line += 1;
        } else if let Some(content) = raw.strip_prefix('-') {
            hunk.lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                old_line: Some(old_line),
                new_line: None,
                content: content.to_string(),
            });
            old_line += 1;
        } else if let Some(content) = raw.strip_prefix(' ') {
            hunk.lines.push(DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(old_line),
                new_line: Some(new_line),
                content: content.to_string(),
            });
            old_line += 1;
            new_line += 1;
        } else if raw == r"\ No newline at end of file" {
            continue;
        }
    }

    if let Some(hunk) = current.take() {
        hunks.push(hunk);
    }

    Ok(DiffFile {
        old_path: old_path.to_string(),
        new_path: new_path.to_string(),
        hunks,
    })
}

fn parse_hunk_header(header: &str) -> AppResult<(u32, u32, u32, u32)> {
    let end = header
        .find(" @@")
        .ok_or_else(|| AppError::Diff(format!("invalid hunk header: {header}")))?;
    let body = &header[3..end];
    let mut parts = body.split_whitespace();
    let old = parts
        .next()
        .ok_or_else(|| AppError::Diff(format!("missing old range: {header}")))?;
    let new = parts
        .next()
        .ok_or_else(|| AppError::Diff(format!("missing new range: {header}")))?;
    let (old_start, old_lines) = parse_range(old, '-')?;
    let (new_start, new_lines) = parse_range(new, '+')?;
    Ok((old_start, old_lines, new_start, new_lines))
}

fn parse_range(input: &str, prefix: char) -> AppResult<(u32, u32)> {
    let range = input
        .strip_prefix(prefix)
        .ok_or_else(|| AppError::Diff(format!("invalid range prefix: {input}")))?;
    let mut parts = range.split(',');
    let start = parts
        .next()
        .ok_or_else(|| AppError::Diff(format!("missing range start: {input}")))?
        .parse::<u32>()
        .map_err(|_| AppError::Diff(format!("invalid range start: {input}")))?;
    let len = parts
        .next()
        .unwrap_or("1")
        .parse::<u32>()
        .map_err(|_| AppError::Diff(format!("invalid range length: {input}")))?;
    Ok((start, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_added_removed_and_context_lines() {
        let diff = r#"
@@ -10,3 +10,4 @@ fn main() {
 let a = 1;
-let b = old();
+let b = new();
+let c = extra();
 }
"#;

        let file = parse_unified_diff("src/main.rs", "src/main.rs", diff).unwrap();

        assert_eq!(file.hunks.len(), 1);
        assert_eq!(file.hunks[0].old_start, 10);
        assert_eq!(file.hunks[0].new_start, 10);
        assert_eq!(file.hunks[0].lines[1].kind, DiffLineKind::Removed);
        assert_eq!(file.hunks[0].lines[1].old_line, Some(11));
        assert_eq!(file.hunks[0].lines[1].new_line, None);
        assert_eq!(file.hunks[0].lines[2].kind, DiffLineKind::Added);
        assert_eq!(file.hunks[0].lines[2].old_line, None);
        assert_eq!(file.hunks[0].lines[2].new_line, Some(11));
        assert_eq!(file.hunks[0].lines[3].new_line, Some(12));
    }

    #[test]
    fn parses_single_line_hunk_ranges() {
        let diff = "@@ -1 +1 @@\n-old\n+new\n";
        let file = parse_unified_diff("a.txt", "a.txt", diff).unwrap();
        assert_eq!(file.hunks[0].old_lines, 1);
        assert_eq!(file.hunks[0].new_lines, 1);
    }
}
```

Add this line to `src/lib.rs`:

```rust
pub mod diff;
```

- [x] **Step 2: Run diff tests**

Run: `cargo test diff::tests -- --nocapture`

Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add src/diff.rs
git commit -m "feat: parse unified diffs"
```

## Task 4: Rule Engine

**Files:**
- Create: `src/rules.rs`
- Modify: `src/lib.rs`
- Test: unit tests in `src/rules.rs`

- [x] **Step 1: Implement rules parsing and matching**

Create `src/rules.rs`:

```rust
use crate::{
    diff::{DiffFile, DiffLineKind},
    error::{AppError, AppResult},
};
use globset::{Glob, GlobMatcher};
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Clone, Debug, Deserialize)]
pub struct RulesFile {
    pub rules: Vec<RuleConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RuleConfig {
    pub id: String,
    pub title: String,
    pub severity: Severity,
    pub path: String,
    pub pattern: String,
    pub message: String,
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

pub struct Ruleset {
    hash: String,
    rules: Vec<CompiledRule>,
}

impl Ruleset {
    pub fn from_path(path: impl AsRef<Path>) -> AppResult<Self> {
        let text = fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    pub fn from_toml(text: &str) -> AppResult<Self> {
        let parsed: RulesFile = toml::from_str(text)?;
        let mut rules = Vec::new();
        for config in parsed.rules {
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
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        Ok(Self { hash, rules })
    }

    pub fn hash(&self) -> &str {
        &self.hash
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
}
```

Add this line to `src/lib.rs`:

```rust
pub mod rules;
```

- [x] **Step 2: Run rule tests**

Run: `cargo test rules::tests -- --nocapture`

Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add src/rules.rs
git commit -m "feat: evaluate configured review rules"
```

## Task 5: Comment Builder

**Files:**
- Create: `src/comments.rs`
- Modify: `src/lib.rs`
- Test: unit tests in `src/comments.rs`

- [x] **Step 1: Implement markdown comment builder**

Create `src/comments.rs`:

```rust
use crate::rules::{Finding, Severity};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentDraft {
    pub path: String,
    pub new_line: Option<u32>,
    pub body: String,
}

pub fn build_comment_drafts(findings: &[Finding]) -> Vec<CommentDraft> {
    let mut grouped: BTreeMap<(String, Option<u32>), Vec<&Finding>> = BTreeMap::new();
    for finding in findings {
        grouped
            .entry((finding.path.clone(), finding.new_line))
            .or_default()
            .push(finding);
    }

    grouped
        .into_iter()
        .map(|((path, new_line), group)| CommentDraft {
            path,
            new_line,
            body: build_body(&group),
        })
        .collect()
}

fn build_body(findings: &[&Finding]) -> String {
    let mut body = String::new();
    for (index, finding) in findings.iter().enumerate() {
        if index > 0 {
            body.push_str("\n\n---\n\n");
        }
        body.push_str(&format!(
            "**[{}] {}**\n\n{}\n\n<!-- gitlab-work-runner:rule={} -->",
            severity_label(&finding.severity),
            finding.title,
            finding.message,
            finding.rule_id
        ));
    }
    body
}

fn severity_label(severity: &Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(rule_id: &str, line: Option<u32>) -> Finding {
        Finding {
            rule_id: rule_id.into(),
            severity: Severity::Warning,
            path: "src/lib.rs".into(),
            new_line: line,
            title: "Avoid unwrap".into(),
            message: "Do not unwrap.".into(),
        }
    }

    #[test]
    fn creates_stable_marker() {
        let drafts = build_comment_drafts(&[finding("forbid-unwrap", Some(12))]);

        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].new_line, Some(12));
        assert!(drafts[0].body.contains("**[warning] Avoid unwrap**"));
        assert!(drafts[0]
            .body
            .contains("<!-- gitlab-work-runner:rule=forbid-unwrap -->"));
    }

    #[test]
    fn groups_findings_on_same_line() {
        let drafts = build_comment_drafts(&[
            finding("forbid-unwrap", Some(12)),
            finding("other-rule", Some(12)),
        ]);

        assert_eq!(drafts.len(), 1);
        assert!(drafts[0].body.contains("forbid-unwrap"));
        assert!(drafts[0].body.contains("other-rule"));
    }
}
```

Add this line to `src/lib.rs`:

```rust
pub mod comments;
```

- [x] **Step 2: Run comment tests**

Run: `cargo test comments::tests -- --nocapture`

Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add src/comments.rs
git commit -m "feat: build review comments"
```

## Task 6: SQLite State Store

**Files:**
- Create: `src/storage.rs`
- Modify: `src/lib.rs`
- Test: unit tests in `src/storage.rs`

- [x] **Step 1: Implement state store**

Create `src/storage.rs`:

```rust
use crate::error::AppResult;
use chrono::Utc;
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

#[derive(Clone)]
pub struct StateStore {
    pool: SqlitePool,
}

#[derive(Clone, Debug)]
pub struct ReviewKey<'a> {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: &'a str,
    pub ruleset_hash: &'a str,
}

#[derive(Clone, Debug)]
pub struct StoredComment<'a> {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: &'a str,
    pub ruleset_hash: &'a str,
    pub rule_id: &'a str,
    pub path: &'a str,
    pub new_line: Option<i64>,
    pub discussion_id: Option<&'a str>,
    pub note_id: Option<i64>,
}

impl StateStore {
    pub async fn connect(database_url: &str) -> AppResult<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> AppResult<()> {
        sqlx::query(
            r#"
create table if not exists processed_reviews (
    id integer primary key autoincrement,
    project_id integer not null,
    mr_iid integer not null,
    commit_sha text not null,
    ruleset_hash text not null,
    status text not null,
    created_at text not null,
    updated_at text not null,
    unique(project_id, mr_iid, commit_sha, ruleset_hash)
);
"#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
create table if not exists review_comments (
    id integer primary key autoincrement,
    project_id integer not null,
    mr_iid integer not null,
    commit_sha text not null,
    ruleset_hash text not null,
    rule_id text not null,
    path text not null,
    new_line integer,
    discussion_id text,
    note_id integer,
    created_at text not null
);
"#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn has_processed(&self, key: &ReviewKey<'_>) -> AppResult<bool> {
        let count: i64 = sqlx::query_scalar(
            r#"
select count(*) from processed_reviews
where project_id = ? and mr_iid = ? and commit_sha = ? and ruleset_hash = ?
"#,
        )
        .bind(key.project_id)
        .bind(key.mr_iid)
        .bind(key.commit_sha)
        .bind(key.ruleset_hash)
        .fetch_one(&self.pool)
        .await?;
        Ok(count > 0)
    }

    pub async fn mark_processed(&self, key: &ReviewKey<'_>, status: &str) -> AppResult<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
insert into processed_reviews
(project_id, mr_iid, commit_sha, ruleset_hash, status, created_at, updated_at)
values (?, ?, ?, ?, ?, ?, ?)
on conflict(project_id, mr_iid, commit_sha, ruleset_hash)
do update set status = excluded.status, updated_at = excluded.updated_at
"#,
        )
        .bind(key.project_id)
        .bind(key.mr_iid)
        .bind(key.commit_sha)
        .bind(key.ruleset_hash)
        .bind(status)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_comment(&self, comment: &StoredComment<'_>) -> AppResult<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
insert into review_comments
(project_id, mr_iid, commit_sha, ruleset_hash, rule_id, path, new_line, discussion_id, note_id, created_at)
values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
"#,
        )
        .bind(comment.project_id)
        .bind(comment.mr_iid)
        .bind(comment.commit_sha)
        .bind(comment.ruleset_hash)
        .bind(comment.rule_id)
        .bind(comment.path)
        .bind(comment.new_line)
        .bind(comment.discussion_id)
        .bind(comment.note_id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tracks_processed_review_keys() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        let key = ReviewKey {
            project_id: 1,
            mr_iid: 2,
            commit_sha: "abc",
            ruleset_hash: "hash",
        };

        assert!(!store.has_processed(&key).await.unwrap());
        store.mark_processed(&key, "success").await.unwrap();
        assert!(store.has_processed(&key).await.unwrap());
    }
}
```

Add this line to `src/lib.rs`:

```rust
pub mod storage;
```

- [x] **Step 2: Run storage tests**

Run: `cargo test storage::tests -- --nocapture`

Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add src/storage.rs
git commit -m "feat: persist review state"
```

## Task 7: GitLab API Client

**Files:**
- Create: `src/gitlab.rs`
- Modify: `src/lib.rs`
- Test: unit tests in `src/gitlab.rs`

- [x] **Step 1: Implement GitLab client DTOs and methods**

Create `src/gitlab.rs`:

```rust
use crate::error::AppResult;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct GitLabClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MergeRequestChanges {
    pub changes: Vec<GitLabChange>,
    pub diff_refs: DiffRefs,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GitLabChange {
    pub old_path: String,
    pub new_path: String,
    pub new_file: bool,
    pub renamed_file: bool,
    pub deleted_file: bool,
    pub diff: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiffRefs {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CreateDiscussionRequest {
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<DiscussionPosition>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiscussionPosition {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
    pub position_type: String,
    pub old_path: String,
    pub new_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_line: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreatedDiscussion {
    pub id: String,
    #[serde(default)]
    pub notes: Vec<CreatedNote>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreatedNote {
    pub id: i64,
}

impl GitLabClient {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
        }
    }

    pub async fn merge_request_changes(
        &self,
        project_id: i64,
        mr_iid: i64,
    ) -> AppResult<MergeRequestChanges> {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/changes",
            self.base_url, project_id, mr_iid
        );
        let response = self
            .http
            .get(url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }

    pub async fn create_discussion(
        &self,
        project_id: i64,
        mr_iid: i64,
        request: &CreateDiscussionRequest,
    ) -> AppResult<CreatedDiscussion> {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/discussions",
            self.base_url, project_id, mr_iid
        );
        let response = self
            .http
            .post(url)
            .header("PRIVATE-TOKEN", &self.token)
            .json(request)
            .send()
            .await?;
        if response.status() == StatusCode::BAD_REQUEST && request.position.is_some() {
            let fallback = CreateDiscussionRequest {
                body: request.body.clone(),
                position: None,
            };
            return self
                .http
                .post(format!(
                    "{}/api/v4/projects/{}/merge_requests/{}/discussions",
                    self.base_url, project_id, mr_iid
                ))
                .header("PRIVATE-TOKEN", &self.token)
                .json(&fallback)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await
                .map_err(Into::into);
        }
        Ok(response.error_for_status()?.json().await?)
    }
}
```

Add this line to `src/lib.rs`:

```rust
pub mod gitlab;
```

- [x] **Step 2: Add API client tests with wiremock**

Add tests in `src/gitlab.rs` that verify:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn fetches_merge_request_changes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/1/merge_requests/2/changes"))
            .and(header("PRIVATE-TOKEN", "token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "changes": [{
                    "old_path": "src/lib.rs",
                    "new_path": "src/lib.rs",
                    "new_file": false,
                    "renamed_file": false,
                    "deleted_file": false,
                    "diff": "@@ -1 +1 @@\n+new\n"
                }],
                "diff_refs": {
                    "base_sha": "base",
                    "start_sha": "start",
                    "head_sha": "head"
                }
            })))
            .mount(&server)
            .await;

        let client = GitLabClient::new(server.uri(), "token".into());
        let changes = client.merge_request_changes(1, 2).await.unwrap();

        assert_eq!(changes.changes.len(), 1);
        assert_eq!(changes.diff_refs.head_sha, "head");
    }
}
```

- [x] **Step 3: Run GitLab tests**

Run: `cargo test gitlab::tests -- --nocapture`

Expected: PASS.

- [x] **Step 4: Commit**

```bash
git add src/gitlab.rs
git commit -m "feat: add gitlab api client"
```

## Task 8: Webhook Parsing

**Files:**
- Create: `src/webhook.rs`
- Create: `tests/fixtures/gitlab_mr_event.json`
- Modify: `src/lib.rs`
- Test: unit tests in `src/webhook.rs`

- [x] **Step 1: Add webhook fixture**

Create `tests/fixtures/gitlab_mr_event.json`:

```json
{
  "object_kind": "merge_request",
  "project": {
    "id": 123
  },
  "object_attributes": {
    "iid": 45,
    "action": "update",
    "last_commit": {
      "id": "abc123"
    },
    "source_branch": "feature/review",
    "target_branch": "main"
  }
}
```

- [x] **Step 2: Implement webhook parser**

Create `src/webhook.rs`:

```rust
use crate::error::{AppError, AppResult};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeRequestEvent {
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: String,
    pub action: String,
    pub source_branch: String,
    pub target_branch: String,
}

#[derive(Debug, Deserialize)]
struct GitLabWebhookPayload {
    object_kind: String,
    project: ProjectPayload,
    object_attributes: MergeRequestAttributes,
}

#[derive(Debug, Deserialize)]
struct ProjectPayload {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct MergeRequestAttributes {
    iid: i64,
    action: String,
    last_commit: LastCommit,
    source_branch: String,
    target_branch: String,
}

#[derive(Debug, Deserialize)]
struct LastCommit {
    id: String,
}

pub fn validate_token(expected: &str, actual: Option<&str>) -> AppResult<()> {
    match actual {
        Some(value) if value == expected => Ok(()),
        _ => Err(AppError::Webhook("invalid X-Gitlab-Token".into())),
    }
}

pub fn parse_merge_request_event(body: &[u8]) -> AppResult<Option<MergeRequestEvent>> {
    let payload: GitLabWebhookPayload = serde_json::from_slice(body)?;
    if payload.object_kind != "merge_request" {
        return Ok(None);
    }
    Ok(Some(MergeRequestEvent {
        project_id: payload.project.id,
        mr_iid: payload.object_attributes.iid,
        commit_sha: payload.object_attributes.last_commit.id,
        action: payload.object_attributes.action,
        source_branch: payload.object_attributes.source_branch,
        target_branch: payload.object_attributes.target_branch,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_secret_token() {
        assert!(validate_token("secret", Some("secret")).is_ok());
        assert!(validate_token("secret", Some("wrong")).is_err());
        assert!(validate_token("secret", None).is_err());
    }

    #[test]
    fn parses_merge_request_event() {
        let body = include_bytes!("../tests/fixtures/gitlab_mr_event.json");
        let event = parse_merge_request_event(body).unwrap().unwrap();

        assert_eq!(event.project_id, 123);
        assert_eq!(event.mr_iid, 45);
        assert_eq!(event.commit_sha, "abc123");
        assert_eq!(event.source_branch, "feature/review");
        assert_eq!(event.target_branch, "main");
    }
}
```

Add this line to `src/lib.rs`:

```rust
pub mod webhook;
```

- [x] **Step 3: Run webhook tests**

Run: `cargo test webhook::tests -- --nocapture`

Expected: PASS.

- [x] **Step 4: Commit**

```bash
git add src/webhook.rs tests/fixtures/gitlab_mr_event.json
git commit -m "feat: parse gitlab webhooks"
```

## Task 9: Review Orchestration

**Files:**
- Create: `src/review.rs`
- Modify: `src/gitlab.rs`
- Modify: `src/lib.rs`
- Test: unit tests in `src/review.rs`

- [x] **Step 1: Implement review service**

Create `src/review.rs`:

```rust
use crate::{
    comments::build_comment_drafts,
    diff::parse_unified_diff,
    error::AppResult,
    gitlab::{CreateDiscussionRequest, DiscussionPosition, GitLabClient},
    rules::Ruleset,
    storage::{ReviewKey, StateStore, StoredComment},
    webhook::MergeRequestEvent,
};

pub struct ReviewService {
    gitlab: GitLabClient,
    store: StateStore,
    ruleset: Ruleset,
}

impl ReviewService {
    pub fn new(gitlab: GitLabClient, store: StateStore, ruleset: Ruleset) -> Self {
        Self {
            gitlab,
            store,
            ruleset,
        }
    }

    pub async fn review_merge_request(&self, event: &MergeRequestEvent) -> AppResult<ReviewSummary> {
        let key = ReviewKey {
            project_id: event.project_id,
            mr_iid: event.mr_iid,
            commit_sha: &event.commit_sha,
            ruleset_hash: self.ruleset.hash(),
        };
        if self.store.has_processed(&key).await? {
            return Ok(ReviewSummary {
                skipped: true,
                findings: 0,
                comments: 0,
            });
        }

        let changes = self
            .gitlab
            .merge_request_changes(event.project_id, event.mr_iid)
            .await?;
        let mut findings = Vec::new();
        for change in &changes.changes {
            if change.deleted_file || change.diff.trim().is_empty() {
                continue;
            }
            let diff_file = parse_unified_diff(&change.old_path, &change.new_path, &change.diff)?;
            findings.extend(self.ruleset.evaluate(&diff_file));
        }

        let drafts = build_comment_drafts(&findings);
        let mut published = 0_usize;
        for draft in &drafts {
            let position = draft.new_line.map(|new_line| DiscussionPosition {
                base_sha: changes.diff_refs.base_sha.clone(),
                start_sha: changes.diff_refs.start_sha.clone(),
                head_sha: changes.diff_refs.head_sha.clone(),
                position_type: "text".into(),
                old_path: draft.path.clone(),
                new_path: draft.path.clone(),
                new_line: Some(new_line),
            });
            let created = self
                .gitlab
                .create_discussion(
                    event.project_id,
                    event.mr_iid,
                    &CreateDiscussionRequest {
                        body: draft.body.clone(),
                        position,
                    },
                )
                .await?;
            self.store
                .record_comment(&StoredComment {
                    project_id: event.project_id,
                    mr_iid: event.mr_iid,
                    commit_sha: &event.commit_sha,
                    ruleset_hash: self.ruleset.hash(),
                    rule_id: "grouped",
                    path: &draft.path,
                    new_line: draft.new_line.map(i64::from),
                    discussion_id: Some(&created.id),
                    note_id: created.notes.first().map(|note| note.id),
                })
                .await?;
            published += 1;
        }

        self.store.mark_processed(&key, "success").await?;
        Ok(ReviewSummary {
            skipped: false,
            findings: findings.len(),
            comments: published,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSummary {
    pub skipped: bool,
    pub findings: usize,
    pub comments: usize,
}
```

Add this line to `src/lib.rs`:

```rust
pub mod review;
```

- [x] **Step 2: Run review compile check**

Run: `cargo test review -- --nocapture`

Expected: PASS or no tests run after compiling `src/review.rs`.

- [x] **Step 3: Commit**

```bash
git add src/review.rs
git commit -m "feat: orchestrate merge request reviews"
```

## Task 10: HTTP Server

**Files:**
- Create: `src/server.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs`
- Test: unit tests in `src/server.rs`

- [x] **Step 1: Implement Axum routes**

Create `src/server.rs`:

```rust
use crate::{
    config::AppConfig,
    error::AppResult,
    gitlab::GitLabClient,
    review::ReviewService,
    rules::Ruleset,
    storage::StateStore,
    webhook::{parse_merge_request_event, validate_token},
};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
use tower_http::trace::TraceLayer;

#[derive(Clone)]
struct AppState {
    config: AppConfig,
    store: StateStore,
}

pub async fn serve(config: AppConfig, store: StateStore) -> AppResult<()> {
    let addr: SocketAddr = config
        .server
        .bind
        .parse()
        .map_err(|err| crate::error::AppError::Config(format!("invalid bind address: {err}")))?;
    let app = router(config, store);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on {}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

fn router(config: AppConfig, store: StateStore) -> Router {
    let state = Arc::new(AppState { config, store });
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/webhooks/gitlab", post(gitlab_webhook))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn readyz() -> impl IntoResponse {
    Json(json!({ "status": "ready" }))
}

async fn gitlab_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let token = headers
        .get("X-Gitlab-Token")
        .and_then(|value| value.to_str().ok());
    if let Err(err) = validate_token(&state.config.server.webhook_secret, token) {
        return (StatusCode::UNAUTHORIZED, err.to_string()).into_response();
    }

    let event = match parse_merge_request_event(&body) {
        Ok(Some(event)) => event,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    };

    let gitlab_token = match state.config.gitlab_token() {
        Ok(token) => token,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let ruleset = match Ruleset::from_path(&state.config.rules.file) {
        Ok(ruleset) => ruleset,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let gitlab = GitLabClient::new(state.config.gitlab.base_url.clone(), gitlab_token);
    let service = ReviewService::new(gitlab, state.store.clone(), ruleset);

    match service.review_merge_request(&event).await {
        Ok(summary) => (StatusCode::ACCEPTED, Json(json!({
            "skipped": summary.skipped,
            "findings": summary.findings,
            "comments": summary.comments
        }))).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}
```

Replace `src/main.rs` with:

```rust
use gitlab_work_runner::{config::AppConfig, server, storage::StateStore};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> gitlab_work_runner::error::AppResult<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = AppConfig::from_path("config.toml")?;
    let store = StateStore::connect(&config.storage.database_url).await?;
    store.migrate().await?;
    server::serve(config, store).await
}
```

Add this line to `src/lib.rs`:

```rust
pub mod server;
```

- [x] **Step 2: Run server compile check**

Run: `cargo test server -- --nocapture`

Expected: PASS or no tests run after compiling `src/server.rs`.

- [x] **Step 3: Commit**

```bash
git add src/server.rs src/main.rs
git commit -m "feat: expose gitlab webhook server"
```

## Task 11: End-to-End Mocked Flow

**Files:**
- Create: `tests/fixtures/mr_changes.json`
- Create: `tests/e2e_review.rs`

- [x] **Step 1: Add MR changes fixture**

Create `tests/fixtures/mr_changes.json`:

```json
{
  "changes": [
    {
      "old_path": "src/lib.rs",
      "new_path": "src/lib.rs",
      "new_file": false,
      "renamed_file": false,
      "deleted_file": false,
      "diff": "@@ -1 +1 @@\n+let value = maybe.unwrap();\n"
    }
  ],
  "diff_refs": {
    "base_sha": "base",
    "start_sha": "start",
    "head_sha": "abc123"
  }
}
```

- [x] **Step 2: Add e2e test**

Create `tests/e2e_review.rs`:

```rust
use gitlab_work_runner::{
    gitlab::GitLabClient,
    review::ReviewService,
    rules::Ruleset,
    storage::StateStore,
    webhook::MergeRequestEvent,
};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn reviews_merge_request_and_records_state() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/123/merge_requests/45/changes"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            include_str!("fixtures/mr_changes.json"),
            "application/json",
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v4/projects/123/merge_requests/45/discussions"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "discussion-1",
            "notes": [{ "id": 99 }]
        })))
        .mount(&server)
        .await;

    let store = StateStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let ruleset = Ruleset::from_toml(
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
    let service = ReviewService::new(
        GitLabClient::new(server.uri(), "token".into()),
        store,
        ruleset,
    );
    let event = MergeRequestEvent {
        project_id: 123,
        mr_iid: 45,
        commit_sha: "abc123".into(),
        action: "update".into(),
        source_branch: "feature/review".into(),
        target_branch: "main".into(),
    };

    let summary = service.review_merge_request(&event).await.unwrap();

    assert_eq!(summary.findings, 1);
    assert_eq!(summary.comments, 1);
    assert!(!summary.skipped);
}
```

- [x] **Step 3: Run e2e test**

Run: `cargo test --test e2e_review -- --nocapture`

Expected: PASS.

- [x] **Step 4: Commit**

```bash
git add tests/fixtures/mr_changes.json tests/e2e_review.rs
git commit -m "test: cover mocked review flow"
```

## Task 12: Documentation Update and Final Verification

**Files:**
- Modify: `README.md`
- Modify: `.gitignore`

- [x] **Step 1: Add runtime ignores**

Create `.gitignore`:

```gitignore
/target/
/gitlab-work-runner.db
/config.toml
/rules.toml
```

- [x] **Step 2: Update README run instructions**

Add this section to `README.md`:

````markdown
## Local Run

```powershell
Copy-Item config.example.toml config.toml
Copy-Item rules.example.toml rules.toml
$env:GITLAB_TOKEN = "<your-token>"
cargo run
```

Configure a GitLab project webhook:

- URL: `http://<host>:8080/webhooks/gitlab`
- Secret token: the value of `[server].webhook_secret`
- Trigger: Merge request events
````

- [x] **Step 3: Run full verification**

Run: `cargo fmt`

Expected: command exits successfully.

Run: `cargo test`

Expected: all tests PASS.

- [x] **Step 4: Commit**

```bash
git add .gitignore README.md
git commit -m "docs: add local run instructions"
```

## Execution Notes

- Keep the first implementation GitLab-only.
- Do not add a Web UI in this plan.
- Keep `/healthz` and `/readyz` only.
- Do not add LLM review or plugin execution in the first version.
- If GitLab position publishing fails with `400 Bad Request`, fall back to MR-level discussion so the service still reports findings.
