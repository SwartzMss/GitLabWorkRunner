use crate::error::{AppResult, ReviewFailure};
use crate::review::ai::AiReviewExecutionMetadata;
use chrono::Utc;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{str::FromStr, time::Duration};
use tracing::info;

pub const REVIEW_TIMEZONE: &str = "UTC";

#[derive(Clone)]
pub struct StateStore {
    pool: SqlitePool,
}

#[derive(Clone, Debug)]
pub struct ReviewRequestStart<'a> {
    pub review_run_id: &'a str,
    pub trigger_type: &'a str,
    pub project_id: i64,
    pub project_name: Option<&'a str>,
    pub project_path_with_namespace: Option<&'a str>,
    pub mr_iid: i64,
    pub commit_sha: &'a str,
    pub note_id: Option<i64>,
    pub requested_ids_json: &'a str,
    pub selected_ai_reviews: usize,
}

#[derive(Clone, Debug)]
pub struct TaskRunStart<'a> {
    pub review_run_id: &'a str,
    pub task_type: &'a str,
    pub task_id: &'a str,
    pub title: &'a str,
}

#[derive(Clone, Debug)]
pub struct TaskRunFinish<'a> {
    pub review_run_id: &'a str,
    pub task_type: &'a str,
    pub task_id: &'a str,
    pub status: &'a str,
    pub findings: usize,
    pub comments: usize,
    pub error_code: Option<&'a str>,
    pub error: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct TaskRunProgress<'a> {
    pub review_run_id: &'a str,
    pub task_type: &'a str,
    pub task_id: &'a str,
    pub phase: &'a str,
    pub message: &'a str,
    pub current: Option<usize>,
    pub total: Option<usize>,
    pub unit: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct TaskRunExecutionMetadata<'a> {
    pub review_run_id: &'a str,
    pub task_type: &'a str,
    pub task_id: &'a str,
    pub metadata: &'a AiReviewExecutionMetadata,
}

#[derive(Clone, Debug)]
pub struct StoredReviewCoverage {
    pub total_files: usize,
    pub fully_reviewed_files: usize,
    pub partially_reviewed_files: usize,
    pub unreviewed_files: usize,
    pub total_diff_bytes: usize,
    pub reviewed_diff_bytes: usize,
    pub required_batches: usize,
    pub planned_batches: usize,
    pub completed_batches: usize,
    pub max_batches: usize,
    pub tool_rounds_used: usize,
    pub max_tool_rounds: usize,
    pub tool_calls_used: usize,
    pub max_tool_calls: usize,
    pub complete: bool,
}

#[derive(Clone, Debug)]
pub struct StoredReviewCoverageFile<'a> {
    pub path: &'a str,
    pub status: &'a str,
    pub reason: &'a str,
    pub total_diff_bytes: usize,
    pub reviewed_diff_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct StoredFinding<'a> {
    pub review_run_id: &'a str,
    pub task_type: &'a str,
    pub task_id: &'a str,
    pub rule_id: &'a str,
    pub severity: &'a str,
    pub path: &'a str,
    pub new_line: Option<i64>,
    pub title: &'a str,
    pub message: &'a str,
}

#[derive(Clone, Debug)]
pub struct StoredComment<'a> {
    pub review_run_id: &'a str,
    pub project_id: i64,
    pub mr_iid: i64,
    pub commit_sha: &'a str,
    pub rule_id: &'a str,
    pub path: &'a str,
    pub new_line: Option<i64>,
    pub discussion_id: Option<&'a str>,
    pub note_id: Option<i64>,
    pub publish_position: &'a str,
}

impl StateStore {
    pub async fn connect(database_url: &str) -> AppResult<Self> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(30));
        info!(database_url, "connecting state store");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        info!(database_url, "state store connected");
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> AppResult<()> {
        info!("state store migration started");
        sqlx::query(
            r#"
create table if not exists review_requests (
    id integer primary key autoincrement,
    review_run_id text not null unique,
    trigger_type text not null,
    project_id integer not null,
    mr_iid integer not null,
    commit_sha text not null,
    note_id integer,
    requested_ids_json text not null,
    selected_ai_reviews integer not null,
    selected_script_tasks integer not null,
    status text not null,
    findings integer not null default 0,
    comments integer not null default 0,
    timezone text not null,
    started_at text not null,
    finished_at text
);
"#,
        )
        .execute(&self.pool)
        .await?;
        self.ensure_column("review_requests", "project_name", "text")
            .await?;
        self.ensure_column("review_requests", "project_path_with_namespace", "text")
            .await?;
        self.ensure_column("review_requests", "error_code", "text")
            .await?;
        self.ensure_column("review_requests", "error", "text")
            .await?;
        sqlx::query(
            r#"
create table if not exists review_task_runs (
    id integer primary key autoincrement,
    review_run_id text not null,
    task_type text not null,
    task_id text not null,
    title text not null,
    status text not null,
    findings integer not null default 0,
    comments integer not null default 0,
    error text,
    execution_mode text,
    fallback_reason text,
    context_elapsed_ms integer,
    fallback_elapsed_ms integer,
    started_at text not null,
    finished_at text,
    unique(review_run_id, task_type, task_id)
);
"#,
        )
        .execute(&self.pool)
        .await?;
        self.ensure_column("review_task_runs", "error_code", "text")
            .await?;
        self.ensure_column("review_task_runs", "execution_mode", "text")
            .await?;
        self.ensure_column("review_task_runs", "fallback_reason", "text")
            .await?;
        self.ensure_column("review_task_runs", "context_elapsed_ms", "integer")
            .await?;
        self.ensure_column("review_task_runs", "fallback_elapsed_ms", "integer")
            .await?;
        for column in [
            "progress_current",
            "progress_total",
            "coverage_total_files",
            "coverage_fully_reviewed_files",
            "coverage_partially_reviewed_files",
            "coverage_unreviewed_files",
            "coverage_total_diff_bytes",
            "coverage_reviewed_diff_bytes",
            "coverage_required_batches",
            "coverage_planned_batches",
            "coverage_completed_batches",
            "coverage_max_batches",
            "tool_rounds_used",
            "max_tool_rounds",
            "tool_calls_used",
            "max_tool_calls",
            "coverage_complete",
        ] {
            self.ensure_column("review_task_runs", column, "integer")
                .await?;
        }
        for column in [
            "progress_phase",
            "progress_message",
            "progress_unit",
            "progress_updated_at",
        ] {
            self.ensure_column("review_task_runs", column, "text")
                .await?;
        }
        sqlx::query(
            r#"
create table if not exists review_coverage_files (
    id integer primary key autoincrement,
    review_run_id text not null,
    task_type text not null,
    task_id text not null,
    path text not null,
    status text not null,
    reason text not null,
    total_diff_bytes integer not null,
    reviewed_diff_bytes integer not null,
    created_at text not null
);
create index if not exists review_coverage_files_run_task
on review_coverage_files(review_run_id, task_type, task_id);
"#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
create table if not exists review_findings (
    id integer primary key autoincrement,
    review_run_id text not null,
    task_type text not null,
    task_id text not null,
    rule_id text not null,
    severity text not null,
    path text not null,
    new_line integer,
    title text not null,
    message text not null,
    created_at text not null
);
"#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
create table if not exists review_comment_records (
    id integer primary key autoincrement,
    review_run_id text not null,
    project_id integer not null,
    mr_iid integer not null,
    commit_sha text not null,
    rule_id text not null,
    path text not null,
    new_line integer,
    discussion_id text,
    note_id integer,
    publish_position text not null default 'inline',
    created_at text not null
);
"#,
        )
        .execute(&self.pool)
        .await?;
        self.ensure_column(
            "review_comment_records",
            "publish_position",
            "text not null default 'inline'",
        )
        .await?;
        sqlx::query(
            r#"
create table if not exists review_notifications (
    id integer primary key autoincrement,
    review_run_id text not null,
    project_id integer not null,
    mr_iid integer not null,
    commit_sha text not null,
    kind text not null,
    status text not null,
    discussion_id text,
    note_id integer,
    error text,
    created_at text not null
);
"#,
        )
        .execute(&self.pool)
        .await?;
        info!("state store migration completed");
        Ok(())
    }

    async fn ensure_column(&self, table: &str, column: &str, definition: &str) -> AppResult<()> {
        if !self.column_exists(table, column).await? {
            let sql = format!("alter table {table} add column {column} {definition}");
            if let Err(error) = sqlx::query(&sql).execute(&self.pool).await {
                if !self.column_exists(table, column).await? {
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    async fn column_exists(&self, table: &str, column: &str) -> AppResult<bool> {
        let pragma = format!("pragma table_info({table})");
        Ok(sqlx::query(&pragma)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .any(|row| row.get::<String, _>("name") == column))
    }

    pub async fn start_review_request(&self, request: &ReviewRequestStart<'_>) -> AppResult<()> {
        let now = now_rfc3339();
        sqlx::query(
            r#"
insert into review_requests
(review_run_id, trigger_type, project_id, project_name, project_path_with_namespace,
 mr_iid, commit_sha, note_id, requested_ids_json,
 selected_ai_reviews, selected_script_tasks, status, findings, comments, timezone, started_at)
values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 'running', 0, 0, ?, ?)
on conflict(review_run_id) do update set
    status = 'running',
    project_name = excluded.project_name,
    project_path_with_namespace = excluded.project_path_with_namespace,
    error_code = null,
    error = null,
    finished_at = null
"#,
        )
        .bind(request.review_run_id)
        .bind(request.trigger_type)
        .bind(request.project_id)
        .bind(request.project_name)
        .bind(request.project_path_with_namespace)
        .bind(request.mr_iid)
        .bind(request.commit_sha)
        .bind(request.note_id)
        .bind(request.requested_ids_json)
        .bind(request.selected_ai_reviews as i64)
        .bind(REVIEW_TIMEZONE)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        info!(
            review_run_id = %request.review_run_id,
            project_id = request.project_id,
            mr_iid = request.mr_iid,
            commit_sha = %request.commit_sha,
            note_id = ?request.note_id,
            "review request state recorded"
        );
        Ok(())
    }

    pub async fn finish_review_request(
        &self,
        review_run_id: &str,
        status: &str,
        findings: usize,
        comments: usize,
    ) -> AppResult<()> {
        self.finish_review_request_with_failure(review_run_id, status, findings, comments, None)
            .await
    }

    pub async fn finish_review_request_with_failure(
        &self,
        review_run_id: &str,
        status: &str,
        findings: usize,
        comments: usize,
        failure: Option<&ReviewFailure>,
    ) -> AppResult<()> {
        let now = now_rfc3339();
        sqlx::query(
            r#"
update review_requests
set status = ?, findings = ?, comments = ?, error_code = ?, error = ?, finished_at = ?
where review_run_id = ?
"#,
        )
        .bind(status)
        .bind(findings as i64)
        .bind(comments as i64)
        .bind(failure.map(|failure| failure.code.as_str()))
        .bind(failure.map(|failure| failure.message.as_str()))
        .bind(&now)
        .bind(review_run_id)
        .execute(&self.pool)
        .await?;
        info!(
            review_run_id,
            status, findings, comments, "review request state finalized"
        );
        Ok(())
    }

    pub async fn start_task_run(&self, task: &TaskRunStart<'_>) -> AppResult<()> {
        let now = now_rfc3339();
        sqlx::query(
            r#"
insert into review_task_runs
(review_run_id, task_type, task_id, title, status, findings, comments, started_at)
values (?, ?, ?, ?, 'running', 0, 0, ?)
on conflict(review_run_id, task_type, task_id) do update set
    title = excluded.title,
    status = 'running',
    error_code = null,
    error = null,
    progress_phase = null,
    progress_message = null,
    progress_current = null,
    progress_total = null,
    progress_unit = null,
    progress_updated_at = null,
    finished_at = null
"#,
        )
        .bind(task.review_run_id)
        .bind(task.task_type)
        .bind(task.task_id)
        .bind(task.title)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn finish_task_run(&self, task: &TaskRunFinish<'_>) -> AppResult<()> {
        let now = now_rfc3339();
        sqlx::query(
            r#"
update review_task_runs
set status = ?, findings = ?, comments = ?, error_code = ?, error = ?, finished_at = ?
where review_run_id = ? and task_type = ? and task_id = ?
"#,
        )
        .bind(task.status)
        .bind(task.findings as i64)
        .bind(task.comments as i64)
        .bind(task.error_code)
        .bind(task.error)
        .bind(&now)
        .bind(task.review_run_id)
        .bind(task.task_type)
        .bind(task.task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_task_progress(&self, progress: &TaskRunProgress<'_>) -> AppResult<()> {
        let now = now_rfc3339();
        sqlx::query(
            r#"
update review_task_runs
set progress_phase = ?, progress_message = ?, progress_current = ?, progress_total = ?,
    progress_unit = ?, progress_updated_at = ?
where review_run_id = ? and task_type = ? and task_id = ?
"#,
        )
        .bind(progress.phase)
        .bind(progress.message)
        .bind(progress.current.map(|value| value as i64))
        .bind(progress.total.map(|value| value as i64))
        .bind(progress.unit)
        .bind(&now)
        .bind(progress.review_run_id)
        .bind(progress.task_type)
        .bind(progress.task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_task_execution_metadata(
        &self,
        task: &TaskRunExecutionMetadata<'_>,
    ) -> AppResult<()> {
        sqlx::query(
            r#"
update review_task_runs
set execution_mode = ?, fallback_reason = ?, context_elapsed_ms = ?, fallback_elapsed_ms = ?
where review_run_id = ? and task_type = ? and task_id = ?
"#,
        )
        .bind(task.metadata.execution_mode.as_str())
        .bind(
            task.metadata
                .fallback_reason
                .map(|fallback_reason| fallback_reason.as_str()),
        )
        .bind(task.metadata.context_elapsed_ms.map(saturating_u64_to_i64))
        .bind(task.metadata.fallback_elapsed_ms.map(saturating_u64_to_i64))
        .bind(task.review_run_id)
        .bind(task.task_type)
        .bind(task.task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn finish_task_run_with_coverage(
        &self,
        task: &TaskRunFinish<'_>,
        coverage: &StoredReviewCoverage,
        files: &[StoredReviewCoverageFile<'_>],
    ) -> AppResult<()> {
        self.finish_task_run_with_coverage_and_metadata(task, coverage, files, None)
            .await
    }

    pub async fn finish_task_run_with_coverage_and_metadata(
        &self,
        task: &TaskRunFinish<'_>,
        coverage: &StoredReviewCoverage,
        files: &[StoredReviewCoverageFile<'_>],
        metadata: Option<&AiReviewExecutionMetadata>,
    ) -> AppResult<()> {
        self.finish_task_run_with_optional_coverage_and_metadata(
            task,
            Some(coverage),
            files,
            metadata,
        )
        .await
    }

    pub async fn finish_task_run_with_optional_coverage_and_metadata(
        &self,
        task: &TaskRunFinish<'_>,
        coverage: Option<&StoredReviewCoverage>,
        files: &[StoredReviewCoverageFile<'_>],
        metadata: Option<&AiReviewExecutionMetadata>,
    ) -> AppResult<()> {
        let now = now_rfc3339();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
update review_task_runs set
    status = ?, findings = ?, comments = ?, error_code = ?, error = ?, finished_at = ?,
    execution_mode = case when ? then ? else execution_mode end,
    fallback_reason = case when ? then ? else fallback_reason end,
    context_elapsed_ms = case when ? then ? else context_elapsed_ms end,
    fallback_elapsed_ms = case when ? then ? else fallback_elapsed_ms end
where review_run_id = ? and task_type = ? and task_id = ?
"#,
        )
        .bind(task.status)
        .bind(task.findings as i64)
        .bind(task.comments as i64)
        .bind(task.error_code)
        .bind(task.error)
        .bind(&now)
        .bind(metadata.is_some())
        .bind(metadata.map(|metadata| metadata.execution_mode.as_str()))
        .bind(metadata.is_some())
        .bind(metadata.and_then(|metadata| {
            metadata
                .fallback_reason
                .map(|fallback_reason| fallback_reason.as_str())
        }))
        .bind(metadata.is_some())
        .bind(metadata.and_then(|metadata| metadata.context_elapsed_ms.map(saturating_u64_to_i64)))
        .bind(metadata.is_some())
        .bind(metadata.and_then(|metadata| metadata.fallback_elapsed_ms.map(saturating_u64_to_i64)))
        .bind(task.review_run_id)
        .bind(task.task_type)
        .bind(task.task_id)
        .execute(&mut *tx)
        .await?;
        let Some(coverage) = coverage else {
            tx.commit().await?;
            return Ok(());
        };
        sqlx::query(
            r#"update review_task_runs set
coverage_total_files = ?, coverage_fully_reviewed_files = ?,
coverage_partially_reviewed_files = ?, coverage_unreviewed_files = ?,
coverage_total_diff_bytes = ?, coverage_reviewed_diff_bytes = ?,
coverage_required_batches = ?, coverage_planned_batches = ?,
coverage_completed_batches = ?, coverage_max_batches = ?,
tool_rounds_used = ?, max_tool_rounds = ?,
tool_calls_used = ?, max_tool_calls = ?, coverage_complete = ?
where review_run_id = ? and task_type = ? and task_id = ?"#,
        )
        .bind(coverage.total_files as i64)
        .bind(coverage.fully_reviewed_files as i64)
        .bind(coverage.partially_reviewed_files as i64)
        .bind(coverage.unreviewed_files as i64)
        .bind(coverage.total_diff_bytes as i64)
        .bind(coverage.reviewed_diff_bytes as i64)
        .bind(coverage.required_batches as i64)
        .bind(coverage.planned_batches as i64)
        .bind(coverage.completed_batches as i64)
        .bind(coverage.max_batches as i64)
        .bind(coverage.tool_rounds_used as i64)
        .bind(coverage.max_tool_rounds as i64)
        .bind(coverage.tool_calls_used as i64)
        .bind(coverage.max_tool_calls as i64)
        .bind(coverage.complete)
        .bind(task.review_run_id)
        .bind(task.task_type)
        .bind(task.task_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "delete from review_coverage_files where review_run_id = ? and task_type = ? and task_id = ?",
        )
        .bind(task.review_run_id)
        .bind(task.task_type)
        .bind(task.task_id)
        .execute(&mut *tx)
        .await?;
        for file in files {
            sqlx::query(
                r#"insert into review_coverage_files
(review_run_id, task_type, task_id, path, status, reason, total_diff_bytes, reviewed_diff_bytes, created_at)
values (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(task.review_run_id)
            .bind(task.task_type)
            .bind(task.task_id)
            .bind(file.path)
            .bind(file.status)
            .bind(file.reason)
            .bind(file.total_diff_bytes as i64)
            .bind(file.reviewed_diff_bytes as i64)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn record_finding(&self, finding: &StoredFinding<'_>) -> AppResult<()> {
        let now = now_rfc3339();
        sqlx::query(
            r#"
insert into review_findings
(review_run_id, task_type, task_id, rule_id, severity, path, new_line, title, message, created_at)
values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
"#,
        )
        .bind(finding.review_run_id)
        .bind(finding.task_type)
        .bind(finding.task_id)
        .bind(finding.rule_id)
        .bind(finding.severity)
        .bind(finding.path)
        .bind(finding.new_line)
        .bind(finding.title)
        .bind(finding.message)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_comment(&self, comment: &StoredComment<'_>) -> AppResult<()> {
        let now = now_rfc3339();
        sqlx::query(
            r#"
insert into review_comment_records
(review_run_id, project_id, mr_iid, commit_sha, rule_id, path, new_line, discussion_id, note_id, publish_position, created_at)
values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
"#,
        )
        .bind(comment.review_run_id)
        .bind(comment.project_id)
        .bind(comment.mr_iid)
        .bind(comment.commit_sha)
        .bind(comment.rule_id)
        .bind(comment.path)
        .bind(comment.new_line)
        .bind(comment.discussion_id)
        .bind(comment.note_id)
        .bind(comment.publish_position)
        .bind(now)
        .execute(&self.pool)
        .await?;
        info!(
            review_run_id = %comment.review_run_id,
            project_id = comment.project_id,
            mr_iid = comment.mr_iid,
            commit_sha = %comment.commit_sha,
            rule_id = %comment.rule_id,
            path = %comment.path,
            new_line = ?comment.new_line,
            discussion_id = ?comment.discussion_id,
            note_id = ?comment.note_id,
            publish_position = %comment.publish_position,
            "review comment state recorded"
        );
        Ok(())
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn saturating_u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::ai::{
        AiReviewExecutionMetadata, AiReviewExecutionMode, AiReviewFallbackReason,
    };

    #[tokio::test]
    async fn records_review_requests() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_review_request(&ReviewRequestStart {
                review_run_id: "rr-1",
                trigger_type: "manual_note",
                project_id: 1,
                project_name: Some("Runner"),
                project_path_with_namespace: Some("platform/runner"),
                mr_iid: 2,
                commit_sha: "abc",
                note_id: Some(9),
                requested_ids_json: r#"["ai-review"]"#,
                selected_ai_reviews: 1,
            })
            .await
            .unwrap();
        store
            .finish_review_request("rr-1", "completed", 3, 2)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar(
            "select count(*) from review_requests where review_run_id = ? and project_path_with_namespace = ?",
        )
                .bind("rr-1")
                .bind("platform/runner")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        let selected_script_tasks: i64 = sqlx::query_scalar(
            "select selected_script_tasks from review_requests where review_run_id = ?",
        )
        .bind("rr-1")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(selected_script_tasks, 0);
    }

    #[tokio::test]
    async fn records_task_progress() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_task_run(&TaskRunStart {
                review_run_id: "rr-progress",
                task_type: "ai_review",
                task_id: "ai-review",
                title: "AI Review",
            })
            .await
            .unwrap();

        store
            .update_task_progress(&TaskRunProgress {
                review_run_id: "rr-progress",
                task_type: "ai_review",
                task_id: "ai-review",
                phase: "reviewing_batch",
                message: "正在审查第 2 / 5 个批次",
                current: Some(2),
                total: Some(5),
                unit: Some("batch"),
            })
            .await
            .unwrap();

        let row = sqlx::query(
            "select progress_phase, progress_message, progress_current, progress_total, progress_unit, progress_updated_at from review_task_runs where review_run_id = ?",
        )
        .bind("rr-progress")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("progress_phase"), "reviewing_batch");
        assert_eq!(
            row.get::<String, _>("progress_message"),
            "正在审查第 2 / 5 个批次"
        );
        assert_eq!(row.get::<i64, _>("progress_current"), 2);
        assert_eq!(row.get::<i64, _>("progress_total"), 5);
        assert_eq!(row.get::<String, _>("progress_unit"), "batch");
        assert!(!row.get::<String, _>("progress_updated_at").is_empty());
    }

    #[tokio::test]
    async fn records_structured_review_request_failure() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_review_request(&ReviewRequestStart {
                review_run_id: "rr-failed",
                trigger_type: "manual_note",
                project_id: 1,
                project_name: None,
                project_path_with_namespace: None,
                mr_iid: 2,
                commit_sha: "abc",
                note_id: None,
                requested_ids_json: "[]",
                selected_ai_reviews: 1,
            })
            .await
            .unwrap();
        let failure = ReviewFailure::new(
            crate::error::ReviewErrorCode::ReviewRunTimeout,
            "review exceeded its deadline",
        );

        store
            .finish_review_request_with_failure("rr-failed", "failed", 0, 0, Some(&failure))
            .await
            .unwrap();

        let row =
            sqlx::query("select error_code, error from review_requests where review_run_id = ?")
                .bind("rr-failed")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(row.get::<String, _>("error_code"), "review_run_timeout");
        assert_eq!(
            row.get::<String, _>("error"),
            "review exceeded its deadline"
        );
    }

    #[tokio::test]
    async fn records_and_replaces_task_coverage_atomically() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_task_run(&TaskRunStart {
                review_run_id: "rr-coverage",
                task_type: "ai_review",
                task_id: "ai-review",
                title: "AI Review",
            })
            .await
            .unwrap();
        let coverage = StoredReviewCoverage {
            total_files: 3,
            fully_reviewed_files: 1,
            partially_reviewed_files: 1,
            unreviewed_files: 1,
            total_diff_bytes: 30,
            reviewed_diff_bytes: 15,
            required_batches: 3,
            planned_batches: 2,
            completed_batches: 2,
            max_batches: 4,
            tool_rounds_used: 2,
            max_tool_rounds: 3,
            tool_calls_used: 5,
            max_tool_calls: 8,
            complete: false,
        };
        let file = StoredReviewCoverageFile {
            path: "src/a.rs",
            status: "partial",
            reason: "single_file_diff_truncated",
            total_diff_bytes: 20,
            reviewed_diff_bytes: 10,
        };
        let finish = TaskRunFinish {
            review_run_id: "rr-coverage",
            task_type: "ai_review",
            task_id: "ai-review",
            status: "completed",
            findings: 0,
            comments: 0,
            error_code: None,
            error: None,
        };

        store
            .finish_task_run_with_coverage(&finish, &coverage, std::slice::from_ref(&file))
            .await
            .unwrap();
        store
            .finish_task_run_with_coverage(&finish, &coverage, &[file])
            .await
            .unwrap();

        let rows: i64 = sqlx::query_scalar(
            "select count(*) from review_coverage_files where review_run_id = 'rr-coverage'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        let task_row = sqlx::query("select coverage_reviewed_diff_bytes, coverage_max_batches, tool_rounds_used, max_tool_rounds, tool_calls_used, max_tool_calls from review_task_runs where review_run_id = 'rr-coverage'")
            .fetch_one(&store.pool).await.unwrap();
        assert_eq!(rows, 1);
        assert_eq!(task_row.get::<i64, _>("coverage_reviewed_diff_bytes"), 15);
        assert_eq!(task_row.get::<i64, _>("coverage_max_batches"), 4);
        assert_eq!(task_row.get::<i64, _>("tool_rounds_used"), 2);
        assert_eq!(task_row.get::<i64, _>("max_tool_rounds"), 3);
        assert_eq!(task_row.get::<i64, _>("tool_calls_used"), 5);
        assert_eq!(task_row.get::<i64, _>("max_tool_calls"), 8);
    }

    #[tokio::test]
    async fn records_ai_execution_metadata_with_task_coverage() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_task_run(&TaskRunStart {
                review_run_id: "rr-metadata",
                task_type: "ai_review",
                task_id: "ai-review",
                title: "AI Review",
            })
            .await
            .unwrap();
        let coverage = StoredReviewCoverage {
            total_files: 1,
            fully_reviewed_files: 1,
            partially_reviewed_files: 0,
            unreviewed_files: 0,
            total_diff_bytes: 10,
            reviewed_diff_bytes: 10,
            required_batches: 1,
            planned_batches: 1,
            completed_batches: 1,
            max_batches: 1,
            tool_rounds_used: 1,
            max_tool_rounds: 2,
            tool_calls_used: 2,
            max_tool_calls: 4,
            complete: true,
        };
        let finish = TaskRunFinish {
            review_run_id: "rr-metadata",
            task_type: "ai_review",
            task_id: "ai-review",
            status: "completed",
            findings: 0,
            comments: 0,
            error_code: None,
            error: None,
        };
        let metadata = AiReviewExecutionMetadata {
            execution_mode: AiReviewExecutionMode::DiffOnlyFallback,
            fallback_reason: Some(AiReviewFallbackReason::AiToolLoopTimeout),
            context_elapsed_ms: Some(2_400_000),
            fallback_elapsed_ms: Some(386_000),
        };

        store
            .finish_task_run_with_coverage_and_metadata(&finish, &coverage, &[], Some(&metadata))
            .await
            .unwrap();
        store
            .finish_task_run_with_coverage(&finish, &coverage, &[])
            .await
            .unwrap();

        let row = sqlx::query(
            "select execution_mode, fallback_reason, context_elapsed_ms, fallback_elapsed_ms from review_task_runs where review_run_id = ?",
        )
        .bind("rr-metadata")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("execution_mode"), "diff_only_fallback");
        assert_eq!(
            row.get::<String, _>("fallback_reason"),
            "ai_tool_loop_timeout"
        );
        assert_eq!(row.get::<i64, _>("context_elapsed_ms"), 2_400_000);
        assert_eq!(row.get::<i64, _>("fallback_elapsed_ms"), 386_000);
    }

    #[tokio::test]
    async fn updates_ai_execution_metadata_before_task_finishes() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_task_run(&TaskRunStart {
                review_run_id: "rr-running-metadata",
                task_type: "ai_review",
                task_id: "ai-review",
                title: "AI Review",
            })
            .await
            .unwrap();
        let metadata = AiReviewExecutionMetadata {
            execution_mode: AiReviewExecutionMode::DiffOnlyFallback,
            fallback_reason: Some(AiReviewFallbackReason::AiRequestTimeout),
            context_elapsed_ms: Some(2_400_000),
            fallback_elapsed_ms: None,
        };

        store
            .update_task_execution_metadata(&TaskRunExecutionMetadata {
                review_run_id: "rr-running-metadata",
                task_type: "ai_review",
                task_id: "ai-review",
                metadata: &metadata,
            })
            .await
            .unwrap();

        let row = sqlx::query(
            "select status, execution_mode, fallback_reason, context_elapsed_ms, fallback_elapsed_ms from review_task_runs where review_run_id = ?",
        )
        .bind("rr-running-metadata")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("status"), "running");
        assert_eq!(row.get::<String, _>("execution_mode"), "diff_only_fallback");
        assert_eq!(
            row.get::<String, _>("fallback_reason"),
            "ai_request_timeout"
        );
        assert_eq!(row.get::<i64, _>("context_elapsed_ms"), 2_400_000);
        assert!(row.get::<Option<i64>, _>("fallback_elapsed_ms").is_none());
    }

    #[tokio::test]
    async fn updates_context_execution_metadata_before_task_finishes() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_task_run(&TaskRunStart {
                review_run_id: "rr-running-context",
                task_type: "ai_review",
                task_id: "ai-review",
                title: "AI Review",
            })
            .await
            .unwrap();
        let metadata = AiReviewExecutionMetadata {
            execution_mode: AiReviewExecutionMode::Context,
            fallback_reason: None,
            context_elapsed_ms: Some(215),
            fallback_elapsed_ms: None,
        };

        store
            .update_task_execution_metadata(&TaskRunExecutionMetadata {
                review_run_id: "rr-running-context",
                task_type: "ai_review",
                task_id: "ai-review",
                metadata: &metadata,
            })
            .await
            .unwrap();

        let row = sqlx::query(
            "select status, execution_mode, fallback_reason, context_elapsed_ms, fallback_elapsed_ms from review_task_runs where review_run_id = ?",
        )
        .bind("rr-running-context")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("status"), "running");
        assert_eq!(row.get::<String, _>("execution_mode"), "context");
        assert!(row.get::<Option<String>, _>("fallback_reason").is_none());
        assert_eq!(row.get::<i64, _>("context_elapsed_ms"), 215);
        assert!(row.get::<Option<i64>, _>("fallback_elapsed_ms").is_none());
    }

    #[tokio::test]
    async fn execution_metadata_elapsed_milliseconds_saturate_at_i64_max() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_task_run(&TaskRunStart {
                review_run_id: "rr-saturated-metadata",
                task_type: "ai_review",
                task_id: "ai-review",
                title: "AI Review",
            })
            .await
            .unwrap();
        let coverage = StoredReviewCoverage {
            total_files: 0,
            fully_reviewed_files: 0,
            partially_reviewed_files: 0,
            unreviewed_files: 0,
            total_diff_bytes: 0,
            reviewed_diff_bytes: 0,
            required_batches: 0,
            planned_batches: 0,
            completed_batches: 0,
            max_batches: 0,
            tool_rounds_used: 0,
            max_tool_rounds: 0,
            tool_calls_used: 0,
            max_tool_calls: 0,
            complete: true,
        };
        let finish = TaskRunFinish {
            review_run_id: "rr-saturated-metadata",
            task_type: "ai_review",
            task_id: "ai-review",
            status: "completed",
            findings: 0,
            comments: 0,
            error_code: None,
            error: None,
        };
        let metadata = AiReviewExecutionMetadata {
            execution_mode: AiReviewExecutionMode::Context,
            fallback_reason: None,
            context_elapsed_ms: Some(u64::MAX),
            fallback_elapsed_ms: Some(u64::MAX),
        };

        store
            .finish_task_run_with_coverage_and_metadata(&finish, &coverage, &[], Some(&metadata))
            .await
            .unwrap();

        let row = sqlx::query(
            "select context_elapsed_ms, fallback_elapsed_ms from review_task_runs where review_run_id = ?",
        )
        .bind("rr-saturated-metadata")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(row.get::<i64, _>("context_elapsed_ms"), i64::MAX);
        assert_eq!(row.get::<i64, _>("fallback_elapsed_ms"), i64::MAX);
    }

    #[tokio::test]
    async fn records_metadata_without_fabricating_coverage() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_task_run(&TaskRunStart {
                review_run_id: "rr-no-coverage",
                task_type: "ai_review",
                task_id: "ai-review",
                title: "AI Review",
            })
            .await
            .unwrap();
        let finish = TaskRunFinish {
            review_run_id: "rr-no-coverage",
            task_type: "ai_review",
            task_id: "ai-review",
            status: "failed",
            findings: 0,
            comments: 0,
            error_code: Some("archive_download_failed"),
            error: Some("archive failed before coverage planning"),
        };
        let metadata = AiReviewExecutionMetadata {
            execution_mode: AiReviewExecutionMode::Context,
            fallback_reason: None,
            context_elapsed_ms: Some(42),
            fallback_elapsed_ms: None,
        };

        store
            .finish_task_run_with_optional_coverage_and_metadata(
                &finish,
                None,
                &[],
                Some(&metadata),
            )
            .await
            .unwrap();

        let row = sqlx::query("select status, error_code, execution_mode, context_elapsed_ms, coverage_total_files, coverage_complete from review_task_runs where review_run_id = ?")
            .bind("rr-no-coverage")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("status"), "failed");
        assert_eq!(
            row.get::<String, _>("error_code"),
            "archive_download_failed"
        );
        assert_eq!(row.get::<String, _>("execution_mode"), "context");
        assert_eq!(row.get::<i64, _>("context_elapsed_ms"), 42);
        assert!(row.get::<Option<i64>, _>("coverage_total_files").is_none());
        assert!(row.get::<Option<bool>, _>("coverage_complete").is_none());
        let files = sqlx::query(
            "select count(*) as count from review_coverage_files where review_run_id = ?",
        )
        .bind("rr-no-coverage")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(files.get::<i64, _>("count"), 0);

        let coverage = StoredReviewCoverage {
            total_files: 1,
            fully_reviewed_files: 0,
            partially_reviewed_files: 1,
            unreviewed_files: 0,
            total_diff_bytes: 10,
            reviewed_diff_bytes: 5,
            required_batches: 1,
            planned_batches: 1,
            completed_batches: 1,
            max_batches: 1,
            tool_rounds_used: 1,
            max_tool_rounds: 2,
            tool_calls_used: 1,
            max_tool_calls: 2,
            complete: false,
        };
        let file = StoredReviewCoverageFile {
            path: "src/lib.rs",
            status: "partial",
            reason: "batch_execution_failed",
            total_diff_bytes: 10,
            reviewed_diff_bytes: 5,
        };
        store
            .finish_task_run_with_coverage(&finish, &coverage, &[file])
            .await
            .unwrap();
        store
            .finish_task_run_with_optional_coverage_and_metadata(
                &finish,
                None,
                &[],
                Some(&metadata),
            )
            .await
            .unwrap();
        let preserved = sqlx::query(
            "select coverage_total_files from review_task_runs where review_run_id = ?",
        )
        .bind("rr-no-coverage")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(preserved.get::<i64, _>("coverage_total_files"), 1);
        let files = sqlx::query(
            "select count(*) as count from review_coverage_files where review_run_id = ?",
        )
        .bind("rr-no-coverage")
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(files.get::<i64, _>("count"), 1);
    }

    #[tokio::test]
    async fn concurrent_migrations_from_two_connections_both_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let database_url = format!("sqlite://{}", dir.path().join("state.db").display());
        let first = StateStore::connect(&database_url).await.unwrap();
        let second = StateStore::connect(&database_url).await.unwrap();

        let (first_result, second_result) = tokio::join!(first.migrate(), second.migrate());

        first_result.unwrap();
        second_result.unwrap();
        let columns = sqlx::query("pragma table_info(review_task_runs)")
            .fetch_all(&first.pool)
            .await
            .unwrap();
        for expected in [
            "execution_mode",
            "fallback_reason",
            "context_elapsed_ms",
            "fallback_elapsed_ms",
        ] {
            assert!(columns
                .iter()
                .any(|row| row.get::<String, _>("name") == expected));
        }
    }

    #[tokio::test]
    async fn migration_keeps_existing_rows_metadata_null_and_is_idempotent() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            r#"create table review_task_runs (
                id integer primary key autoincrement,
                review_run_id text not null,
                task_type text not null,
                task_id text not null,
                title text not null,
                status text not null,
                findings integer not null default 0,
                comments integer not null default 0,
                error text,
                started_at text not null,
                finished_at text,
                unique(review_run_id, task_type, task_id)
            )"#,
        )
        .execute(&store.pool)
        .await
        .unwrap();
        sqlx::query("insert into review_task_runs (review_run_id, task_type, task_id, title, status, started_at) values ('legacy', 'ai_review', 'ai-review', 'AI Review', 'completed', 'now')")
            .execute(&store.pool)
            .await
            .unwrap();

        store.migrate().await.unwrap();
        store.migrate().await.unwrap();

        let row = sqlx::query(
            "select execution_mode, fallback_reason, context_elapsed_ms, fallback_elapsed_ms from review_task_runs where review_run_id = 'legacy'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert!(row.get::<Option<String>, _>("execution_mode").is_none());
        assert!(row.get::<Option<String>, _>("fallback_reason").is_none());
        assert!(row.get::<Option<i64>, _>("context_elapsed_ms").is_none());
        assert!(row.get::<Option<i64>, _>("fallback_elapsed_ms").is_none());
    }
}
