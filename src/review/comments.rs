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
        Severity::Info => "信息",
        Severity::Warning => "警告",
        Severity::Error => "错误",
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
        assert!(drafts[0].body.contains("**[警告] Avoid unwrap**"));
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
