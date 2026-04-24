//! Error types for af-swarm-state.
//! Issue: alternatefutures/admin#134

use thiserror::Error;

/// Top-level error type for state store operations.
#[derive(Debug, Error)]
pub enum StateError {
    /// Dapr state write failed due to ETag mismatch (concurrent write detected).
    /// The caller should retry: re-read the record, apply changes, re-write.
    #[error("ETag conflict on key {key}: concurrent write detected")]
    ETagConflict { key: String },

    /// Agent is currently ACTIVE (hydrated and processing a task).
    /// The orchestrator should route the task to the active agent or queue it.
    #[error("agent {agent_id} in tenant {tenant_id} is currently ACTIVE (last hydrated: {last_hydrated_secs}s ago)")]
    AgentBusy {
        tenant_id: String,
        agent_id: String,
        last_hydrated_secs: u64,
    },

    /// Torn write detected: AgentStateRecord.working_memory_version does not
    /// match WorkingMemoryRecord.version. Triggers checkpoint recovery.
    #[error("torn write detected for agent {agent_id}: state.working_memory_version={state_version} != memory.version={memory_version}")]
    TornWrite {
        agent_id: String,
        state_version: u64,
        memory_version: u64,
    },

    /// No valid checkpoint found to recover from after a torn write.
    #[error("no valid checkpoint for agent {agent_id} in tenant {tenant_id}; manual recovery required")]
    NoCheckpointAvailable { tenant_id: String, agent_id: String },

    /// Checkpoint integrity check failed (SHA-256 digest mismatch).
    #[error("checkpoint {seq} for agent {agent_id} has invalid digest; possible tampering")]
    CheckpointCorrupted { agent_id: String, seq: u64 },

    /// Encryption key is unavailable. Agent hydration is rejected — do NOT
    /// proceed with plaintext (v3.0 §5.3.2 RESTRICTED routing).
    #[error("encryption key {key_id} unavailable for agent {agent_id}: {source}")]
    EncryptionKeyUnavailable {
        key_id: String,
        agent_id: String,
        source: String,
    },

    /// LanceDB record IDs referenced in AgentStateRecord are not present in the
    /// local LanceDB instance. Agent migrated to a new node without data transfer.
    /// Degrade to SYNAPSE-only retrieval (FM-23, state-schema.md §11).
    #[error("LanceDB migration gap for agent {agent_id}: {missing_count} record(s) missing")]
    LanceDbMigrationGap { agent_id: String, missing_count: usize },

    /// Schema version in the stored record is newer than this binary knows how
    /// to handle. Binary needs to be updated.
    #[error("unsupported schema version {found} for record type {record_type} (max known: {max_known})")]
    UnsupportedSchemaVersion {
        record_type: String,
        found: u32,
        max_known: u32,
    },

    /// Input validation failure (e.g. tenant_id or agent_id contains KEY_SEP).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Underlying Dapr state store error.
    #[error("Dapr state store error: {0}")]
    DaprError(String),

    /// Protobuf serialization/deserialization error.
    #[error("proto encode/decode error: {0}")]
    ProtoError(#[from] prost::DecodeError),

    /// AES-256-GCM encryption/decryption error.
    #[error("AES-GCM error: authentication tag mismatch or corrupted ciphertext")]
    AesGcmError,

    /// Generic I/O or storage error.
    #[error("storage error: {0}")]
    Storage(#[from] anyhow::Error),
}

/// Result alias for state store operations.
pub type StateResult<T> = Result<T, StateError>;
