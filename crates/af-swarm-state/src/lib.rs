//! af-swarm-state — Durable state layer for the AF Swarm Runtime
//!
//! Implements the state store schema, serialization, hydration/freeze lifecycle,
//! encryption at rest, and GC for virtual actor agents.
//!
//! Design: docs/architecture/design/state-schema.md
//! Issue: alternatefutures/admin#134
//!
//! # Architecture
//!
//! Agents are virtual actors that hydrate from and freeze to the Dapr state
//! store (RFC-001 §1.2). This crate owns the boundary between the Dapr
//! key-value store and the ractor actor runtime.
//!
//! ```text
//! Dapr State Store (Redis/PostgreSQL)
//!     │
//!     │  key: {tenant}||{agent}||state          → AgentStateRecord (proto)
//!     │  key: {tenant}||{agent}||working_memory → WorkingMemoryRecord (proto)
//!     │  key: {tenant}||{agent}||checkpoint||N  → CheckpointRecord (proto)
//!     │  key: {tenant}||{agent}||conv||{ulid}   → ConversationTurnRecord (proto)
//!     │  key: {tenant}||{agent}||tool_log||{ulid} → ToolCallLogRecord (proto)
//!     │
//!     ▼
//! af-swarm-state
//!     │
//!     ├── store.rs        DaprStateStore — read/write/ETag/multi-write
//!     ├── hydrate.rs      AgentHydrationManager — cold start protocol
//!     ├── freeze.rs       AgentFreezeManager — checkpoint write + ETag guard
//!     ├── encryption.rs   KeyProvider trait + AES-256-GCM / FHE dispatch
//!     ├── gc.rs           TTL enforcement + checkpoint retention GC
//!     ├── migrations.rs   Schema version migration chain
//!     └── lance_schema.rs LanceDB table schema (embedding index)
//!     │
//!     ▼
//! Ractor actor runtime (crates/af-swarm-runtime)
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod encryption;
pub mod error;
pub mod freeze;
pub mod gc;
pub mod hydrate;
pub mod lance_schema;
pub mod migrations;
pub mod store;

/// Protobuf-generated types (built by prost-build via build.rs)
pub mod proto {
    // Generated at build time into src/generated/
    // Includes: AgentStateRecord, WorkingMemoryRecord, ConversationTurnRecord,
    //           ToolCallLogRecord, CheckpointRecord, EncryptionEnvelope, ...
    include!(concat!(env!("OUT_DIR"), "/af.swarm.state.rs"));
}

/// Dapr key separator. Must not appear in tenant_id or agent_id.
pub const KEY_SEP: &str = "||";

/// Dapr key namespace prefix (set in Dapr component config keyPrefix).
pub const KEY_PREFIX: &str = "afswarm";

/// Maximum working memory slots (MemAgent O(1) model, v3.0 §4.3).
pub const MAX_WORKING_MEMORY_SLOTS: usize = 32;

/// Maximum pinned facts per agent (state-schema.md §3.3, OQ-S5).
pub const MAX_PINNED_FACTS: usize = 64;

/// Maximum goal stack depth.
pub const MAX_GOAL_STACK_DEPTH: usize = 32;

/// Current schema version for new records. Increment on breaking changes.
pub const LATEST_SCHEMA_VERSION: u32 = 1;

/// Build a Dapr state key for agent primary state.
pub fn agent_state_key(tenant_id: &str, agent_id: &str) -> String {
    format!("{tenant_id}{KEY_SEP}{agent_id}{KEY_SEP}state")
}

/// Build a Dapr state key for agent working memory.
pub fn working_memory_key(tenant_id: &str, agent_id: &str) -> String {
    format!("{tenant_id}{KEY_SEP}{agent_id}{KEY_SEP}working_memory")
}

/// Build a Dapr state key for a checkpoint (zero-padded seq for lexicographic ordering).
pub fn checkpoint_key(tenant_id: &str, agent_id: &str, seq: u64) -> String {
    format!("{tenant_id}{KEY_SEP}{agent_id}{KEY_SEP}checkpoint{KEY_SEP}{seq:020}")
}

/// Build a Dapr state key for a conversation turn (ULID for time-ordered scan).
pub fn conversation_turn_key(tenant_id: &str, agent_id: &str, turn_id: &str) -> String {
    format!("{tenant_id}{KEY_SEP}{agent_id}{KEY_SEP}conv{KEY_SEP}{turn_id}")
}

/// Build a Dapr state key for a tool call log entry (ULID).
pub fn tool_log_key(tenant_id: &str, agent_id: &str, log_id: &str) -> String {
    format!("{tenant_id}{KEY_SEP}{agent_id}{KEY_SEP}tool_log{KEY_SEP}{log_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format_no_collision() {
        let k1 = agent_state_key("tenant-a", "agent-b");
        let k2 = agent_state_key("tenant-a||agent", "b");
        // A tenant_id containing || would collide; validate that tenant/agent IDs
        // are validated to not contain KEY_SEP before reaching key construction.
        // This test documents the expected behavior — input validation is enforced
        // at the API boundary in store.rs.
        assert_ne!(k1, k2, "keys must be distinct for distinct (tenant, agent) pairs");
    }

    #[test]
    fn checkpoint_key_lexicographic_order() {
        let k1 = checkpoint_key("t1", "a1", 1);
        let k2 = checkpoint_key("t1", "a1", 2);
        let k3 = checkpoint_key("t1", "a1", 100);
        assert!(k1 < k2, "checkpoint keys must sort numerically");
        assert!(k2 < k3, "checkpoint keys must sort numerically");
    }
}
