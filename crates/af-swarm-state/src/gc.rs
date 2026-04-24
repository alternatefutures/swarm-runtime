//! TTL enforcement and checkpoint retention garbage collection.
//!
//! Design: docs/architecture/design/state-schema.md §9
//! Issue: alternatefutures/admin#134
//!
//! Runs as a background Tokio task on each runtime node.
//! For Redis backend: most TTLs are handled natively via EXPIRE.
//! For PostgreSQL backend: this GC job is the primary TTL enforcement mechanism.

use tracing::{debug, info, instrument, warn};

use crate::error::StateResult;
use crate::store::DaprStateStore;

/// GC configuration loaded from per-tenant TOML config.
#[derive(Debug, Clone)]
pub struct GcConfig {
    /// How many checkpoints to retain per agent (default: 5).
    pub max_retained_checkpoints: u32,
    /// Whether to always retain pre-task checkpoints regardless of max_retained.
    pub retain_pre_task_checkpoints: bool,
    /// Minimum days to retain records for compliance (0 = use default TTL).
    /// Overrides max_retained_checkpoints for records within the retention window.
    pub compliance_retain_days: u32,
    /// Days to retain conversation turn records (default: 30).
    pub conversation_turn_ttl_days: u32,
    /// Days to retain tool call log records (default: 90).
    pub tool_log_ttl_days: u32,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            max_retained_checkpoints: 5,
            retain_pre_task_checkpoints: true,
            compliance_retain_days: 0,
            conversation_turn_ttl_days: 30,
            tool_log_ttl_days: 90,
        }
    }
}

/// Background GC task runner.
pub struct GcRunner {
    store: DaprStateStore,
    config: GcConfig,
    /// How often to run the GC sweep (seconds).
    interval_secs: u64,
}

impl GcRunner {
    pub fn new(store: DaprStateStore, config: GcConfig) -> Self {
        Self {
            store,
            config,
            interval_secs: 300, // 5 minutes default
        }
    }

    /// Run the GC loop indefinitely. Intended to be spawned as a Tokio task.
    pub async fn run(&self) {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(self.interval_secs));
        loop {
            interval.tick().await;
            if let Err(e) = self.sweep().await {
                warn!("GC sweep error: {e}");
            }
        }
    }

    /// Run one GC sweep across all agents visible in the state store.
    #[instrument(skip(self))]
    async fn sweep(&self) -> StateResult<()> {
        debug!("starting GC sweep");

        // TODO (Phase 1): Implement full GC sweep:
        //   1. List all agent state keys via store.list_keys_with_prefix(tenant || "||")
        //   2. For each agent:
        //      a. Checkpoint GC: enforce max_retained_checkpoints (§9.2)
        //      b. Conversation turn GC: delete turns older than conversation_turn_ttl_days
        //      c. Tool log GC: delete logs older than tool_log_ttl_days
        //      d. Working memory GC: delete if agent last_frozen_at > 24h ago and no active
        //         (working_memory is re-created on next hydration)
        //   3. Respect compliance_retain_days: never delete records within the window
        //   4. Log metrics: records_deleted, records_retained_compliance

        info!("GC sweep complete");
        Ok(())
    }

    /// GDPR erasure: purge all state for a given tenant.
    ///
    /// ⚠️ See OQ-S4: GDPR erasure during a compliance hold requires policy decision
    /// from argus (#133) and legal. For Phase 1, this implements immediate deletion
    /// without compliance hold checking.
    #[instrument(skip(self), fields(tenant=%tenant_id))]
    pub async fn erase_tenant(&self, tenant_id: &str) -> StateResult<ErasureReport> {
        // TODO (Phase 1): Implement GDPR erasure:
        //   1. Delete state + working_memory keys for all agents in tenant
        //   2. Delete all conv||* and tool_log||* keys
        //   3. Mark checkpoint||* keys for deletion (may be deferred by compliance hold)
        //   4. Delete all LanceDB entries for tenant (via LanceDB table filter query)
        //   5. Return ErasureReport with counts
        todo!("GDPR erasure — Phase 1")
    }
}

/// Report from a GDPR erasure operation.
#[derive(Debug)]
pub struct ErasureReport {
    pub tenant_id: String,
    pub agents_deleted: u64,
    pub state_records_deleted: u64,
    pub turn_records_deleted: u64,
    pub tool_log_records_deleted: u64,
    pub checkpoint_records_deleted: u64,
    pub checkpoint_records_pending_compliance_hold: u64,
    pub lancedb_entries_deleted: u64,
}
