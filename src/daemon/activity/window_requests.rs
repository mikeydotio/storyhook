//! Coalesced view demand and per-project retry cadence, independent of verifier slots.

use crate::store::ProjectId;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Condvar, Mutex, PoisonError},
    time::{Duration, Instant},
};

/// A phase requests work without waiting for the view worker or tmux.
#[derive(Clone, Default)]
pub(crate) struct Requests(Arc<(Mutex<BTreeSet<ProjectId>>, Condvar)>);

impl Requests {
    /// Coalesce repeated demand until the worker takes it.
    pub(crate) fn request(&self, project: ProjectId) {
        self.0
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(project);
        self.0.1.notify_one();
    }

    /// Drain before processing so requests during a reconcile remain pending.
    pub(crate) fn take(&self) -> BTreeSet<ProjectId> {
        std::mem::take(&mut *self.0.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Wait only while no request is queued; a raced notification cannot be lost.
    pub(super) fn wait(&self, duration: Duration) {
        let pending = self.0.0.lock().unwrap_or_else(PoisonError::into_inner);
        drop(
            self.0
                .1
                .wait_timeout_while(pending, duration, |set| set.is_empty())
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// Worker-owned schedule. Requests and periodic repairs share one cooldown.
#[derive(Default)]
pub(super) struct Schedule {
    pending: BTreeSet<ProjectId>,
    completed: BTreeMap<ProjectId, Instant>,
}

impl Schedule {
    /// Retain requests until they become eligible, even if another pass is running.
    pub(super) fn request(&mut self, projects: impl IntoIterator<Item = ProjectId>) {
        self.pending.extend(projects);
    }

    /// Take eligible work once; completion establishes its next retry boundary.
    pub(super) fn due(&mut self, now: Instant, interval: Duration) -> BTreeSet<ProjectId> {
        let due = self
            .pending
            .iter()
            .copied()
            .filter(|id| {
                self.completed
                    .get(id)
                    .is_none_or(|completed| now.duration_since(*completed) >= interval)
            })
            .collect::<BTreeSet<_>>();
        self.pending.retain(|id| !due.contains(id));
        due
    }

    /// Measure cooldown from completion so failures cannot immediately loop.
    pub(super) fn completed(&mut self, project: ProjectId, now: Instant) {
        self.completed.insert(project, now);
    }

    /// Deleted or invalid registrations cannot retain pending work or cooldowns.
    pub(super) fn retain(&mut self, projects: &BTreeSet<ProjectId>) {
        self.pending.retain(|id| projects.contains(id));
        self.completed.retain(|id, _| projects.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_during_work_are_not_consumed_with_the_previous_batch() {
        let requests = Requests::default();
        let project = ProjectId::new(1);
        requests.request(project);
        requests.request(project);
        assert_eq!(requests.take(), BTreeSet::from([project]));
        requests.clone().request(project);
        assert_eq!(requests.take(), BTreeSet::from([project]));
        assert!(requests.take().is_empty());
    }

    #[test]
    fn cooldown_retains_requests_and_does_not_starve_other_projects() {
        let mut schedule = Schedule::default();
        let one = ProjectId::new(1);
        let two = ProjectId::new(2);
        let now = Instant::now();
        let interval = super::super::window::RECONCILE_INTERVAL;
        schedule.completed(one, now);
        schedule.request([one, two]);
        assert_eq!(schedule.due(now, interval), BTreeSet::from([two]));
        assert!(schedule.due(now + interval / 2, interval).is_empty());
        assert_eq!(
            schedule.due(now + interval, interval),
            BTreeSet::from([one])
        );
        schedule.request([one]);
        schedule.retain(&BTreeSet::new());
        assert!(schedule.due(now + interval * 2, interval).is_empty());
    }
}
