//! Claude API budget gate — rate-limit gating and per-tenant budget enforcement.
//!
//! Implements the budget pool design from inference-cluster.md §7.1.
//! This module runs on the Rust runtime side; the authoritative gate also lives
//! in ClaudeProxyDeployment (Python/Ray Serve) for defense-in-depth.
//!
//! Issue: alternatefutures/admin#132

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use dashmap::DashMap;

/// Global and per-tenant Claude API token budget.
///
/// Budget windows reset every hour. Values are advisory on the Rust side
/// (the actual gate is in the cluster); this prevents the Rust runtime from
/// even attempting a Claude route when the budget is clearly exhausted.
pub struct ClaudeBudgetGate {
    global_used: Arc<AtomicU64>,
    global_limit: u64,
    tenant_used: Arc<DashMap<String, AtomicU64>>,
    per_tenant_limit: u64,
    window_start: Mutex<Instant>,
    window_duration_secs: u64,
}

impl ClaudeBudgetGate {
    pub fn new(global_hourly_limit: u64, per_tenant_hourly_limit: u64) -> Self {
        Self {
            global_used: Arc::new(AtomicU64::new(0)),
            global_limit: global_hourly_limit,
            tenant_used: Arc::new(DashMap::new()),
            per_tenant_limit: per_tenant_hourly_limit,
            window_start: Mutex::new(Instant::now()),
            window_duration_secs: 3600,
        }
    }

    /// Returns true if there is sufficient Claude budget for the given tenant.
    pub fn has_capacity(&self, tenant_id: &str) -> bool {
        self.maybe_reset_window();
        self.check_global() && self.check_tenant(tenant_id)
    }

    /// Record token usage after a Claude API response.
    pub fn record_usage(&self, tenant_id: &str, tokens: u64) {
        self.global_used.fetch_add(tokens, Ordering::Relaxed);
        self.tenant_used
            .entry(tenant_id.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(tokens, Ordering::Relaxed);
    }

    fn check_global(&self) -> bool {
        self.global_used.load(Ordering::Relaxed) < self.global_limit
    }

    fn check_tenant(&self, tenant_id: &str) -> bool {
        match self.tenant_used.get(tenant_id) {
            Some(used) => used.load(Ordering::Relaxed) < self.per_tenant_limit,
            None => true, // No usage recorded yet → capacity available
        }
    }

    fn maybe_reset_window(&self) {
        let mut start = self.window_start.lock().unwrap();
        if start.elapsed().as_secs() >= self.window_duration_secs {
            self.global_used.store(0, Ordering::Relaxed);
            self.tenant_used.iter().for_each(|e| {
                e.value().store(0, Ordering::Relaxed);
            });
            *start = Instant::now();
        }
    }
}

// TODO(#132): Feed Claude API response headers (x-ratelimit-remaining-requests,
// x-ratelimit-reset-requests) back into this gate for accurate remaining capacity.
// See design §7.4.
