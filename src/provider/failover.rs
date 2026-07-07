//! Shared runtime state driving per-alias backend failover.
//!
//! A [`FailoverRegistry`] holds one [`FailoverState`] per model alias that has more than one
//! backend target. Request handlers report failures (429 / 5xx / transport timeouts) through the
//! registry; when the configured threshold is reached within the sliding window — and the minimum
//! switch interval has elapsed — the alias advances to the next target. Switching is one-way: the
//! active index never moves back toward the preferred target.

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::{
    config::{Config, FailoverPolicy},
    error::ProxyError,
    provider::backend_request::{is_retryable_status, is_retryable_transport_error},
};

/// Reports whether an error should count toward failover (429 / 5xx / transport timeout).
pub fn is_failover_countable(error: &ProxyError) -> bool {
    match error {
        ProxyError::UpstreamStatus { status, .. } => is_retryable_status(*status),
        ProxyError::UpstreamHttp(err) => is_retryable_transport_error(err),
        _ => false,
    }
}

/// Per-alias failover state guarding the currently active target index.
#[derive(Debug)]
pub struct FailoverState {
    active_index: usize,
    target_count: usize,
    policy: FailoverPolicy,
    failures: VecDeque<Instant>,
    last_switch: Option<Instant>,
}

impl FailoverState {
    /// Creates state for an alias with `target_count` ordered targets.
    pub fn new(target_count: usize, policy: FailoverPolicy) -> Self {
        Self {
            active_index: 0,
            target_count,
            policy,
            failures: VecDeque::new(),
            last_switch: None,
        }
    }

    /// Returns the index of the currently active target.
    pub fn current_index(&self) -> usize {
        self.active_index
    }

    /// Records a failure at `now` and switches to the next target when the policy is satisfied.
    ///
    /// Returns `Some(new_index)` when a switch occurred, `None` otherwise.
    pub fn record_failure(&mut self, now: Instant) -> Option<usize> {
        // Drop timestamps that have aged out of the sliding window.
        let window = Duration::from_millis(self.policy.window_ms);
        while let Some(oldest) = self.failures.front() {
            if now.duration_since(*oldest) > window {
                self.failures.pop_front();
            } else {
                break;
            }
        }
        self.failures.push_back(now);

        if self.active_index + 1 >= self.target_count {
            // Already on the last target; nowhere left to advance.
            return None;
        }
        if (self.failures.len() as u32) < self.policy.failure_threshold {
            return None;
        }
        if let Some(last_switch) = self.last_switch {
            let min_interval = Duration::from_millis(self.policy.min_switch_interval_ms);
            if now.duration_since(last_switch) < min_interval {
                // Threshold met but the minimum switch interval has not elapsed.
                return None;
            }
        }

        self.active_index += 1;
        self.last_switch = Some(now);
        self.failures.clear();
        Some(self.active_index)
    }
}

/// Registry of per-alias failover state shared across requests.
#[derive(Debug, Default)]
pub struct FailoverRegistry {
    states: HashMap<String, Mutex<FailoverState>>,
}

impl FailoverRegistry {
    /// Builds a registry, creating state only for aliases with more than one target.
    pub fn from_config(config: &Config) -> Self {
        let mut states = HashMap::new();
        for (alias, model_alias) in &config.model_aliases {
            let targets = model_alias.targets();
            if targets.len() > 1 {
                states.insert(
                    alias.clone(),
                    Mutex::new(FailoverState::new(
                        targets.len(),
                        model_alias.failover_policy(),
                    )),
                );
            }
        }
        Self { states }
    }

    /// Returns the currently active target index for `alias` (0 when no failover state exists).
    pub fn current_index(&self, alias: &str) -> usize {
        match self.states.get(alias) {
            Some(state) => state
                .lock()
                .expect("failover state mutex poisoned")
                .current_index(),
            None => 0,
        }
    }

    /// Records a failure for `alias`, returning the new index if a switch occurred.
    pub fn record_failure(&self, alias: &str, now: Instant) -> Option<usize> {
        let state = self.states.get(alias)?;
        state
            .lock()
            .expect("failover state mutex poisoned")
            .record_failure(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(
        window_ms: u64,
        failure_threshold: u32,
        min_switch_interval_ms: u64,
    ) -> FailoverPolicy {
        FailoverPolicy {
            window_ms,
            failure_threshold,
            min_switch_interval_ms,
        }
    }

    #[test]
    fn switches_after_threshold_reached_within_window() {
        let mut state = FailoverState::new(2, policy(60_000, 3, 30_000));
        let start = Instant::now();

        assert_eq!(state.record_failure(start), None);
        assert_eq!(state.current_index(), 0);
        assert_eq!(
            state.record_failure(start + Duration::from_millis(10)),
            None
        );
        assert_eq!(
            state.record_failure(start + Duration::from_millis(20)),
            Some(1)
        );
        assert_eq!(state.current_index(), 1);
    }

    #[test]
    fn does_not_switch_before_min_interval_even_when_threshold_met() {
        let mut state = FailoverState::new(3, policy(60_000, 2, 30_000));
        let start = Instant::now();

        // First switch: 0 -> 1 at t=0.
        assert_eq!(state.record_failure(start), None);
        assert_eq!(state.record_failure(start), Some(1));

        // Threshold met again quickly, but min interval (30s) has not elapsed: stays put.
        let soon = start + Duration::from_millis(5_000);
        assert_eq!(state.record_failure(soon), None);
        assert_eq!(state.record_failure(soon), None);
        assert_eq!(state.current_index(), 1);

        // Once the interval elapses, the accumulated failures already satisfy the threshold, so the
        // next failure switches immediately.
        let later = start + Duration::from_millis(31_000);
        assert_eq!(state.record_failure(later), Some(2));
        assert_eq!(state.current_index(), 2);
    }

    #[test]
    fn expired_failures_do_not_count_toward_threshold() {
        let mut state = FailoverState::new(2, policy(1_000, 3, 1));
        let start = Instant::now();

        assert_eq!(state.record_failure(start), None);
        assert_eq!(
            state.record_failure(start + Duration::from_millis(100)),
            None
        );
        // This failure is 2s after the first, which has aged out of the 1s window.
        assert_eq!(
            state.record_failure(start + Duration::from_millis(2_000)),
            None
        );
        assert_eq!(state.current_index(), 0);
    }

    #[test]
    fn does_not_advance_past_last_target() {
        let mut state = FailoverState::new(2, policy(60_000, 1, 1));
        let start = Instant::now();

        assert_eq!(state.record_failure(start), Some(1));
        // Already at the last target; further failures never advance.
        assert_eq!(
            state.record_failure(start + Duration::from_millis(10)),
            None
        );
        assert_eq!(
            state.record_failure(start + Duration::from_millis(20)),
            None
        );
        assert_eq!(state.current_index(), 1);
    }

    #[test]
    fn registry_returns_zero_for_unknown_alias() {
        let registry = FailoverRegistry::default();
        assert_eq!(registry.current_index("nope"), 0);
        assert_eq!(registry.record_failure("nope", Instant::now()), None);
    }
}
