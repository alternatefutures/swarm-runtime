//! Dapr state store client for af-swarm-state.
//!
//! Wraps Dapr's state management API with ETag-based optimistic concurrency.
//! Design: docs/architecture/design/state-schema.md §4
//! Issue: alternatefutures/admin#134
//!
//! ⚠️ COORDINATION REQUIRED — senku (#130):
//!   The Dapr state store backend (Redis vs PostgreSQL) is senku's decision.
//!   The DaprStateStore here is backend-agnostic at the API level.
//!   TTL handling differs: Redis uses EXPIRE natively; PostgreSQL needs GC.
//!   See OQ-S3 in state-schema.md.

use bytes::Bytes;
use prost::Message;
use tracing::{debug, error, instrument, warn};

use crate::error::{StateError, StateResult};
use crate::{KEY_SEP};

/// ETag value returned by Dapr state store reads.
/// Used for optimistic concurrency: write must supply the same ETag it read.
#[derive(Debug, Clone)]
pub struct ETag(pub String);

/// A state record with its ETag.
#[derive(Debug)]
pub struct StateRecord {
    pub value: Bytes,
    pub etag: ETag,
}

/// TTL configuration for a state write operation.
#[derive(Debug, Clone)]
pub enum TtlPolicy {
    /// No TTL — record persists until explicitly deleted.
    None,
    /// Expire the record after N seconds.
    /// For Redis backend: uses EXPIRE. For PostgreSQL: scheduled via GC job.
    Seconds(u64),
}

/// Dapr state store client.
///
/// Wraps the Dapr Rust SDK (github.com/dapr/rust-sdk) with:
///   - ETag-based optimistic concurrency for all writes
///   - Input validation (no KEY_SEP in tenant/agent IDs)
///   - Structured error mapping to StateError variants
///
/// All keys are prefixed with the component's keyPrefix (set in Dapr YAML config).
pub struct DaprStateStore {
    // TODO (Phase 1): Replace with actual dapr::Client when wiring up the Dapr SDK.
    // This skeleton uses a placeholder to define the interface without
    // depending on a running Dapr sidecar for compilation.
    store_name: String,
}

impl DaprStateStore {
    /// Create a new DaprStateStore connected to the named Dapr state component.
    /// `store_name` must match the `metadata.name` in the Dapr component YAML.
    pub fn new(store_name: impl Into<String>) -> Self {
        Self {
            store_name: store_name.into(),
        }
    }

    /// Validate that a tenant_id or agent_id does not contain the KEY_SEP character,
    /// which would produce ambiguous composite keys.
    fn validate_id(value: &str, field: &str) -> StateResult<()> {
        if value.contains(KEY_SEP) {
            return Err(StateError::InvalidInput(format!(
                "{field} must not contain '{KEY_SEP}': got {value:?}"
            )));
        }
        if value.is_empty() {
            return Err(StateError::InvalidInput(format!("{field} must not be empty")));
        }
        Ok(())
    }

    /// Read a state record by key, returning the value bytes and its ETag.
    /// Returns None if the key does not exist.
    #[instrument(skip(self), fields(store = %self.store_name))]
    pub async fn get(&self, key: &str) -> StateResult<Option<StateRecord>> {
        // TODO (Phase 1): Implement with dapr::Client.get_state()
        // Dapr get_state returns (value, etag) with strong consistency by default.
        debug!("get state: key={key}");
        todo!("wire Dapr SDK client — Phase 1")
    }

    /// Write a state record with ETag guard (optimistic concurrency).
    ///
    /// If `etag` is None, writes unconditionally (first-write semantics).
    /// If `etag` is Some, the write fails with StateError::ETagConflict if the
    /// current ETag in the store does not match.
    #[instrument(skip(self, value), fields(store = %self.store_name))]
    pub async fn set(
        &self,
        key: &str,
        value: Bytes,
        etag: Option<&ETag>,
        ttl: TtlPolicy,
    ) -> StateResult<ETag> {
        // TODO (Phase 1): Implement with dapr::Client.save_state() with ETag metadata.
        // Map ETAG_MISMATCH Dapr error to StateError::ETagConflict.
        debug!("set state: key={key} ttl={ttl:?}");
        todo!("wire Dapr SDK client — Phase 1")
    }

    /// Delete a state record by key with ETag guard.
    #[instrument(skip(self), fields(store = %self.store_name))]
    pub async fn delete(&self, key: &str, etag: Option<&ETag>) -> StateResult<()> {
        // TODO (Phase 1): Implement with dapr::Client.delete_state().
        debug!("delete state: key={key}");
        todo!("wire Dapr SDK client — Phase 1")
    }

    /// Multi-state write: save multiple records in a single Dapr call.
    ///
    /// Used for the freeze operation (AgentStateRecord + WorkingMemoryRecord
    /// written together). Note: NOT a distributed atomic transaction — Dapr
    /// processes each key individually. Write order: working_memory first, then
    /// state (so version mismatch detection works correctly on recovery).
    #[instrument(skip(self, items), fields(store = %self.store_name))]
    pub async fn set_bulk(
        &self,
        items: Vec<(String, Bytes, Option<ETag>, TtlPolicy)>,
    ) -> StateResult<Vec<ETag>> {
        // TODO (Phase 1): Implement with dapr::Client.execute_state_transaction()
        // or sequential save_state calls with ETags.
        debug!("set_bulk: {} records", items.len());
        todo!("wire Dapr SDK client — Phase 1")
    }

    /// List all keys matching a prefix pattern (for checkpoint enumeration, GC).
    ///
    /// Note: Dapr's state management API does not provide a native key scan.
    /// For Redis backend: use a Redis SCAN via the Dapr query API.
    /// For PostgreSQL backend: use a LIKE query via the Dapr query API.
    ///
    /// ⚠️ This requires the Dapr query state API (alpha feature in Dapr 1.11+).
    /// Coordinate with senku (#130) on whether the chosen backend supports it.
    #[instrument(skip(self), fields(store = %self.store_name))]
    pub async fn list_keys_with_prefix(&self, prefix: &str) -> StateResult<Vec<String>> {
        // TODO (Phase 1 or Phase 9): Implement via Dapr query API.
        debug!("list_keys_with_prefix: prefix={prefix}");
        todo!("wire Dapr query API — Phase 1")
    }
}
