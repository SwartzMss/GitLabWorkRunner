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
