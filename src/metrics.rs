//! Lightweight in-memory request/token counters exposed via `GET /metrics`.
//!
//! Counters are simple monotonic totals kept per backend. They are best-effort observability, not
//! billing-grade accounting: streaming responses count token usage only when the upstream reports
//! it, and requests that fail before resolving a backend are not attributed to any backend.

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::Serialize;

/// Monotonic counters accumulated for a single backend.
#[derive(Debug, Default)]
struct BackendCounters {
    success_requests: AtomicU64,
    failure_requests: AtomicU64,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
}

/// Registry of per-backend counters shared across all request handlers.
#[derive(Debug, Default)]
pub struct MetricsRegistry {
    backends: BTreeMap<String, BackendCounters>,
}

impl MetricsRegistry {
    /// Builds a registry pre-populated with zeroed counters for each known backend name.
    ///
    /// Pre-populating means backends that have not served a request yet still appear in the
    /// snapshot with zero counts.
    pub fn with_backends<I, S>(backend_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let backends = backend_names
            .into_iter()
            .map(|name| (name.into(), BackendCounters::default()))
            .collect();
        Self { backends }
    }

    /// Records a successful request against `backend`, adding any reported token usage.
    pub fn record_success(&self, backend: &str, input_tokens: u32, output_tokens: u32) {
        let Some(counters) = self.backends.get(backend) else {
            return;
        };
        counters.success_requests.fetch_add(1, Ordering::Relaxed);
        counters
            .input_tokens
            .fetch_add(u64::from(input_tokens), Ordering::Relaxed);
        counters
            .output_tokens
            .fetch_add(u64::from(output_tokens), Ordering::Relaxed);
    }

    /// Records a failed request against `backend`.
    pub fn record_failure(&self, backend: &str) {
        let Some(counters) = self.backends.get(backend) else {
            return;
        };
        counters.failure_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns a serializable snapshot of the current per-backend counters.
    pub fn snapshot(&self) -> Vec<BackendMetrics> {
        self.backends
            .iter()
            .map(|(name, counters)| BackendMetrics {
                backend: name.clone(),
                success_requests: counters.success_requests.load(Ordering::Relaxed),
                failure_requests: counters.failure_requests.load(Ordering::Relaxed),
                input_tokens: counters.input_tokens.load(Ordering::Relaxed),
                output_tokens: counters.output_tokens.load(Ordering::Relaxed),
            })
            .collect()
    }
}

/// Serializable per-backend counter snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendMetrics {
    pub backend: String,
    pub success_requests: u64,
    pub failure_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The currently active backend target for a model alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AliasTargetMetrics {
    pub alias: String,
    pub backend: String,
    pub model: String,
}

/// Full `/metrics` response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetricsSnapshot {
    /// Currently active backend target per model alias (reflects any failover switch).
    pub aliases: Vec<AliasTargetMetrics>,
    /// Monotonic per-backend request and token counters.
    pub backends: Vec<BackendMetrics>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_success_and_failure_counts_and_tokens() {
        let registry = MetricsRegistry::with_backends(["deepseek", "anthropic"]);
        registry.record_success("deepseek", 10, 4);
        registry.record_success("deepseek", 5, 2);
        registry.record_failure("deepseek");
        registry.record_failure("anthropic");

        let snapshot = registry.snapshot();
        let deepseek = snapshot.iter().find(|m| m.backend == "deepseek").unwrap();
        assert_eq!(deepseek.success_requests, 2);
        assert_eq!(deepseek.failure_requests, 1);
        assert_eq!(deepseek.input_tokens, 15);
        assert_eq!(deepseek.output_tokens, 6);

        let anthropic = snapshot.iter().find(|m| m.backend == "anthropic").unwrap();
        assert_eq!(anthropic.success_requests, 0);
        assert_eq!(anthropic.failure_requests, 1);
        assert_eq!(anthropic.input_tokens, 0);
    }

    #[test]
    fn ignores_unknown_backend_names() {
        let registry = MetricsRegistry::with_backends(["deepseek"]);
        registry.record_success("unknown", 100, 100);
        registry.record_failure("unknown");

        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].backend, "deepseek");
        assert_eq!(snapshot[0].success_requests, 0);
    }

    #[test]
    fn snapshot_is_sorted_by_backend_name() {
        let registry = MetricsRegistry::with_backends(["zeta", "alpha", "mu"]);
        let names: Vec<_> = registry
            .snapshot()
            .into_iter()
            .map(|m| m.backend)
            .collect();
        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }
}
