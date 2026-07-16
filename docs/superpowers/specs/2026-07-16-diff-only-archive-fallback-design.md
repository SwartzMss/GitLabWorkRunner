# Diff-Only Archive Fallback and Script Support Removal

## Goal

Keep AI reviews available when a repository archive exceeds configured safety limits. In that case, the review continues from the GitLab merge request diff without repository context tools. At the same time, remove script-task support completely because it is no longer part of the product.

## Scope

This change:

- removes script-task configuration, selection, execution, reporting, documentation, and tests;
- preserves existing historical database rows so old review details remain readable;
- keeps repository context tools for AI reviews when archive preparation succeeds;
- falls back to a diff-only AI review only for `archive_limit_exceeded` errors raised while downloading or extracting the repository archive;
- records the fallback clearly in application logs;
- reuses the prepared context, including a diff-only result, for a clean second pass.

This change does not make other archive failures recoverable. Authentication failures, GitLab HTTP errors, timeouts, malformed ZIP files, filesystem errors, and other internal failures continue to fail the AI review.

## Runtime Flow

The service first fetches the complete merge request changes as it does today. For each selected AI review, it then attempts to prepare repository context from the archive.

If download and extraction succeed, the AI review receives both the MR diff and a source directory. The existing `read_file`, `search_code`, and `list_files` context tools remain available.

If either archive download or extraction returns the structured `archive_limit_exceeded` error, context preparation returns a successful diff-only outcome instead of propagating the error. The service logs a warning containing the project, merge request, commit, AI review ID, error code, and limit error summary. It then invokes the AI review with no source directory. The AI layer therefore exposes no repository context tools and reviews only the supplied MR diff.

The fallback is scoped to the affected AI review. It does not publish the existing run-level archive failure notification, and the AI task can finish successfully or fail later for an unrelated reason in the normal way.

For `second_pass_on_clean`, context preparation occurs once. The second pass reuses the same source directory or the same diff-only state and never retries the archive download.

## Error Classification

Fallback is based on `ReviewErrorCode::ArchiveLimitExceeded`, not rendered error text. This includes every configured archive hard limit:

- `max_archive_bytes` during download or extraction;
- `max_extracted_files`;
- `max_extracted_bytes`;
- `max_single_file_bytes`;
- `max_entry_path_bytes`.

All other error codes retain their current behavior. This prevents infrastructure, permission, timeout, corrupt-data, and filesystem problems from being silently hidden as reduced review coverage.

## Script Support Removal

Script tasks are removed from the active product surface:

- delete script-task rule models and parsing;
- delete automatic and manual script-task selection;
- delete the script runner, subprocess handling, result parsing, comment publishing, and script work-directory cleanup;
- remove script-task counts and execution branches from review orchestration;
- remove script-task-specific configuration examples, README sections, example scripts, and tests;
- remove script-task-specific Dashboard labels and presentation that are no longer reachable.

Database compatibility is intentionally conservative. Existing tables, generic task types, or historical rows that may contain `task_type = 'script_task'` are not destructively migrated away. Old runs remain queryable. New runs never create script-task records.

Archive downloading, extraction, limits, and AI context work-directory cleanup remain supported because AI context tools still depend on them. Code currently colocated in script-oriented modules will move or be renamed to an AI-context/archive boundary where necessary; unrelated refactoring is out of scope.

Top-level rules deserialization will reject unknown fields. Existing rules files containing `script_tasks` will therefore fail to load with a configuration error instead of silently ignoring commands that no longer run. This is an intentional breaking configuration change consistent with complete removal. The release notes and README must call it out.

## Observability and User Experience

An archive-limit fallback produces a warning log that explicitly says the AI review is continuing in diff-only mode. It includes enough identifiers to correlate the fallback with the review run.

No GitLab failure comment is posted merely because fallback occurred. Review findings and the final summary retain their existing format. This design does not add coverage details or fallback warnings to GitLab comments.

The Dashboard continues to report existing diff/batch coverage. No new schema field is required for fallback state; operational diagnosis uses the structured warning log. Historical script task rows remain renderable through generic or legacy-compatible paths, but the UI no longer advertises script tasks as a current capability.

## Testing

Tests will verify:

- an archive download exceeding `max_archive_bytes` continues into an AI review with no context tools;
- extraction exceeding each representative extraction limit follows the same diff-only path;
- diff-only fallback can produce and publish normal findings;
- a clean second pass does not retry archive preparation;
- archive timeout, permission, HTTP, malformed ZIP, and filesystem errors still fail;
- successful archive preparation still enables context tools;
- rules containing removed script-task configuration are rejected clearly;
- manual and automatic review selection no longer selects or runs scripts;
- current Dashboard, storage, configuration, and end-to-end tests pass after script code removal;
- documentation and example configuration contain no active script-task instructions.

## Success Criteria

A repository larger than an archive safety limit can complete an AI review from its MR diff without context-tool calls and without an archive-limit failure notification. Repositories within the limits retain context-assisted AI review. Script tasks cannot be configured, selected, triggered, or executed, while historical stored review data remains intact.
