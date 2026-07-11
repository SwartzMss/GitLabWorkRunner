use crate::{
    error::{AppError, AppResult},
    storage::REVIEW_TIMEZONE,
};
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
    QueryBuilder, Row, Sqlite, SqlitePool,
};
use std::{str::FromStr, time::Duration};
use tracing::info;

#[derive(Clone)]
pub struct DashboardStore {
    pool: SqlitePool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunListParams {
    pub status: Option<String>,
    pub project: Option<String>,
    pub project_id: Option<i64>,
    pub mr_iid: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DashboardListParams {
    pub project: Option<String>,
    pub project_id: Option<i64>,
    pub mr_iid: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardSummary {
    pub timezone: String,
    pub total_runs: i64,
    pub running_runs: i64,
    pub completed_runs: i64,
    pub failed_runs: i64,
    pub total_projects: i64,
    pub total_merge_requests: i64,
    pub total_findings: i64,
    pub total_comments: i64,
    pub last_review_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardRun {
    pub review_run_id: String,
    pub trigger_type: String,
    pub project_id: i64,
    pub project_label: String,
    pub mr_iid: i64,
    pub commit_sha: String,
    pub note_id: Option<i64>,
    pub requested_ids_json: String,
    pub selected_ai_reviews: i64,
    pub selected_script_tasks: i64,
    pub status: String,
    pub findings: i64,
    pub comments: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub total_task_runs: i64,
    pub completed_task_runs: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FindingSeveritySummary {
    pub total: i64,
    pub error: i64,
    pub warning: i64,
    pub info: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardProject {
    pub project_id: i64,
    pub project_label: String,
    pub total_runs: i64,
    pub running_runs: i64,
    pub failed_runs: i64,
    pub total_merge_requests: i64,
    pub total_findings: i64,
    pub total_comments: i64,
    pub last_review_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardMergeRequest {
    pub project_id: i64,
    pub project_label: String,
    pub mr_iid: i64,
    pub total_runs: i64,
    pub running_runs: i64,
    pub failed_runs: i64,
    pub total_findings: i64,
    pub total_comments: i64,
    pub last_commit_sha: Option<String>,
    pub last_status: Option<String>,
    pub last_review_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardTaskRun {
    pub task_type: String,
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub findings: i64,
    pub comments: i64,
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub coverage_total_files: Option<i64>,
    pub coverage_fully_reviewed_files: Option<i64>,
    pub coverage_partially_reviewed_files: Option<i64>,
    pub coverage_unreviewed_files: Option<i64>,
    pub coverage_total_diff_bytes: Option<i64>,
    pub coverage_reviewed_diff_bytes: Option<i64>,
    pub coverage_required_batches: Option<i64>,
    pub coverage_planned_batches: Option<i64>,
    pub coverage_completed_batches: Option<i64>,
    pub coverage_max_batches: Option<i64>,
    pub tool_calls_used: Option<i64>,
    pub max_tool_calls: Option<i64>,
    pub coverage_complete: Option<bool>,
    pub incomplete_files: Vec<DashboardCoverageFile>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardCoverageFile {
    pub path: String,
    pub status: String,
    pub reason: String,
    pub total_diff_bytes: i64,
    pub reviewed_diff_bytes: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardFinding {
    pub review_run_id: String,
    pub project_id: i64,
    pub project_label: String,
    pub mr_iid: i64,
    pub commit_sha: String,
    pub task_type: String,
    pub task_id: String,
    pub rule_id: String,
    pub severity: String,
    pub path: String,
    pub new_line: Option<i64>,
    pub title: String,
    pub message: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardComment {
    pub review_run_id: String,
    pub project_id: i64,
    pub project_label: String,
    pub mr_iid: i64,
    pub commit_sha: String,
    pub rule_id: String,
    pub path: String,
    pub new_line: Option<i64>,
    pub discussion_id: Option<String>,
    pub note_id: Option<i64>,
    pub publish_position: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardRunDetail {
    pub run: DashboardRun,
    pub failure: Option<DashboardFailure>,
    pub tasks: Vec<DashboardTaskRun>,
    pub findings: Vec<DashboardFinding>,
    pub comments: Vec<DashboardComment>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardFailure {
    pub code: Option<String>,
    pub message: String,
}

impl DashboardStore {
    pub async fn connect(database_url: &str) -> AppResult<Self> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(false)
            .read_only(true)
            .busy_timeout(Duration::from_secs(5));
        info!(database_url, "connecting dashboard store");
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        let store = Self { pool };
        store.check_schema().await?;
        info!(database_url, "dashboard store connected");
        Ok(store)
    }

    pub async fn check_schema(&self) -> AppResult<()> {
        let required = [
            "review_requests",
            "review_task_runs",
            "review_findings",
            "review_comment_records",
            "review_coverage_files",
        ];
        for table in required {
            let exists: Option<i64> = sqlx::query_scalar(
                "select 1 from sqlite_master where type = 'table' and name = ? limit 1",
            )
            .bind(table)
            .fetch_optional(&self.pool)
            .await?;
            if exists.is_none() {
                return Err(AppError::Storage(format!(
                    "dashboard database is missing required table `{table}`; start gitlab-work-runner once to run migrations"
                )));
            }
        }
        for column in ["project_name", "project_path_with_namespace"] {
            if !self.column_exists("review_requests", column).await? {
                return Err(AppError::Storage(format!(
                    "dashboard database is missing required column `review_requests.{column}`; start gitlab-work-runner once to run migrations"
                )));
            }
        }
        for column in ["error_code", "error"] {
            if !self.column_exists("review_requests", column).await? {
                return Err(AppError::Storage(format!(
                    "dashboard database is missing required column `review_requests.{column}`; start gitlab-work-runner once to run migrations"
                )));
            }
        }
        for column in [
            "error_code",
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
            "tool_calls_used",
            "max_tool_calls",
            "coverage_complete",
        ] {
            if !self.column_exists("review_task_runs", column).await? {
                return Err(AppError::Storage(format!(
                    "dashboard database is missing required column `review_task_runs.{column}`; start gitlab-work-runner once to run migrations"
                )));
            }
        }
        Ok(())
    }

    async fn column_exists(&self, table: &str, column: &str) -> AppResult<bool> {
        let pragma = format!("pragma table_info({table})");
        let rows = sqlx::query(&pragma).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().any(|row| {
            let name: String = row.get("name");
            name == column
        }))
    }

    pub async fn summary(&self) -> AppResult<DashboardSummary> {
        let row = sqlx::query(
            r#"
select
    count(*) as total_runs,
    sum(case when effective_status = 'running' then 1 else 0 end) as running_runs,
    sum(case when effective_status = 'completed' then 1 else 0 end) as completed_runs,
    sum(case when effective_status = 'failed' then 1 else 0 end) as failed_runs,
    count(distinct project_id) as total_projects,
    count(distinct project_id || ':' || mr_iid) as total_merge_requests,
    coalesce(sum(findings), 0) as total_findings,
    coalesce(sum(comments), 0) as total_comments,
    max(started_at) as last_review_at
from (
    select rr.*,
        case
            when rr.status = 'running' then 'running'
            when rr.status = 'completed'
                and not exists (
                    select 1 from review_task_runs task
                    where task.review_run_id = rr.review_run_id and task.status <> 'completed'
                )
                then 'completed'
            else 'failed'
        end as effective_status
    from review_requests rr
) runs
"#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(DashboardSummary {
            timezone: REVIEW_TIMEZONE.into(),
            total_runs: row.get("total_runs"),
            running_runs: row.get::<Option<i64>, _>("running_runs").unwrap_or(0),
            completed_runs: row.get::<Option<i64>, _>("completed_runs").unwrap_or(0),
            failed_runs: row.get::<Option<i64>, _>("failed_runs").unwrap_or(0),
            total_projects: row.get("total_projects"),
            total_merge_requests: row.get("total_merge_requests"),
            total_findings: row.get("total_findings"),
            total_comments: row.get("total_comments"),
            last_review_at: row.get("last_review_at"),
        })
    }

    pub async fn finding_summary(&self) -> AppResult<FindingSeveritySummary> {
        let row = sqlx::query(
            r#"
select
    count(*) as total,
    sum(case when severity = 'error' then 1 else 0 end) as error,
    sum(case when severity = 'warning' then 1 else 0 end) as warning,
    sum(case when severity = 'info' then 1 else 0 end) as info
from review_findings
"#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(FindingSeveritySummary {
            total: row.get("total"),
            error: row.get::<Option<i64>, _>("error").unwrap_or(0),
            warning: row.get::<Option<i64>, _>("warning").unwrap_or(0),
            info: row.get::<Option<i64>, _>("info").unwrap_or(0),
        })
    }

    pub async fn runs(&self, params: &RunListParams) -> AppResult<Vec<DashboardRun>> {
        let mut builder: QueryBuilder<'_, Sqlite> = QueryBuilder::new(
            r#"
select
    rr.review_run_id, rr.trigger_type, rr.project_id, rr.mr_iid, rr.commit_sha, rr.note_id, rr.requested_ids_json,
    coalesce(nullif(rr.project_path_with_namespace, ''), nullif(rr.project_name, ''), '#' || rr.project_id) as project_label,
    rr.selected_ai_reviews, rr.selected_script_tasks,
    case
        when rr.status = 'running' then 'running'
        when rr.status = 'completed'
            and not exists (
                select 1 from review_task_runs task
                where task.review_run_id = rr.review_run_id and task.status <> 'completed'
            )
            then 'completed'
        else 'failed'
    end as status,
    rr.findings, rr.comments, rr.started_at, rr.finished_at,
    cast((julianday(coalesce(rr.finished_at, datetime('now'))) - julianday(rr.started_at)) * 86400000 as integer) as duration_ms,
    coalesce((select count(*) from review_task_runs task where task.review_run_id = rr.review_run_id), 0) as total_task_runs,
    coalesce((select count(*) from review_task_runs task where task.review_run_id = rr.review_run_id and task.status = 'completed'), 0) as completed_task_runs
from review_requests rr
where 1 = 1
"#,
        );
        push_run_filters(&mut builder, params);
        builder.push(" order by started_at desc limit ");
        builder.push_bind(params.limit.unwrap_or(50).clamp(1, 200));
        builder.push(" offset ");
        builder.push_bind(params.offset.unwrap_or(0).max(0));
        let rows = builder.build().fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(row_to_run).collect())
    }

    pub async fn run_detail(&self, review_run_id: &str) -> AppResult<Option<DashboardRunDetail>> {
        let run = sqlx::query(
            r#"
select
    rr.review_run_id, rr.trigger_type, rr.project_id, rr.mr_iid, rr.commit_sha, rr.note_id, rr.requested_ids_json,
    coalesce(nullif(rr.project_path_with_namespace, ''), nullif(rr.project_name, ''), '#' || rr.project_id) as project_label,
    rr.selected_ai_reviews, rr.selected_script_tasks,
    case
        when rr.status = 'running' then 'running'
        when rr.status = 'completed'
            and not exists (
                select 1 from review_task_runs task
                where task.review_run_id = rr.review_run_id and task.status <> 'completed'
            )
            then 'completed'
        else 'failed'
    end as status,
    rr.findings, rr.comments, rr.error_code, rr.error, rr.started_at, rr.finished_at,
    cast((julianday(coalesce(rr.finished_at, datetime('now'))) - julianday(rr.started_at)) * 86400000 as integer) as duration_ms,
    coalesce((select count(*) from review_task_runs task where task.review_run_id = rr.review_run_id), 0) as total_task_runs,
    coalesce((select count(*) from review_task_runs task where task.review_run_id = rr.review_run_id and task.status = 'completed'), 0) as completed_task_runs
from review_requests rr
where review_run_id = ?
"#,
        )
        .bind(review_run_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = run else {
            return Ok(None);
        };
        let mut failure = dashboard_failure(
            row.get::<Option<String>, _>("error_code"),
            row.get::<Option<String>, _>("error"),
        );
        let run = row_to_run(row);
        let tasks = self.tasks(review_run_id).await?;
        if failure.is_none() && run.status == "failed" {
            failure = tasks.iter().find_map(dashboard_task_failure);
        }
        let findings = self.findings(review_run_id).await?;
        let comments = self.comments(review_run_id).await?;
        Ok(Some(DashboardRunDetail {
            run,
            failure,
            tasks,
            findings,
            comments,
        }))
    }

    pub async fn projects(&self) -> AppResult<Vec<DashboardProject>> {
        let rows = sqlx::query(
            r#"
select
    project_id,
    (
        select coalesce(nullif(latest.project_path_with_namespace, ''), nullif(latest.project_name, ''), '#' || latest.project_id)
        from review_requests latest
        where latest.project_id = grouped.project_id
        order by started_at desc
        limit 1
    ) as project_label,
    count(*) as total_runs,
    sum(case when status = 'running' then 1 else 0 end) as running_runs,
    sum(case when status <> 'running' and not (
        status = 'completed'
        and not exists (
            select 1 from review_task_runs task
            where task.review_run_id = grouped.review_run_id and task.status <> 'completed'
        )
    ) then 1 else 0 end) as failed_runs,
    count(distinct mr_iid) as total_merge_requests,
    coalesce(sum(findings), 0) as total_findings,
    coalesce(sum(comments), 0) as total_comments,
    max(started_at) as last_review_at
from review_requests grouped
group by project_id
order by last_review_at desc
"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| DashboardProject {
                project_id: row.get("project_id"),
                project_label: row.get("project_label"),
                total_runs: row.get("total_runs"),
                running_runs: row.get::<Option<i64>, _>("running_runs").unwrap_or(0),
                failed_runs: row.get::<Option<i64>, _>("failed_runs").unwrap_or(0),
                total_merge_requests: row.get("total_merge_requests"),
                total_findings: row.get("total_findings"),
                total_comments: row.get("total_comments"),
                last_review_at: row.get("last_review_at"),
            })
            .collect())
    }

    pub async fn merge_requests(&self) -> AppResult<Vec<DashboardMergeRequest>> {
        let rows = sqlx::query(
            r#"
select
    project_id,
    (
        select coalesce(nullif(latest.project_path_with_namespace, ''), nullif(latest.project_name, ''), '#' || latest.project_id)
        from review_requests latest
        where latest.project_id = grouped.project_id and latest.mr_iid = grouped.mr_iid
        order by started_at desc
        limit 1
    ) as project_label,
    mr_iid,
    count(*) as total_runs,
    sum(case when status = 'running' then 1 else 0 end) as running_runs,
    sum(case when status <> 'running' and not (
        status = 'completed'
        and not exists (
            select 1 from review_task_runs task
            where task.review_run_id = grouped.review_run_id and task.status <> 'completed'
        )
    ) then 1 else 0 end) as failed_runs,
    coalesce(sum(findings), 0) as total_findings,
    coalesce(sum(comments), 0) as total_comments,
    (
        select commit_sha from review_requests latest
        where latest.project_id = grouped.project_id and latest.mr_iid = grouped.mr_iid
        order by started_at desc
        limit 1
    ) as last_commit_sha,
    (
        select case
            when latest.status = 'running' then 'running'
            when latest.status = 'completed'
                and not exists (
                    select 1 from review_task_runs task
                    where task.review_run_id = latest.review_run_id and task.status <> 'completed'
                )
                then 'completed'
            else 'failed'
        end
        from review_requests latest
        where latest.project_id = grouped.project_id and latest.mr_iid = grouped.mr_iid
        order by started_at desc
        limit 1
    ) as last_status,
    max(started_at) as last_review_at
from review_requests grouped
group by project_id, mr_iid
order by last_review_at desc
limit 300
"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| DashboardMergeRequest {
                project_id: row.get("project_id"),
                project_label: row.get("project_label"),
                mr_iid: row.get("mr_iid"),
                total_runs: row.get("total_runs"),
                running_runs: row.get::<Option<i64>, _>("running_runs").unwrap_or(0),
                failed_runs: row.get::<Option<i64>, _>("failed_runs").unwrap_or(0),
                total_findings: row.get("total_findings"),
                total_comments: row.get("total_comments"),
                last_commit_sha: row.get("last_commit_sha"),
                last_status: row.get("last_status"),
                last_review_at: row.get("last_review_at"),
            })
            .collect())
    }

    pub async fn findings_list(
        &self,
        params: &DashboardListParams,
    ) -> AppResult<Vec<DashboardFinding>> {
        let mut builder: QueryBuilder<'_, Sqlite> = QueryBuilder::new(
            r#"
select
    finding.review_run_id, request.project_id,
    coalesce(nullif(request.project_path_with_namespace, ''), nullif(request.project_name, ''), '#' || request.project_id) as project_label,
    request.mr_iid, request.commit_sha,
    finding.task_type, finding.task_id, finding.rule_id, finding.severity, finding.path,
    finding.new_line, finding.title, finding.message, finding.created_at
from review_findings finding
join review_requests request on request.review_run_id = finding.review_run_id
where 1 = 1
"#,
        );
        push_dashboard_list_filters(&mut builder, params);
        builder.push(" order by finding.created_at desc, finding.id desc limit ");
        builder.push_bind(params.limit.unwrap_or(100).clamp(1, 500));
        builder.push(" offset ");
        builder.push_bind(params.offset.unwrap_or(0).max(0));
        let rows = builder.build().fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(row_to_finding).collect())
    }

    pub async fn comments_list(
        &self,
        params: &DashboardListParams,
    ) -> AppResult<Vec<DashboardComment>> {
        let mut builder: QueryBuilder<'_, Sqlite> = QueryBuilder::new(
            r#"
select
    comment.review_run_id, request.project_id,
    coalesce(nullif(request.project_path_with_namespace, ''), nullif(request.project_name, ''), '#' || request.project_id) as project_label,
    request.mr_iid, request.commit_sha,
    comment.rule_id, comment.path, comment.new_line, comment.discussion_id, comment.note_id,
    comment.publish_position,
    comment.created_at
from review_comment_records comment
join review_requests request on request.review_run_id = comment.review_run_id
where 1 = 1
"#,
        );
        push_dashboard_list_filters(&mut builder, params);
        builder.push(" order by comment.created_at desc, comment.id desc limit ");
        builder.push_bind(params.limit.unwrap_or(100).clamp(1, 500));
        builder.push(" offset ");
        builder.push_bind(params.offset.unwrap_or(0).max(0));
        let rows = builder.build().fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(row_to_comment).collect())
    }

    async fn tasks(&self, review_run_id: &str) -> AppResult<Vec<DashboardTaskRun>> {
        let rows = sqlx::query(
            r#"
select task_type, task_id, title, status, findings, comments, error_code, error, started_at, finished_at,
    coverage_total_files, coverage_fully_reviewed_files, coverage_partially_reviewed_files,
    coverage_unreviewed_files, coverage_total_diff_bytes, coverage_reviewed_diff_bytes,
    coverage_required_batches, coverage_planned_batches, coverage_completed_batches,
    coverage_max_batches, tool_calls_used, max_tool_calls, coverage_complete
from review_task_runs
where review_run_id = ?
order by started_at asc, id asc
"#,
        )
        .bind(review_run_id)
        .fetch_all(&self.pool)
        .await?;
        let mut tasks = Vec::new();
        for row in rows {
            let task_type: String = row.get("task_type");
            let task_id: String = row.get("task_id");
            let file_rows = sqlx::query(
                r#"select path, status, reason, total_diff_bytes, reviewed_diff_bytes
from review_coverage_files where review_run_id = ? and task_type = ? and task_id = ? order by id"#,
            )
            .bind(review_run_id)
            .bind(&task_type)
            .bind(&task_id)
            .fetch_all(&self.pool)
            .await?;
            let incomplete_files = file_rows
                .into_iter()
                .map(|file| DashboardCoverageFile {
                    path: file.get("path"),
                    status: file.get("status"),
                    reason: file.get("reason"),
                    total_diff_bytes: file.get("total_diff_bytes"),
                    reviewed_diff_bytes: file.get("reviewed_diff_bytes"),
                })
                .collect();
            tasks.push(DashboardTaskRun {
                task_type,
                task_id,
                title: row.get("title"),
                status: row.get("status"),
                findings: row.get("findings"),
                comments: row.get("comments"),
                error_code: row.get("error_code"),
                error: error_preview(row.get("error")),
                started_at: row.get("started_at"),
                finished_at: row.get("finished_at"),
                coverage_total_files: row.get("coverage_total_files"),
                coverage_fully_reviewed_files: row.get("coverage_fully_reviewed_files"),
                coverage_partially_reviewed_files: row.get("coverage_partially_reviewed_files"),
                coverage_unreviewed_files: row.get("coverage_unreviewed_files"),
                coverage_total_diff_bytes: row.get("coverage_total_diff_bytes"),
                coverage_reviewed_diff_bytes: row.get("coverage_reviewed_diff_bytes"),
                coverage_required_batches: row.get("coverage_required_batches"),
                coverage_planned_batches: row.get("coverage_planned_batches"),
                coverage_completed_batches: row.get("coverage_completed_batches"),
                coverage_max_batches: row.get("coverage_max_batches"),
                tool_calls_used: row.get("tool_calls_used"),
                max_tool_calls: row.get("max_tool_calls"),
                coverage_complete: row.get("coverage_complete"),
                incomplete_files,
            });
        }
        Ok(tasks)
    }

    async fn findings(&self, review_run_id: &str) -> AppResult<Vec<DashboardFinding>> {
        let rows = sqlx::query(
            r#"
select
    finding.review_run_id, request.project_id,
    coalesce(nullif(request.project_path_with_namespace, ''), nullif(request.project_name, ''), '#' || request.project_id) as project_label,
    request.mr_iid, request.commit_sha,
    finding.task_type, finding.task_id, finding.rule_id, finding.severity, finding.path,
    finding.new_line, finding.title, finding.message, finding.created_at
from review_findings finding
join review_requests request on request.review_run_id = finding.review_run_id
where finding.review_run_id = ?
order by finding.id asc
limit 500
"#,
        )
        .bind(review_run_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_finding).collect())
    }

    async fn comments(&self, review_run_id: &str) -> AppResult<Vec<DashboardComment>> {
        let rows = sqlx::query(
            r#"
select
    comment.review_run_id, request.project_id,
    coalesce(nullif(request.project_path_with_namespace, ''), nullif(request.project_name, ''), '#' || request.project_id) as project_label,
    request.mr_iid, request.commit_sha,
    comment.rule_id, comment.path, comment.new_line, comment.discussion_id, comment.note_id,
    comment.publish_position,
    comment.created_at
from review_comment_records comment
join review_requests request on request.review_run_id = comment.review_run_id
where comment.review_run_id = ?
order by comment.id asc
limit 500
"#,
        )
        .bind(review_run_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_comment).collect())
    }
}

const ERROR_PREVIEW_MAX_BYTES: usize = 4 * 1024;

fn dashboard_failure(code: Option<String>, message: Option<String>) -> Option<DashboardFailure> {
    if code.is_none() && message.is_none() {
        return None;
    }
    Some(DashboardFailure {
        code,
        message: error_preview(message).unwrap_or_default(),
    })
}

fn dashboard_task_failure(task: &DashboardTaskRun) -> Option<DashboardFailure> {
    if task.status == "completed" {
        return None;
    }
    dashboard_failure(task.error_code.clone(), task.error.clone())
}

fn error_preview(value: Option<String>) -> Option<String> {
    value.map(|value| truncate_utf8_bytes(value, ERROR_PREVIEW_MAX_BYTES))
}

fn truncate_utf8_bytes(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

fn push_run_filters(builder: &mut QueryBuilder<'_, Sqlite>, params: &RunListParams) {
    if let Some(status) = params.status.as_deref().filter(|status| !status.is_empty()) {
        builder.push(" and ");
        push_effective_status_expr(builder, "rr");
        builder.push(" = ");
        builder.push_bind(status.to_owned());
    }
    if let Some(project_id) = params.project_id {
        builder.push(" and project_id = ");
        builder.push_bind(project_id);
    }
    if let Some(project) = params
        .project
        .as_deref()
        .filter(|project| !project.is_empty())
    {
        push_project_filter(builder, "rr", project);
    }
    if let Some(mr_iid) = params.mr_iid {
        builder.push(" and mr_iid = ");
        builder.push_bind(mr_iid);
    }
}

fn push_effective_status_expr(builder: &mut QueryBuilder<'_, Sqlite>, table_alias: &str) {
    builder.push("case when ");
    builder.push(table_alias);
    builder.push(".status = 'running' then 'running' when ");
    builder.push(table_alias);
    builder.push(".status = 'completed' and not exists (select 1 from review_task_runs task where task.review_run_id = ");
    builder.push(table_alias);
    builder
        .push(".review_run_id and task.status <> 'completed') then 'completed' else 'failed' end");
}

fn push_dashboard_list_filters(
    builder: &mut QueryBuilder<'_, Sqlite>,
    params: &DashboardListParams,
) {
    if let Some(project_id) = params.project_id {
        builder.push(" and request.project_id = ");
        builder.push_bind(project_id);
    }
    if let Some(project) = params
        .project
        .as_deref()
        .filter(|project| !project.is_empty())
    {
        push_project_filter(builder, "request", project);
    }
    if let Some(mr_iid) = params.mr_iid {
        builder.push(" and request.mr_iid = ");
        builder.push_bind(mr_iid);
    }
}

fn push_project_filter(builder: &mut QueryBuilder<'_, Sqlite>, table_alias: &str, project: &str) {
    let project_id_text = project.strip_prefix('#').unwrap_or(project);
    if let Ok(project_id) = project_id_text.parse::<i64>() {
        builder.push(" and ");
        builder.push(table_alias);
        builder.push(".project_id = ");
        builder.push_bind(project_id);
        return;
    }

    let pattern = format!("%{}%", project.to_lowercase());
    builder.push(" and (lower(coalesce(");
    builder.push(table_alias);
    builder.push(".project_path_with_namespace, '')) like ");
    builder.push_bind(pattern.clone());
    builder.push(" or lower(coalesce(");
    builder.push(table_alias);
    builder.push(".project_name, '')) like ");
    builder.push_bind(pattern);
    builder.push(")");
}

fn row_to_run(row: SqliteRow) -> DashboardRun {
    DashboardRun {
        review_run_id: row.get("review_run_id"),
        trigger_type: row.get("trigger_type"),
        project_id: row.get("project_id"),
        project_label: row.get("project_label"),
        mr_iid: row.get("mr_iid"),
        commit_sha: row.get("commit_sha"),
        note_id: row.get("note_id"),
        requested_ids_json: row.get("requested_ids_json"),
        selected_ai_reviews: row.get("selected_ai_reviews"),
        selected_script_tasks: row.get("selected_script_tasks"),
        status: row.get("status"),
        findings: row.get("findings"),
        comments: row.get("comments"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        duration_ms: row.get("duration_ms"),
        total_task_runs: row.get("total_task_runs"),
        completed_task_runs: row.get("completed_task_runs"),
    }
}

fn row_to_finding(row: SqliteRow) -> DashboardFinding {
    DashboardFinding {
        review_run_id: row.get("review_run_id"),
        project_id: row.get("project_id"),
        project_label: row.get("project_label"),
        mr_iid: row.get("mr_iid"),
        commit_sha: row.get("commit_sha"),
        task_type: row.get("task_type"),
        task_id: row.get("task_id"),
        rule_id: row.get("rule_id"),
        severity: row.get("severity"),
        path: row.get("path"),
        new_line: row.get("new_line"),
        title: row.get("title"),
        message: row.get("message"),
        created_at: row.get("created_at"),
    }
}

fn row_to_comment(row: SqliteRow) -> DashboardComment {
    DashboardComment {
        review_run_id: row.get("review_run_id"),
        project_id: row.get("project_id"),
        project_label: row.get("project_label"),
        mr_iid: row.get("mr_iid"),
        commit_sha: row.get("commit_sha"),
        rule_id: row.get("rule_id"),
        path: row.get("path"),
        new_line: row.get("new_line"),
        discussion_id: row.get("discussion_id"),
        note_id: row.get("note_id"),
        publish_position: row.get("publish_position"),
        created_at: row.get("created_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_preview_is_utf8_safe_and_limited_to_four_kibibytes() {
        let preview = error_preview(Some("錯".repeat(2_000))).unwrap();

        assert!(preview.len() <= ERROR_PREVIEW_MAX_BYTES);
        assert!(preview.is_char_boundary(preview.len()));
    }

    #[test]
    fn dashboard_failure_keeps_legacy_message_without_code() {
        let failure = dashboard_failure(None, Some("legacy failure".into())).unwrap();

        assert_eq!(failure.code, None);
        assert_eq!(failure.message, "legacy failure");
    }
}
