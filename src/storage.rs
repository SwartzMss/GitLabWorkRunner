use crate::error::AppResult;
use chrono::Utc;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::str::FromStr;

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
        let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
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
