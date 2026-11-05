//! A cancellable sequence of attempts. Resource owners decide when launching is
//! allowed and what observation proves success; this module owns retry timing,
//! budgets and retirement of asynchronous replies. It never runs platform work.

use crate::types::{Generation, MonoMs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId {
    pub generation: Generation,
    pub serial: u64,
}

/// Shared by tasks in one owner; cancellation must not rewind this sequence.
#[derive(Default)]
pub struct Requests(u64);

#[derive(Clone, Copy, Debug)]
pub enum Limit {
    Attempts(u32),
    /// Required cleanup remains owned until it succeeds or the resource is gone.
    UntilCancelled,
    /// Measured from the first failed reply, checked on each subsequent failure.
    FailureWindow(u64),
}

#[derive(Clone, Copy, Debug)]
pub enum Delay {
    Fixed(u64),
    /// One delay per failed attempt; the final entry repeats.
    Staged(&'static [u64]),
}

impl Delay {
    fn after(self, attempts: u32) -> u64 {
        match self {
            Self::Fixed(ms) => ms,
            Self::Staged(delays) => delays
                .get(attempts.saturating_sub(1) as usize)
                .or_else(|| delays.last())
                .copied()
                .unwrap_or(0),
        }
    }
}

/// Resource owners classify failure; the task applies the budget and wait.
pub enum Failure {
    Retry,
    /// The resource was busy: retire this request but repeat the same attempt.
    Defer(u64),
    Stop,
}

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub delay: Delay,
    pub limit: Limit,
    /// None when the platform already reports bounded call timeouts.
    pub request_timeout_ms: Option<u64>,
    /// Some when a successful reply still needs a resource observation.
    pub confirmation_timeout_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Running { id: RequestId, until: Option<MonoMs> },
    Confirming { id: RequestId, until: MonoMs },
    Backoff { until: MonoMs },
    Deferred { until: MonoMs },
    Succeeded { id: Option<RequestId> },
    Exhausted,
}

pub struct Task {
    policy: Policy,
    state: State,
    attempts: u32,
    first_failure: Option<MonoMs>,
}

impl Task {
    pub fn new(policy: Policy) -> Self {
        Self { policy, state: State::Idle, attempts: 0, first_failure: None }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn budget(&self) -> (u32, Limit) {
        (self.attempts, self.policy.limit)
    }

    /// Retire pending replies and retry history. The owner's Requests survives.
    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.attempts = 0;
        self.first_failure = None;
    }

    /// Expire work even while a resource prerequisite prevents another launch.
    pub fn tick(&mut self, now: MonoMs) {
        let until = match self.state {
            State::Running { until, .. } => until,
            State::Confirming { until, .. } => Some(until),
            _ => None,
        };
        if until.is_some_and(|until| now >= until) {
            self.failed(now);
        }
    }

    /// Called at the resource's launch point. Repeated calls cannot duplicate
    /// an attempt or revive an exhausted task.
    pub fn launch(
        &mut self,
        requests: &mut Requests,
        generation: Generation,
        now: MonoMs,
    ) -> Option<RequestId> {
        self.try_launch(requests, generation, now, |_| true)
    }

    /// Admit an attempt only if the resource can start it. A rejected admission
    /// consumes no budget and keeps the previous state. IDs are never reused.
    pub fn try_launch(
        &mut self,
        requests: &mut Requests,
        generation: Generation,
        now: MonoMs,
        admit: impl FnOnce(RequestId) -> bool,
    ) -> Option<RequestId> {
        self.tick(now);
        let repeat = match self.state {
            State::Idle => false,
            State::Backoff { until } if now >= until => false,
            State::Deferred { until } if now >= until => true,
            _ => return None,
        };
        if !repeat && matches!(self.policy.limit, Limit::Attempts(limit) if self.attempts >= limit)
        {
            self.state = State::Exhausted;
            return None;
        }
        requests.0 += 1;
        let id = RequestId { generation, serial: requests.0 };
        if !admit(id) {
            return None;
        }
        if !repeat {
            self.attempts = self.attempts.saturating_add(1);
        }
        self.state = State::Running {
            id,
            until: self.policy.request_timeout_ms.map(|ms| now.saturating_add(ms)),
        };
        Some(id)
    }

    /// Only the current, unexpired request can acknowledge or fail an attempt.
    pub fn reply(&mut self, id: RequestId, ok: bool, now: MonoMs) -> bool {
        if !ok {
            return self.fail(id, Failure::Retry, now);
        }
        self.tick(now);
        if !matches!(self.state, State::Running { id: current, .. } if current == id) {
            return false;
        }
        self.state = match self.policy.confirmation_timeout_ms {
            Some(ms) => State::Confirming { id, until: now.saturating_add(ms) },
            None => State::Succeeded { id: Some(id) },
        };
        true
    }

    pub fn fail(&mut self, id: RequestId, failure: Failure, now: MonoMs) -> bool {
        self.tick(now);
        if !matches!(self.state, State::Running { id: current, .. } if current == id) {
            return false;
        }
        match failure {
            Failure::Retry => self.failed(now),
            Failure::Defer(ms) => self.state = State::Deferred { until: now.saturating_add(ms) },
            Failure::Stop => self.state = State::Exhausted,
        };
        true
    }

    /// The owner observed the current target actually applied. This can arrive
    /// before the reply, or after timeout; it does not launch any work.
    pub fn observed_success(&mut self) {
        let id = match self.state {
            State::Running { id, .. } | State::Confirming { id, .. } => Some(id),
            State::Succeeded { id } => id,
            _ => None,
        };
        self.state = State::Succeeded { id };
    }

    fn failed(&mut self, now: MonoMs) {
        let first = *self.first_failure.get_or_insert(now);
        let exhausted = match self.policy.limit {
            Limit::UntilCancelled => false,
            Limit::Attempts(limit) => self.attempts >= limit,
            Limit::FailureWindow(ms) => now.saturating_since(first) >= ms,
        };
        self.state = if exhausted {
            State::Exhausted
        } else {
            State::Backoff { until: now.saturating_add(self.policy.delay.after(self.attempts)) }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confirmed_policy() -> Policy {
        Policy {
            delay: Delay::Fixed(1_000),
            limit: Limit::Attempts(3),
            request_timeout_ms: Some(5_000),
            confirmation_timeout_ms: Some(5_000),
        }
    }

    #[test]
    fn busy_waits_reuse_the_budget_and_do_not_advance_staged_backoff() {
        let mut task = Task::new(Policy {
            delay: Delay::Staged(&[10, 20]),
            limit: Limit::Attempts(3),
            request_timeout_ms: None,
            confirmation_timeout_ms: None,
        });
        let mut ids = Requests::default();
        let mut now = MonoMs(0);
        for (attempt, delay) in [(1, 10), (2, 20), (3, 20)] {
            let mut id = task.launch(&mut ids, Generation(attempt), now).unwrap();
            assert_eq!(task.budget().0, attempt as u32);
            // Busy even on the final attempt must not exhaust the task.
            for _ in 0..2 {
                assert!(task.fail(id, Failure::Defer(50), now));
                assert!(!task.reply(id, true, now));
                assert!(task
                    .launch(&mut ids, Generation(attempt), now.saturating_add(49))
                    .is_none());
                now = now.saturating_add(50);
                let next = task.launch(&mut ids, Generation(attempt), now).unwrap();
                assert_ne!(next, id);
                assert!(!task.fail(id, Failure::Stop, now));
                assert_eq!(task.budget().0, attempt as u32);
                id = next;
            }
            assert!(task.fail(id, Failure::Retry, now));
            if attempt == 3 {
                assert_eq!(task.state(), State::Exhausted);
            } else {
                now = now.saturating_add(delay);
                assert_eq!(task.state(), State::Backoff { until: now });
            }
        }
        task.reset();
        let id = task.launch(&mut ids, Generation(4), now).unwrap();
        assert!(task.fail(id, Failure::Stop, now));
        assert!(task.launch(&mut ids, Generation(4), now.saturating_add(100)).is_none());
    }

    #[test]
    fn rejected_admission_spends_no_attempt_and_never_reuses_an_id() {
        let mut task = Task::new(Policy {
            delay: Delay::Fixed(10),
            limit: Limit::Attempts(2),
            request_timeout_ms: None,
            confirmation_timeout_ms: None,
        });
        let mut ids = Requests::default();
        let mut rejected = Vec::new();
        for ms in 0..5 {
            assert!(task
                .try_launch(&mut ids, Generation(1), MonoMs(ms), |id| {
                    rejected.push(id);
                    false
                })
                .is_none());
            assert_eq!(task.state(), State::Idle);
        }
        let first = task.launch(&mut ids, Generation(1), MonoMs(5)).unwrap();
        assert!(!rejected.contains(&first));
        task.reply(first, false, MonoMs(5));
        assert!(task
            .try_launch(&mut ids, Generation(1), MonoMs(14), |_| {
                panic!("backoff must prevent resource admission")
            })
            .is_none());
        for ms in 15..20 {
            assert!(task.try_launch(&mut ids, Generation(1), MonoMs(ms), |_| false).is_none());
            assert_eq!(task.state(), State::Backoff { until: MonoMs(15) });
        }
        let second = task.launch(&mut ids, Generation(1), MonoMs(20)).unwrap();
        assert_ne!(first, second);
        task.reply(second, false, MonoMs(20));
        assert_eq!(task.state(), State::Exhausted);
        assert!(task
            .try_launch(&mut ids, Generation(1), MonoMs(30), |_| {
                panic!("exhaustion must prevent resource admission")
            })
            .is_none());
    }

    #[test]
    fn failure_window_starts_at_the_first_failure_and_preserves_the_retry_delay() {
        let mut task = Task::new(Policy {
            delay: Delay::Fixed(2_000),
            limit: Limit::FailureWindow(30_000),
            request_timeout_ms: None,
            confirmation_timeout_ms: None,
        });
        let mut ids = Requests::default();
        let first = task.launch(&mut ids, Generation(1), MonoMs(0)).unwrap();
        assert!(task.launch(&mut ids, Generation(1), MonoMs(50)).is_none());
        assert!(task.reply(first, false, MonoMs(100)));
        assert!(task.launch(&mut ids, Generation(1), MonoMs(2_099)).is_none());
        let second = task.launch(&mut ids, Generation(1), MonoMs(2_100)).unwrap();
        assert!(!task.reply(first, true, MonoMs(2_101)));
        assert!(task.reply(second, false, MonoMs(30_099)));
        let last = task.launch(&mut ids, Generation(1), MonoMs(32_099)).unwrap();
        assert!(task.reply(last, false, MonoMs(32_100)));
        assert_eq!(task.state(), State::Exhausted);
        assert!(task.launch(&mut ids, Generation(1), MonoMs(90_000)).is_none());
    }

    #[test]
    fn callback_and_confirmation_timeouts_share_one_attempt_budget() {
        let mut task = Task::new(confirmed_policy());
        let mut ids = Requests::default();
        let first = task.launch(&mut ids, Generation(1), MonoMs(0)).unwrap();
        assert!(task.reply(first, true, MonoMs(100)));
        assert!(
            !task.reply(first, true, MonoMs(5_000)),
            "duplicate ack cannot extend confirmation"
        );
        task.tick(MonoMs(5_100));
        assert_eq!(task.state(), State::Backoff { until: MonoMs(6_100) });
        let second = task.launch(&mut ids, Generation(1), MonoMs(6_100)).unwrap();
        assert!(!task.reply(second, true, MonoMs(11_100)), "deadline also applies between ticks");
        assert_eq!(task.state(), State::Backoff { until: MonoMs(12_100) });
        let third = task.launch(&mut ids, Generation(1), MonoMs(12_100)).unwrap();
        assert!(task.reply(third, false, MonoMs(12_200)));
        assert_eq!(task.state(), State::Exhausted);
        assert!(task.launch(&mut ids, Generation(1), MonoMs(60_000)).is_none());
        task.observed_success();
        assert!(matches!(task.state(), State::Succeeded { .. }), "actual resource state wins");
        assert!(!task.reply(third, false, MonoMs(60_001)));
    }

    #[test]
    fn cancellation_and_other_tasks_cannot_reuse_or_complete_an_attempt() {
        let mut task = Task::new(confirmed_policy());
        let mut other = Task::new(confirmed_policy());
        let mut ids = Requests::default();
        let old = task.launch(&mut ids, Generation(1), MonoMs(0)).unwrap();
        task.reset();
        let new = task.launch(&mut ids, Generation(1), MonoMs(1)).unwrap();
        let unrelated = other.launch(&mut ids, Generation(1), MonoMs(1)).unwrap();
        assert_ne!(old, new);
        assert_ne!(new, unrelated);
        assert!(!task.reply(old, false, MonoMs(2)));
        assert!(!task.reply(unrelated, true, MonoMs(2)));
        assert!(task.reply(new, true, MonoMs(2)));
        task.observed_success();
        task.tick(MonoMs(60_000));
        assert_eq!(task.state(), State::Succeeded { id: Some(new) });
        assert!(task.launch(&mut ids, Generation(1), MonoMs(60_000)).is_none());
    }
}
