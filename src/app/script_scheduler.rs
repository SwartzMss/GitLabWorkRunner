use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub(crate) struct ScriptTaskScheduler {
    active: Mutex<HashMap<String, usize>>,
}

pub(crate) struct ScriptTaskPermit {
    scheduler: Arc<ScriptTaskScheduler>,
    task_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScriptTaskBusy {
    pub active_count: usize,
    pub max_concurrent: usize,
}

impl ScriptTaskScheduler {
    pub(crate) fn try_acquire(
        self: &Arc<Self>,
        task_id: &str,
        max_concurrent: usize,
    ) -> Result<ScriptTaskPermit, ScriptTaskBusy> {
        let max_concurrent = max_concurrent.max(1);
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let active_count = active.get(task_id).copied().unwrap_or_default();
        if active_count >= max_concurrent {
            return Err(ScriptTaskBusy {
                active_count,
                max_concurrent,
            });
        }
        active.insert(task_id.to_owned(), active_count + 1);
        Ok(ScriptTaskPermit {
            scheduler: Arc::clone(self),
            task_id: task_id.to_owned(),
        })
    }
}

impl Drop for ScriptTaskPermit {
    fn drop(&mut self) {
        let mut active = self
            .scheduler
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = active.get_mut(&self.task_id) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.task_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_script_has_its_own_non_waiting_concurrency_limit() {
        let scheduler = Arc::new(ScriptTaskScheduler::default());
        let first = scheduler.try_acquire("lint", 1).unwrap();
        assert_eq!(
            scheduler.try_acquire("lint", 1).err().unwrap().active_count,
            1
        );
        let second_script = scheduler.try_acquire("security", 1).unwrap();
        drop(first);
        assert!(scheduler.try_acquire("lint", 1).is_ok());
        drop(second_script);
    }
}
