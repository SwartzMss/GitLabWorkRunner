use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tracing::info;

#[derive(Default)]
pub(crate) struct ActiveReviews {
    running: Mutex<HashMap<ActiveReviewKey, String>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ActiveReviewKey {
    pub(crate) project_id: i64,
    pub(crate) mr_iid: i64,
    pub(crate) commit_sha: String,
}

pub(crate) struct ActiveReviewStart {
    pub(crate) guard: ActiveReviewGuard,
    pub(crate) active_count: usize,
}

pub(crate) enum ActiveReviewStartError {
    Duplicate {
        active_review_run_id: String,
        active_count: usize,
    },
    QueueBusy {
        active_count: usize,
        max_concurrent_reviews: usize,
    },
}

pub(crate) struct ActiveReviewGuard {
    active_reviews: Arc<ActiveReviews>,
    key: ActiveReviewKey,
    review_run_id: String,
}

impl ActiveReviews {
    pub(crate) fn try_start(
        self: &Arc<Self>,
        key: ActiveReviewKey,
        review_run_id: String,
        max_concurrent_reviews: usize,
    ) -> Result<ActiveReviewStart, ActiveReviewStartError> {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(active_review_run_id) = running.get(&key) {
            return Err(ActiveReviewStartError::Duplicate {
                active_review_run_id: active_review_run_id.clone(),
                active_count: running.len(),
            });
        }
        let max_concurrent_reviews = max_concurrent_reviews.max(1);
        let active_count = running.len();
        if active_count >= max_concurrent_reviews {
            return Err(ActiveReviewStartError::QueueBusy {
                active_count,
                max_concurrent_reviews,
            });
        }
        running.insert(key.clone(), review_run_id.clone());
        Ok(ActiveReviewStart {
            active_count: running.len(),
            guard: ActiveReviewGuard {
                active_reviews: Arc::clone(self),
                key,
                review_run_id,
            },
        })
    }

    fn finish(&self, key: &ActiveReviewKey, review_run_id: &str) -> bool {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if running
            .get(key)
            .is_some_and(|active_review_run_id| active_review_run_id == review_run_id)
        {
            running.remove(key);
            true
        } else {
            false
        }
    }
}

impl Drop for ActiveReviewGuard {
    fn drop(&mut self) {
        let removed = self.active_reviews.finish(&self.key, &self.review_run_id);
        info!(
            review_run_id = %self.review_run_id,
            project_id = self.key.project_id,
            mr_iid = self.key.mr_iid,
            commit_sha = %self.key.commit_sha,
            removed,
            "review run removed from active registry"
        );
    }
}
