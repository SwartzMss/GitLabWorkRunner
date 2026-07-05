use crate::error::AppResult;
use chrono::Utc;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::str::FromStr;
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
    pub mr_iid: i64,
    pub commit_sha: &'a str,
    pub note_id: Option<i64>,
    pub requested_ids_json: &'a str,
    pub selected_ai_reviews: usize,
    pub selected_script_tasks: usize,
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
    pub error: Option<&'a str>,
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
}

impl StateStore {
    pub async fn connect(database_url: &str) -> AppResult<Self> {
        let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
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
    started_at text not null,
    finished_at text,
    unique(review_run_id, task_type, task_id)
);
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
    created_at text not null
);
"#,
        )
        .execute(&self.pool)
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

    pub async fn start_review_request(&self, request: &ReviewRequestStart<'_>) -> AppResult<()> {
        let now = now_rfc3339();
        sqlx::query(
            r#"
insert into review_requests
(review_run_id, trigger_type, project_id, mr_iid, commit_sha, note_id, requested_ids_json,
 selected_ai_reviews, selected_script_tasks, status, findings, comments, timezone, started_at)
values (?, ?, ?, ?, ?, ?, ?, ?, ?, 'running', 0, 0, ?, ?)
on conflict(review_run_id) do update set
    status = 'running',
    finished_at = null
"#,
        )
        .bind(request.review_run_id)
        .bind(request.trigger_type)
        .bind(request.project_id)
        .bind(request.mr_iid)
        .bind(request.commit_sha)
        .bind(request.note_id)
        .bind(request.requested_ids_json)
        .bind(request.selected_ai_reviews as i64)
        .bind(request.selected_script_tasks as i64)
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
        let now = now_rfc3339();
        sqlx::query(
            r#"
update review_requests
set status = ?, findings = ?, comments = ?, finished_at = ?
where review_run_id = ?
"#,
        )
        .bind(status)
        .bind(findings as i64)
        .bind(comments as i64)
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
    error = null,
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
set status = ?, findings = ?, comments = ?, error = ?, finished_at = ?
where review_run_id = ? and task_type = ? and task_id = ?
"#,
        )
        .bind(task.status)
        .bind(task.findings as i64)
        .bind(task.comments as i64)
        .bind(task.error)
        .bind(&now)
        .bind(task.review_run_id)
        .bind(task.task_type)
        .bind(task.task_id)
        .execute(&self.pool)
        .await?;
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
(review_run_id, project_id, mr_iid, commit_sha, rule_id, path, new_line, discussion_id, note_id, created_at)
values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            "review comment state recorded"
        );
        Ok(())
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_review_requests() {
        let store = StateStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        store
            .start_review_request(&ReviewRequestStart {
                review_run_id: "rr-1",
                trigger_type: "manual_note",
                project_id: 1,
                mr_iid: 2,
                commit_sha: "abc",
                note_id: Some(9),
                requested_ids_json: r#"["ai-review"]"#,
                selected_ai_reviews: 1,
                selected_script_tasks: 0,
            })
            .await
            .unwrap();
        store
            .finish_review_request("rr-1", "completed", 3, 2)
            .await
            .unwrap();
        let count: i64 =
            sqlx::query_scalar("select count(*) from review_requests where review_run_id = ?")
                .bind("rr-1")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }
}
