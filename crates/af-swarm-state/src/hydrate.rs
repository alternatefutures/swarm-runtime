//! Agent hydration — cold start protocol from persisted state.
//!
//! Implements the hydration protocol from state-schema.md §7.3.
//! Called by the ractor actor runtime when a FROZEN agent receives a task.
//! Issue: alternatefutures/admin#134

use prost::Message;
use tracing::{error, info, instrument, warn};

use crate::error::{StateError, StateResult};
use crate::proto::{AgentLifecycleStatus, AgentStateRecord, WorkingMemoryRecord};
use crate::store::DaprStateStore;
use crate::{agent_state_key, working_memory_key};

/// The result of a successful hydration.
pub struct HydrationResult {
    pub state: AgentStateRecord,
    pub working_memory: WorkingMemoryRecord,
    /// Indicates the state was recovered from a checkpoint (torn write recovery).
    pub recovered_from_checkpoint: bool,
}

/// Manages the cold-start hydration of a virtual actor agent from the Dapr store.
pub struct AgentHydrationManager {
    store: DaprStateStore,
    /// How many seconds a record's `last_hydrated_at` can be stale before we
    /// consider the agent stuck and allow force-freeze (FM-22).
    stale_active_threshold_secs: u64,
}

impl AgentHydrationManager {
    pub fn new(store: DaprStateStore) -> Self {
        Self {
            store,
            stale_active_threshold_secs: 300, // 5 minutes default (RFC-001 §1.2)
        }
    }

    /// Hydrate an agent from the Dapr state store.
    ///
    /// Implements the cold-start protocol:
    ///   1. Read AgentStateRecord
    ///   2. Consistency check (working_memory_version)
    ///   3. Stale ACTIVE guard
    ///   4. Lazy-load WorkingMemoryRecord
    ///   5. Decrypt if encrypted
    ///   6. Return hydrated state to ractor actor runtime
    #[instrument(skip(self), fields(tenant=%tenant_id, agent=%agent_id))]
    pub async fn hydrate(
        &self,
        tenant_id: &str,
        agent_id: &str,
    ) -> StateResult<HydrationResult> {
        let state_key = agent_state_key(tenant_id, agent_id);
        let mem_key = working_memory_key(tenant_id, agent_id);

        // Step 1: Read primary state record
        let state_record = self.store.get(&state_key).await?;
        let (mut state, state_etag) = match state_record {
            None => {
                // First hydration: agent has no persisted state yet.
                // The ractor runtime should initialize a new AgentStateRecord.
                return Err(StateError::InvalidInput(format!(
                    "no state record found for agent {agent_id}; must create new agent"
                )));
            }
            Some(r) => {
                let state = AgentStateRecord::decode(r.value)
                    .map_err(StateError::ProtoError)?;
                (state, r.etag)
            }
        };

        // Step 2: Stale ACTIVE guard (FM-22)
        if state.status == AgentLifecycleStatus::Active as i32 {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let last_hydrated_secs = state
                .last_hydrated_at
                .as_ref()
                .map(|t| t.seconds as u64)
                .unwrap_or(0);

            let age = now_secs.saturating_sub(last_hydrated_secs);

            if age < self.stale_active_threshold_secs {
                return Err(StateError::AgentBusy {
                    tenant_id: tenant_id.to_string(),
                    agent_id: agent_id.to_string(),
                    last_hydrated_secs: age,
                });
            }

            warn!(
                "agent {agent_id} was ACTIVE for {age}s (> {}s threshold); force-freezing",
                self.stale_active_threshold_secs
            );
            // Force-freeze: the ractor actor is presumed dead; allow re-hydration.
            // The actual status update will happen after working memory is loaded.
        }

        // Step 3: Load working memory
        let mem_record = self.store.get(&mem_key).await?;
        let (working_memory, recovered) = match mem_record {
            None => {
                warn!("working memory missing for {agent_id}; attempting checkpoint recovery");
                let mem = self.recover_from_checkpoint(tenant_id, agent_id).await?;
                (mem, true)
            }
            Some(r) => {
                let mem = WorkingMemoryRecord::decode(r.value)
                    .map_err(StateError::ProtoError)?;

                // Step 4: Consistency check (torn write detection, FM-21)
                if mem.version != state.working_memory_version {
                    warn!(
                        "torn write detected for {agent_id}: state.wm_version={} != mem.version={}; recovering",
                        state.working_memory_version, mem.version
                    );
                    let recovered_mem = self
                        .recover_from_checkpoint(tenant_id, agent_id)
                        .await
                        .map_err(|e| StateError::TornWrite {
                            agent_id: agent_id.to_string(),
                            state_version: state.working_memory_version,
                            memory_version: mem.version,
                        })?;
                    // Update state to reflect checkpoint restoration
                    // (checkpoint_recovery sets working_memory_version correctly)
                    (recovered_mem, true)
                } else {
                    (mem, false)
                }
            }
        };

        // TODO (Phase 6): Decrypt encrypted records using KeyProvider.
        // For Phase 1, encryption.scheme == PLAINTEXT for all records.

        info!("hydrated agent {agent_id} (recovered={recovered})");

        Ok(HydrationResult {
            state,
            working_memory,
            recovered_from_checkpoint: recovered,
        })
    }

    /// Recover agent state from the most recent valid checkpoint.
    /// Called when torn write is detected or working memory key is missing.
    async fn recover_from_checkpoint(
        &self,
        tenant_id: &str,
        agent_id: &str,
    ) -> StateResult<WorkingMemoryRecord> {
        // TODO (Phase 1): Implement full checkpoint recovery:
        //   1. List checkpoint keys via store.list_keys_with_prefix()
        //   2. Sort descending by seq (lexicographic sort on zero-padded key)
        //   3. For each checkpoint, verify SHA-256 digest
        //   4. Restore AgentStateRecord + WorkingMemoryRecord from first valid checkpoint
        //   5. Emit TornWriteRecoveryEvent to observability (tracing span)
        todo!("checkpoint recovery — Phase 1")
    }
}
