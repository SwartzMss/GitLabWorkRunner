use crate::rules::AiReviewConfig;

use super::ai_schema::{OpenAiTool, OpenAiToolCall, OpenAiToolFunction};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};
use tracing::info;

const SEARCH_MAX_MATCHES: usize = 50;
const SEARCH_MAX_MATCHES_PER_FILE: usize = 5;
const SEARCH_MAX_FILE_BYTES: u64 = 1024 * 1024;
const READ_MAX_FILE_BYTES: usize = 1024 * 1024;
const READ_MAX_LINES: usize = 250;
const LIST_MAX_FILES: usize = 200;

pub(crate) fn enabled_context_tools(_config: &AiReviewConfig) -> Vec<OpenAiTool> {
    vec![read_file_tool(), search_code_tool(), list_files_tool()]
}

pub(crate) fn read_file_tool() -> OpenAiTool {
    OpenAiTool {
        tool_type: "function",
        function: OpenAiToolFunction {
            name: "read_file",
            description: "Read UTF-8 text from one file in the merge request head checkout. Prefer a narrow start_line/end_line range around a search_code match; omit both only when the whole file is genuinely required. The path must be a repository-relative file path such as \"src/lib.rs\". Absolute paths, parent-directory traversal, .env, and .git are rejected. Returns JSON containing ok, path, content, truncated, and range metadata when requested; on failure returns {\"ok\":false,\"error\":\"...\"}. Content is limited by max_tool_result_bytes and an internal 1 MiB cap, and may be returned with truncated=true.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Repository-relative path of the UTF-8 text file to read, for example \"src/lib.rs\". Do not use absolute paths or \"..\"."
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional one-based first line to return. Supply together with end_line and prefer a narrow range around a search_code match."
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional one-based inclusive last line to return. Supply together with start_line; one read may span at most 250 lines."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
    }
}

pub(crate) fn search_code_tool() -> OpenAiTool {
    OpenAiTool {
        tool_type: "function",
        function: OpenAiToolFunction {
            name: "search_code",
            description: "Search UTF-8 text files in the merge request head checkout using a plain substring query, not a regular expression. Use this to find definitions, references, config keys, or related call sites before deciding whether a diff is buggy. Optional glob limits searched files, for example \"src/**/*.rs\". Sensitive files, dependency/build directories, lock files, and files larger than 1 MiB are skipped. Returns JSON: {\"ok\":true,\"matches\":[{\"path\":\"src/config.rs\",\"line\":42,\"before\":\"...\",\"text\":\"...\",\"after\":\"...\"}],\"truncated\":false}; on failure returns {\"ok\":false,\"error\":\"...\"}. At most 50 total matches and 5 matches per file are returned; result may be truncated by max_tool_result_bytes.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Plain substring to search for. Use exact identifiers, function names, type names, config keys, or distinctive text."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Optional glob that restricts searched files, for example \"src/**/*.rs\" or \"**/*.toml\"."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
    }
}

pub(crate) fn list_files_tool() -> OpenAiTool {
    OpenAiTool {
        tool_type: "function",
        function: OpenAiToolFunction {
            name: "list_files",
            description: "List repository files from the merge request head checkout. Use this when you need to discover likely file paths before calling read_file or search_code. Optional glob limits results, for example \"src/**/*.rs\". Sensitive files, dependency/build directories, and lock files are skipped. Returns JSON: {\"ok\":true,\"files\":[\"src/lib.rs\"],\"truncated\":false}; on failure returns {\"ok\":false,\"error\":\"...\"}. At most 200 files are returned and the result may be truncated by max_tool_result_bytes.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "glob": {
                        "type": "string",
                        "description": "Optional glob that restricts listed files, for example \"src/**/*.rs\" or \"**/*.toml\"."
                    }
                },
                "additionalProperties": false
            }),
        },
    }
}

pub(crate) fn review_findings_tool() -> OpenAiTool {
    OpenAiTool {
        tool_type: "function",
        function: OpenAiToolFunction {
            name: "submit_review_findings",
            description:
                "Submit high-confidence code review findings for the GitLab merge request diff.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "findings": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "line": { "type": "integer" },
                                "severity": {
                                    "type": "string",
                                    "enum": ["error"]
                                },
                                "title": { "type": "string" },
                                "message": { "type": "string" }
                            },
                            "required": ["path", "line", "severity", "title", "message"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["findings"],
                "additionalProperties": false
            }),
        },
    }
}

pub(crate) fn is_context_tool_call(tool_call: &OpenAiToolCall) -> bool {
    matches!(
        tool_call.function.name.as_str(),
        "read_file" | "search_code" | "list_files"
    )
}

pub(crate) fn non_empty_tool_call_id(tool_call: &OpenAiToolCall) -> String {
    if tool_call.id.trim().is_empty() {
        format!("call_{}", tool_call.function.name)
    } else {
        tool_call.id.clone()
    }
}

pub(crate) fn context_tool_cache_key(tool_name: &str, arguments: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return format!("{tool_name}\0{arguments}");
    };
    if tool_name == "read_file" {
        if let Some(path) = value.get_mut("path") {
            if let Some(raw_path) = path.as_str() {
                *path = serde_json::Value::String(normalize_path(raw_path));
            }
        }
    }
    format!("{tool_name}\0{}", canonical_json(&value))
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(object) => {
            let sorted = object
                .iter()
                .map(|(key, value)| (key, canonical_json(value)))
                .collect::<BTreeMap<_, _>>();
            let fields = sorted
                .into_iter()
                .map(|(key, value)| format!("{}:{value}", serde_json::json!(key)))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{fields}}}")
        }
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    }
}

pub(crate) struct AiReviewToolContext {
    source_dir: Option<PathBuf>,
    read_file: bool,
    search_code: bool,
    list_files: bool,
    max_result_bytes: usize,
}

impl AiReviewToolContext {
    pub(crate) fn new(config: &AiReviewConfig, source_dir: Option<&Path>) -> Self {
        let source_dir = source_dir.map(Path::to_path_buf);
        let source_available = source_dir.is_some();
        info!(
            ai_review_id = %config.id,
            read_file = source_available,
            search_code = source_available,
            list_files = source_available,
            max_tool_calls = config.max_tool_calls,
            max_tool_result_bytes = config.max_tool_result_bytes,
            source_dir = %source_dir.as_ref().map(|path| path.display().to_string()).unwrap_or_default(),
            "AI review context tools configured"
        );
        Self {
            source_dir,
            read_file: source_available,
            search_code: source_available,
            list_files: source_available,
            max_result_bytes: config.max_tool_result_bytes.max(1),
        }
    }

    pub(crate) fn call_with_result_limit(
        &self,
        tool_call: &OpenAiToolCall,
        max_result_bytes: usize,
    ) -> String {
        let result = match tool_call.function.name.as_str() {
            "read_file" if self.read_file => self.read_file(&tool_call.function.arguments),
            "search_code" if self.search_code => self.search_code(&tool_call.function.arguments),
            "list_files" if self.list_files => self.list_files(&tool_call.function.arguments),
            name => serde_json::json!({
                "ok": false,
                "error": format!("tool is not enabled: {name}")
            }),
        };
        truncate_json_result(result, self.max_result_bytes.min(max_result_bytes.max(1)))
    }

    pub(crate) fn enabled(&self) -> bool {
        self.read_file || self.search_code || self.list_files
    }

    pub(crate) fn source_available(&self) -> bool {
        self.source_dir.is_some()
    }

    pub(crate) fn enabled_tool_names(&self) -> String {
        let mut names = Vec::new();
        if self.read_file {
            names.push("read_file");
        }
        if self.search_code {
            names.push("search_code");
        }
        if self.list_files {
            names.push("list_files");
        }
        names.join(",")
    }

    pub(crate) fn read_file(&self, arguments: &str) -> serde_json::Value {
        #[derive(serde::Deserialize)]
        struct Args {
            path: String,
            #[serde(default)]
            start_line: Option<usize>,
            #[serde(default)]
            end_line: Option<usize>,
        }
        let args: Args = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(err) => return tool_error(format!("invalid read_file arguments: {err}")),
        };
        let path = match self.resolve_safe_path(&args.path) {
            Ok(path) => path,
            Err(err) => return tool_error(err),
        };
        match (args.start_line, args.end_line) {
            (None, None) => {
                match read_utf8_file_limited(&path, self.max_result_bytes.min(READ_MAX_FILE_BYTES))
                {
                    Ok((content, truncated)) => serde_json::json!({
                        "ok": true,
                        "path": normalize_path(&args.path),
                        "content": content,
                        "truncated": truncated
                    }),
                    Err(err) => tool_error(format!("failed to read file: {err}")),
                }
            }
            (Some(start_line), Some(end_line)) => {
                if start_line == 0 || end_line < start_line {
                    return tool_error(
                        "line range must be one-based and end_line must be at least start_line",
                    );
                }
                if end_line - start_line + 1 > READ_MAX_LINES {
                    return tool_error(format!(
                        "line range must not exceed {READ_MAX_LINES} lines"
                    ));
                }
                match read_utf8_line_range_limited(
                    &path,
                    start_line,
                    end_line,
                    self.max_result_bytes.min(READ_MAX_FILE_BYTES),
                ) {
                    Ok((content, actual_end_line, truncated)) => serde_json::json!({
                        "ok": true,
                        "path": normalize_path(&args.path),
                        "start_line": start_line,
                        "end_line": actual_end_line,
                        "content": content,
                        "truncated": truncated
                    }),
                    Err(err) => tool_error(format!("failed to read file: {err}")),
                }
            }
            _ => tool_error("start_line and end_line must be supplied together"),
        }
    }

    fn search_code(&self, arguments: &str) -> serde_json::Value {
        #[derive(serde::Deserialize)]
        struct Args {
            query: String,
            #[serde(default)]
            glob: Option<String>,
        }
        let args: Args = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(err) => return tool_error(format!("invalid search_code arguments: {err}")),
        };
        if args.query.trim().is_empty() {
            return tool_error("query must not be empty");
        }
        let Some(root) = &self.source_dir else {
            return tool_error("source checkout is not available");
        };
        let matcher = match optional_glob_matcher(args.glob.as_deref()) {
            Ok(matcher) => matcher,
            Err(err) => return tool_error(err),
        };
        let mut matches = Vec::new();
        collect_files(root, root, &mut |relative, path| {
            if matches.len() >= SEARCH_MAX_MATCHES {
                return;
            }
            if is_low_value_context_path(relative) || file_too_large(path, SEARCH_MAX_FILE_BYTES) {
                return;
            }
            if !matcher
                .as_ref()
                .is_none_or(|matcher| matcher.is_match(relative))
            {
                return;
            }
            let Ok(content) = fs::read_to_string(path) else {
                return;
            };
            let lines: Vec<_> = content.lines().collect();
            let mut file_matches = 0_usize;
            for (index, line) in lines.iter().enumerate() {
                if line.contains(&args.query) {
                    matches.push(serde_json::json!({
                        "path": relative,
                        "line": index + 1,
                        "before": index.checked_sub(1).and_then(|before| lines.get(before)).copied().unwrap_or(""),
                        "text": line
                            ,
                        "after": lines.get(index + 1).copied().unwrap_or("")
                    }));
                    file_matches += 1;
                    if matches.len() >= SEARCH_MAX_MATCHES
                        || file_matches >= SEARCH_MAX_MATCHES_PER_FILE
                    {
                        break;
                    }
                }
            }
        });
        serde_json::json!({
            "ok": true,
            "matches": matches,
            "truncated": matches.len() >= SEARCH_MAX_MATCHES
        })
    }

    fn list_files(&self, arguments: &str) -> serde_json::Value {
        #[derive(serde::Deserialize)]
        struct Args {
            #[serde(default)]
            glob: Option<String>,
        }
        let args: Args = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(err) => return tool_error(format!("invalid list_files arguments: {err}")),
        };
        let Some(root) = &self.source_dir else {
            return tool_error("source checkout is not available");
        };
        let matcher = match optional_glob_matcher(args.glob.as_deref()) {
            Ok(matcher) => matcher,
            Err(err) => return tool_error(err),
        };
        let mut files = Vec::new();
        collect_files(root, root, &mut |relative, _path| {
            if files.len() >= LIST_MAX_FILES {
                return;
            }
            if is_low_value_context_path(relative) {
                return;
            }
            if matcher
                .as_ref()
                .is_none_or(|matcher| matcher.is_match(relative))
            {
                files.push(relative.to_string());
            }
        });
        serde_json::json!({
            "ok": true,
            "files": files,
            "truncated": files.len() >= LIST_MAX_FILES
        })
    }

    fn resolve_safe_path(&self, raw_path: &str) -> Result<PathBuf, String> {
        let Some(root) = &self.source_dir else {
            return Err("source checkout is not available".into());
        };
        let normalized = normalize_path(raw_path);
        if normalized.is_empty() {
            return Err("path must not be empty".into());
        }
        if is_sensitive_path(&normalized) {
            return Err("path is blocked by AI review tool policy".into());
        }
        let relative = Path::new(&normalized);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err("path must be a relative repository file path".into());
        }
        let candidate = root.join(relative);
        let canonical_root = root
            .canonicalize()
            .map_err(|err| format!("failed to resolve source checkout: {err}"))?;
        let canonical_candidate = candidate
            .canonicalize()
            .map_err(|err| format!("failed to resolve file path: {err}"))?;
        if !canonical_candidate.starts_with(&canonical_root) {
            return Err("path escapes source checkout".into());
        }
        Ok(canonical_candidate)
    }
}

fn collect_files(root: &Path, current: &Path, visit: &mut impl FnMut(&str, &Path)) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = normalize_path(&relative.to_string_lossy());
            if is_low_value_context_path(&relative) || is_sensitive_path(&relative) {
                continue;
            }
            collect_files(root, &path, visit);
        } else if file_type.is_file() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = normalize_path(&relative.to_string_lossy());
            if is_sensitive_path(&relative) {
                continue;
            }
            visit(&relative, &path);
        }
    }
}

fn optional_glob_matcher(pattern: Option<&str>) -> Result<Option<globset::GlobMatcher>, String> {
    let Some(pattern) = pattern.map(str::trim).filter(|pattern| !pattern.is_empty()) else {
        return Ok(None);
    };
    globset::Glob::new(pattern)
        .map(|glob| Some(glob.compile_matcher()))
        .map_err(|err| format!("invalid glob: {err}"))
}

fn truncate_json_result(value: serde_json::Value, max_bytes: usize) -> String {
    let max_bytes = max_bytes.max(1);
    let text = value.to_string();
    if text.len() <= max_bytes {
        return text;
    }

    let minimal = serde_json::json!({
        "ok": true,
        "truncated": true,
    })
    .to_string();
    if minimal.len() > max_bytes {
        return "0".into();
    }

    let mut boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(text.len());
    let mut low = 0_usize;
    let mut high = boundaries.len();
    let mut best = minimal;
    while low < high {
        let middle = low + (high - low) / 2;
        let candidate = serde_json::json!({
            "ok": true,
            "truncated": true,
            "content": &text[..boundaries[middle]],
        })
        .to_string();
        if candidate.len() <= max_bytes {
            best = candidate;
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    best
}

fn tool_error(message: impl ToString) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "error": message.to_string()
    })
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

fn is_sensitive_path(path: &str) -> bool {
    let normalized = normalize_path(path).to_ascii_lowercase();
    normalized == ".env"
        || normalized.starts_with(".env.")
        || normalized.ends_with("/.env")
        || normalized.contains("/.env.")
        || normalized.contains("/.git/")
        || normalized.starts_with(".git/")
}

fn is_low_value_context_path(path: &str) -> bool {
    let normalized = normalize_path(path).to_ascii_lowercase();
    let components: Vec<_> = normalized.split('/').collect();
    if components.iter().any(|component| {
        matches!(
            *component,
            "node_modules"
                | "target"
                | "dist"
                | "build"
                | ".next"
                | ".nuxt"
                | "coverage"
                | "vendor"
        )
    }) {
        return true;
    }
    matches!(
        normalized.rsplit('/').next().unwrap_or(normalized.as_str()),
        "cargo.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lockb"
            | "composer.lock"
            | "poetry.lock"
    )
}

fn file_too_large(path: &Path, max_bytes: u64) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.len() > max_bytes)
        .unwrap_or(true)
}

fn read_utf8_file_limited(path: &Path, max_bytes: usize) -> Result<(String, bool), String> {
    let max_bytes = max_bytes.max(1);
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    let truncated = bytes.len() > max_bytes;
    if truncated {
        bytes.truncate(max_bytes);
    }
    loop {
        match String::from_utf8(bytes) {
            Ok(text) => return Ok((text, truncated)),
            Err(err) => {
                let utf8_error = err.utf8_error();
                if truncated && utf8_error.error_len().is_none() {
                    let valid_up_to = utf8_error.valid_up_to();
                    bytes = err.into_bytes();
                    bytes.truncate(valid_up_to);
                    continue;
                }
                return Err(format!("file is not valid UTF-8: {utf8_error}"));
            }
        }
    }
}

fn read_utf8_line_range_limited(
    path: &Path,
    start_line: usize,
    end_line: usize,
    max_bytes: usize,
) -> Result<(String, usize, bool), String> {
    if file_too_large(path, READ_MAX_FILE_BYTES as u64) {
        return Err(format!(
            "file exceeds internal {READ_MAX_FILE_BYTES} byte limit"
        ));
    }
    let content = fs::read_to_string(path).map_err(|err| err.to_string())?;
    let lines = content.split_inclusive('\n').collect::<Vec<_>>();
    if start_line > lines.len() {
        return Err(format!(
            "start_line {start_line} exceeds file line count {}",
            lines.len()
        ));
    }
    let actual_end_line = end_line.min(lines.len());
    let selected = lines[start_line - 1..actual_end_line].concat();
    let max_bytes = max_bytes.max(1);
    if selected.len() <= max_bytes {
        return Ok((selected, actual_end_line, false));
    }
    let mut end = max_bytes;
    while end > 0 && !selected.is_char_boundary(end) {
        end -= 1;
    }
    Ok((selected[..end].to_string(), actual_end_line, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::AiReviewConfig;
    use std::io::Write;

    fn test_config() -> AiReviewConfig {
        AiReviewConfig {
            id: "ai-review".into(),
            title: "AI Review".into(),
            base_url: "https://ai.example.com".into(),
            api_key: "test-key".into(),
            model: "test-model".into(),
            timeout_seconds: 60,
            request_timeout_seconds: None,
            max_batch_diff_bytes: 30_000,
            max_batches: 6,
            extra_instructions: String::new(),
            max_tool_calls: 8,
            max_tool_result_bytes: 60_000,
            max_tool_total_bytes: 40_000,
        }
    }

    #[test]
    fn search_code_returns_context_and_limits_matches_per_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("src/lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "before one\nneedle one\nafter one\nneedle two\nneedle three\nneedle four\nneedle five\nneedle six\n",
        )
        .unwrap();
        let context = AiReviewToolContext::new(&test_config(), Some(temp.path()));

        let result = context.search_code(r#"{"query":"needle","glob":"src/**/*.rs"}"#);
        let matches = result["matches"].as_array().unwrap();

        assert_eq!(matches.len(), SEARCH_MAX_MATCHES_PER_FILE);
        assert_eq!(matches[0]["before"], "before one");
        assert_eq!(matches[0]["text"], "needle one");
        assert_eq!(matches[0]["after"], "after one");
    }

    #[test]
    fn search_code_skips_low_value_and_large_files() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Cargo.lock"), "needle lock\n").unwrap();
        let large_file = temp.path().join("src/large.rs");
        std::fs::create_dir_all(large_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&large_file).unwrap();
        file.write_all(&vec![b'a'; SEARCH_MAX_FILE_BYTES as usize + 1])
            .unwrap();
        let context = AiReviewToolContext::new(&test_config(), Some(temp.path()));

        let result = context.search_code(r#"{"query":"needle"}"#);

        assert!(result["matches"].as_array().unwrap().is_empty());
    }

    #[test]
    fn list_files_skips_low_value_paths() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::create_dir_all(temp.path().join("node_modules/pkg")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn value() {}\n").unwrap();
        std::fs::write(temp.path().join("node_modules/pkg/index.js"), "needle\n").unwrap();
        std::fs::write(temp.path().join("package-lock.json"), "{}\n").unwrap();
        let context = AiReviewToolContext::new(&test_config(), Some(temp.path()));

        let result = context.list_files(r#"{}"#);
        let files = result["files"].as_array().unwrap();

        assert_eq!(files, &[serde_json::json!("src/lib.rs")]);
    }

    #[test]
    fn collect_files_prunes_low_value_directories() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::create_dir_all(temp.path().join("node_modules/pkg")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn value() {}\n").unwrap();
        std::fs::write(temp.path().join("node_modules/pkg/index.js"), "needle\n").unwrap();
        let mut visited = Vec::new();

        collect_files(temp.path(), temp.path(), &mut |relative, _path| {
            visited.push(relative.to_string());
        });
        visited.sort();

        assert_eq!(visited, vec!["src/lib.rs"]);
    }

    #[test]
    fn read_file_limits_content_and_preserves_utf8_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.txt");
        std::fs::write(&path, "一二三四五六").unwrap();
        let config = AiReviewConfig {
            max_tool_result_bytes: 10,
            ..test_config()
        };
        let context = AiReviewToolContext::new(&config, Some(temp.path()));

        let result = context.read_file(r#"{"path":"large.txt"}"#);

        assert_eq!(result["ok"], true);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["content"], "一二三");
    }

    #[test]
    fn read_file_returns_requested_inclusive_line_range() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("src/example.rs");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").unwrap();
        let context = AiReviewToolContext::new(&test_config(), Some(temp.path()));

        let result = context.read_file(r#"{"path":"src/example.rs","start_line":2,"end_line":4}"#);

        assert_eq!(result["ok"], true);
        assert_eq!(result["start_line"], 2);
        assert_eq!(result["end_line"], 4);
        assert_eq!(result["content"], "two\nthree\nfour\n");
    }

    #[test]
    fn read_file_rejects_invalid_line_ranges() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("example.rs"), "one\ntwo\n").unwrap();
        let context = AiReviewToolContext::new(&test_config(), Some(temp.path()));

        for arguments in [
            r#"{"path":"example.rs","start_line":0,"end_line":1}"#,
            r#"{"path":"example.rs","start_line":2,"end_line":1}"#,
            r#"{"path":"example.rs","start_line":1}"#,
            r#"{"path":"example.rs","end_line":1}"#,
            r#"{"path":"example.rs","start_line":1,"end_line":251}"#,
        ] {
            let result = context.read_file(arguments);
            assert_eq!(
                result["ok"], false,
                "arguments unexpectedly accepted: {arguments}"
            );
        }
    }

    #[test]
    fn context_tool_cache_key_normalizes_json_keys_and_path_separators() {
        assert_eq!(
            context_tool_cache_key(
                "read_file",
                r#"{"path":"src\\lib.rs","end_line":20,"start_line":10}"#,
            ),
            context_tool_cache_key(
                "read_file",
                r#"{"start_line":10,"path":"src/lib.rs","end_line":20}"#,
            ),
        );
    }

    #[test]
    fn truncated_json_result_never_exceeds_requested_byte_limit() {
        let value = serde_json::json!({
            "ok": true,
            "content": "一二三四五六七八九十".repeat(20),
        });

        for limit in [1, 4, 16, 40, 100] {
            let result = truncate_json_result(value.clone(), limit);
            assert!(
                result.len() <= limit,
                "result used {} bytes for limit {limit}: {result}",
                result.len()
            );
        }
    }
}
