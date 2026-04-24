//! Agent freeze — serialization and checkpoint write on task completion.
//!
//! Implements the freeze side of the hydrate/freeze lifecycle (RFC-001 §1.2).
//! Called by the ractor actor runtime when an agent completes a task.
//! Design: docs/architecture/design/state-schema.md §4.2, §7.1
//! Issue: alternatefutures/admin#134

use prost::Message;
use sha2::{Digest, Sha256};
use tracing::{debug, info, instrument};

use crate::error::{StateError, StateResult};
use crate::proto::{
    AgentLifecycleStatus, AgentStateRecord, CheckpointRecord, CheckpointTrigger,
    WorkingMemoryRecord,
};
use crate::store::{DaprStateStore, ETag, TtlPolicy};
use crate::{agent_state_key, checkpoint_key, working_memory_key, LATEST_SCHEMA_VERSION};

/// Input to the freeze operation.
pub struct FreezeInput {
    pub tenant_id: String,
    pub agent_id: String,
    pub state: AgentStateRecord,
    pub working_memory: WorkingMemoryRecord,
    /// ETag of the AgentStateRecord as read during hydration.
    pub state_etag: ETag,
    /// ETag of the WorkingMemoryRecord as read during hydration.
    pub memory_etag: ETag,
    /// Trigger for the checkpoint write.
    pub checkpoint_trigger: CheckpointTrigger,
}

/// Manages the freeze operation: writing agent state back to the Dapr store.
pub struct AgentFreezeManager {
    store: DaprStateStore,
    /// Checkpoint retention policy: how many checkpoints to keep per agent.
    max_retained_checkpoints: u32,
}

impl AgentFreezeManager {
    pub fn new(store: DaprStateStore) -> Self {
        Self {
            store,
            max_retained_checkpoints: 5, // default per state-schema.md §7.2
        }
    }

    /// Freeze an agent: write state + working memory + checkpoint to the Dapr store.
    ///
    /// Write order (state-schema.md §4.2):
    ///   1. Increment working_memory.version
    ///   2. Write working_memory key (with ETag)
    ///   3. Update state.working_memory_version to match
    ///   4. Set state.status = FROZEN
    ///   5. Write state key (with ETag)
    ///   6. Write checkpoint (no ETag needed — checkpoint keys are unique by seq)
    ///
    /// If step 2 succeeds but step 5 fails → torn write will be detected on next
    /// hydration → checkpoint recovery triggered automatically (state-schema.md §7.3).
    #[instrument(skip(self, input), fields(tenant=%input.tenant_id, agent=%input.agent_id))]
    pub async fn freeze(&self, mut input: FreezeInput) -> StateResult<u64> {
        let state_key = agent_state_key(&input.tenant_id, &input.agent_id);
        let mem_key = working_memory_key(&input.tenant_id, &input.agent_id);

        // Step 1: Increment working memory version
        let new_mem_version = input.working_memory.version + 1;
        input.working_memory.version = new_mem_version;
        input.working_memory.snapshot_at = Some(current_timestamp());
        input.working_memory.schema_version = LATEST_SCHEMA_VERSION;

        // Step 2: Write working memory
        let mem_bytes = bytes::Bytes::from(input.working_memory.encode_to_vec());
        let new_mem_etag = self
            .store
            .set(&mem_key, mem_bytes.clone(), Some(&input.memory_etag), TtlPolicy::Seconds(86400))
            .await?;

        debug!("wrote working_memory for {} (version={})", input.agent_id, new_mem_version);

        // Step 3 & 4: Update state record
        input.state.working_memory_version = new_mem_version;
        input.state.status = AgentLifecycleStatus::Frozen as i32;
        input.state.last_frozen_at = Some(current_timestamp());
        input.state.current_task_id = String::new();
        input.state.current_task_taint = Vec::new();
        input.state.schema_version = LATEST_SCHEMA_VERSION;

        // Step 5: Write state record
        let state_bytes = bytes::Bytes::from(input.state.encode_to_vec());
        let new_state_etag = self
            .store
            .set(&state_key, state_bytes.clone(), Some(&input.state_etag), TtlPolicy::None)
            .await?;

        debug!("wrote state for {} (status=FROZEN)", input.agent_id);

        // Step 6: Write checkpoint (async, best-effort — freeze already succeeded)
        let checkpoint_seq = self
            .write_checkpoint(&input, new_mem_version, state_bytes, mem_bytes)
            .await
            .unwrap_or_else(|e| {
                // Checkpoint write failure is non-fatal: the state is already frozen.
                // Log and continue; the agent is safely frozen.
                tracing::warn!(
                    "checkpoint write failed for {} (non-fatal): {e}",
                    input.agent_id
                );
                0
            });

        info!("frozen agent {} (checkpoint_seq={})", input.agent_id, checkpoint_seq);
        Ok(checkpoint_seq)
    }

    /// Write a checkpoint record and return the checkpoint sequence number.
    async fn write_checkpoint(
        &self,
        input: &FreezeInput,
        _new_mem_version: u64,
        state_bytes: bytes::Bytes,
        mem_bytes: bytes::Bytes,
    ) -> StateResult<u64> {
        // Derive next seq: in production, use a Dapr actor counter or scan for max.
        // For Phase 1 skeleton: use current Unix timestamp as seq (unique enough).
        let seq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Compute SHA-256 integrity digest
        let mut hasher = Sha256::new();
        hasher.update(&state_bytes);
        hasher.update(&mem_bytes);
        let digest = hasher.finalize().to_vec();

        let checkpoint = CheckpointRecord {
            tenant_id: input.tenant_id.clone(),
            agent_id: input.agent_id.clone(),
            checkpoint_seq: seq,
            trigger: input.checkpoint_trigger as i32,
            pre_task_id: String::new(),
            state_snapshot: Some(input.state.clone()),
            memory_snapshot: Some(input.working_memory.clone()),
            sha256_digest: digest,
            created_at: Some(current_timestamp()),
            schema_version: LATEST_SCHEMA_VERSION,
        };

        let cp_key = checkpoint_key(&input.tenant_id, &input.agent_id, seq);
        let cp_bytes = bytes::Bytes::from(checkpoint.encode_to_vec());
        self.store.set(&cp_key, cp_bytes, None, TtlPolicy::None).await?;

        Ok(seq)
    }
}

/// Build a prost Timestamp for the current system time.
fn current_timestamp() -> prost_types::Timestamp {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    prost_types::Timestamp {
        seconds: now.as_secs() as i64,
        nanos: now.subsec_nanos() as i32,
    }
}
