//! AF Swarm Runtime — Agent trait definitions and lifecycle types
//!
//! This file defines the core traits and types for the agent subsystem.
//! Method bodies are intentionally `todo!()` — this is a design scaffold for
//! Phase 0/1 of the 1000-agent Rust rewrite.
//!
//! # Coordination notes
//! - AgentSnapshot serialization format: coordinate with quinn (#134) before implementation.
//!   The field layout here is the source of truth for what must be wire-encodable.
//! - AgentId / TaskId / MessageEnvelope as shared crate types: coordinate with
//!   chief-architect (#136). If a shared crate is created, move these newtypes there.
//! - See docs/architecture/design/rust-runtime-internals.md for full design rationale.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Core newtypes
// ---------------------------------------------------------------------------
// NOTE [OQ-RI-4]: These newtypes should move to a shared crate once
// chief-architect (#136) defines the cross-component type surface.

/// Unique identifier for an agent instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub Uuid);

impl AgentId {
    pub fn new() -> Self {
        AgentId(Uuid::new_v4())
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "agent:{}", self.0)
    }
}

/// Unique identifier for a task dispatched to an agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub Uuid);

impl TaskId {
    pub fn new() -> Self {
        TaskId(Uuid::new_v4())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// Correlation ID for request-response pairing across agents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CorrelationId(pub Uuid);

// ---------------------------------------------------------------------------
// Lifecycle state
// ---------------------------------------------------------------------------

/// The lifecycle state of an agent — used both in-process and persisted to
/// the Dapr actor registry for fast lookup without loading the full snapshot.
///
/// See docs/architecture/design/rust-runtime-internals.md §3.1 for the full
/// state machine diagram and transition table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgentLifecycleState {
    /// Not in memory; full state persisted in Dapr state store.
    Frozen,
    /// Dapr state being read; ractor actor being spawned.
    Hydrating,
    /// Processing messages; Flume inbox active.
    Active,
    /// Finishing in-flight work before freeze; inbox closed to new messages.
    Draining,
    /// Serializing state to Dapr; actor about to be dropped.
    Freezing,
    /// Crashed past restart budget; requires explicit recovery command.
    Quarantined,
}

// ---------------------------------------------------------------------------
// Agent configuration
// ---------------------------------------------------------------------------

/// Per-agent configuration passed at creation and included in the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Human-readable persona name (e.g. "Senku", "Lain").
    pub persona: String,
    /// Capabilities this agent advertises for task routing.
    pub capabilities: Vec<Capability>,
    /// Maximum number of tools this agent may hold simultaneously.
    /// Per RFC §1.4 / Google-MIT study: hard cap of 16 tools per agent.
    pub max_tools: u32,
    /// Maximum context window in tokens.
    pub max_context_tokens: u32,
    /// Idle TTL in seconds before the agent enters Draining state.
    pub idle_ttl_secs: u64,
    /// Which inference tier this agent is allowed to use.
    pub inference_tier: InferenceTier,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            persona: String::from("unnamed"),
            capabilities: vec![],
            max_tools: 16, // per Google/MIT study cap
            max_context_tokens: 128_000,
            idle_ttl_secs: 300, // 5 minutes
            inference_tier: InferenceTier::Open,
        }
    }
}

/// A capability that an agent can advertise for task routing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capability(pub String);

/// Which inference tier an agent (or task) is permitted to use.
/// Maps directly to the DSE classification used for RESTRICTED POD routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceTier {
    /// Routes to OPEN POOL (Tier C — Akash GPU). For PUBLIC/INTERNAL/SENSITIVE tasks.
    Open,
    /// Routes to RESTRICTED POD (Tier A — controlled hardware). For RESTRICTED tasks.
    /// Per RFC §4.1: if RESTRICTED POD is unavailable, the request is REJECTED — no
    /// fallback to OPEN POOL.
    Restricted,
}

// ---------------------------------------------------------------------------
// Task and message types
// ---------------------------------------------------------------------------

/// Priority level for a dispatched task.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    Normal,
    High,
}

impl Default for TaskPriority {
    fn default() -> Self {
        TaskPriority::Normal
    }
}

/// A task dispatched from the orchestrator to an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDispatch {
    pub task_id: TaskId,
    /// If this task is part of a Dapr workflow, the workflow instance ID.
    pub workflow_instance_id: Option<String>,
    pub payload: TaskPayload,
    pub priority: TaskPriority,
    /// Absolute deadline for task completion (None = no deadline).
    pub deadline: Option<DateTime<Utc>>,
    /// Inference tier required for this task.
    pub inference_tier: InferenceTier,
    /// Optional token budget override (uses AgentConfig.max_context_tokens if None).
    pub max_tokens: Option<u32>,
    /// The agent or orchestrator that dispatched this task.
    pub requester_id: Option<AgentId>,
}

/// Opaque task payload. Structure is protocol-specific; the orchestrator and
/// receiving agent must agree on the schema out-of-band (via capability matching).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPayload {
    /// The type tag used for deserialization routing (e.g., "code.review", "marketing.draft").
    pub kind: String,
    /// The raw payload body.
    pub body: serde_json::Value,
}

/// Result returned by an agent after completing a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub outcome: TaskOutcome,
    pub completed_at: DateTime<Utc>,
}

/// Outcome of a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Success { output: serde_json::Value },
    Failure { error: String, retryable: bool },
    Cancelled { reason: String },
}

/// A direct peer-to-peer message between two agents (L1 path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectMessage {
    pub from: AgentId,
    pub to: AgentId,
    pub correlation_id: CorrelationId,
    /// Flexible payload — structure defined by the sending and receiving agent.
    pub payload: serde_json::Value,
}

/// A lifecycle command sent to an agent by the supervisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleCommand {
    /// Begin draining in-flight work before freeze.
    Drain,
    /// Serialize state to Dapr and stop the actor (follows Drain completion).
    Freeze,
    /// Mark the agent quarantined; do not restart.
    Quarantine { reason: String },
    /// Clear quarantine state and re-hydrate the agent.
    Recover,
}

/// All message variants an AgentActor can receive via its Flume inbox.
#[derive(Debug, Clone)]
pub enum AgentMessage {
    /// A new task dispatched by the orchestrator.
    Task(TaskDispatch),
    /// A direct peer-to-peer message from another agent.
    Direct(DirectMessage),
    /// A broadcast event forwarded from the L2 channel (wrapped in Arc to avoid clone).
    Broadcast(std::sync::Arc<BroadcastEvent>),
    /// A lifecycle command from the AgentSupervisor.
    Lifecycle(LifecycleCommand),
    /// Health check — actor replies with AgentMessage::Pong.
    Ping,
    /// Response to Ping.
    Pong,
}

/// Events published on the L2 tokio::broadcast channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BroadcastEvent {
    /// An agent's lifecycle state changed (e.g., Active → Draining).
    AgentLifecycleChanged {
        agent_id: AgentId,
        old_state: AgentLifecycleState,
        new_state: AgentLifecycleState,
        timestamp: DateTime<Utc>,
    },
    /// An agent has been quarantined (broadcast so orchestrator can reroute tasks).
    AgentQuarantined {
        agent_id: AgentId,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    /// Swarm-wide backpressure alert.
    BackpressureAlert {
        level: MemoryPressureLevel,
        active_agent_count: usize,
        active_agent_limit: usize,
        timestamp: DateTime<Utc>,
    },
    /// A task was completed by an agent.
    TaskCompleted {
        result: TaskResult,
    },
}

/// Memory pressure level reported by SwarmSupervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressureLevel {
    None,
    Low,
    High,
    Critical,
}

// ---------------------------------------------------------------------------
// Working memory
// ---------------------------------------------------------------------------

/// An agent's working memory — persisted in AgentSnapshot, loaded on hydration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkingMemory {
    /// Free-form scratchpad for the current task.
    pub scratchpad: String,
    /// Key-value store for task-local variables.
    pub variables: HashMap<String, serde_json::Value>,
    /// Reference to LanceDB memory store (external; LanceDB owns persistence).
    pub lancedb_collection_id: Option<String>,
}

/// A single turn in the agent's conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: ConversationRole,
    pub content: String,
    pub token_count: u32,
    pub timestamp: DateTime<Utc>,
}

/// Role in a conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRole {
    System,
    User,
    Assistant,
    Tool,
}

/// A task that is pending (queued but not yet dispatched or in-progress).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTask {
    pub task: TaskDispatch,
    pub enqueued_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Agent snapshot (hydrate/freeze protocol)
// ---------------------------------------------------------------------------

/// The full durable state of an agent, serialized to/from the Dapr state store.
///
/// # Serialization coordination [OQ-RI-3]
/// The wire format for this type must be agreed with quinn (#134). The field
/// layout here is the source of truth. quinn owns the encoding (serde_json,
/// prost, or bincode). Do NOT change field names or types without coordinating.
///
/// # Schema versioning
/// `snapshot_version` enables forward-compatible migration. Register migration
/// functions in `SnapshotMigrationRegistry` for `(from_version, to_version)` pairs.
/// Current version: 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSnapshot {
    // --- Identity ---
    pub agent_id: AgentId,
    pub config: AgentConfig,

    // --- Lifecycle ---
    pub lifecycle_state: AgentLifecycleState,
    pub last_active_at: DateTime<Utc>,

    // --- Task context ---
    pub pending_tasks: Vec<PendingTask>,
    pub working_memory: WorkingMemory,

    // --- Conversation context ---
    pub conversation_history: Vec<ConversationTurn>,
    /// Sum of token_count across all conversation_history entries.
    pub context_token_count: u32,

    // --- Serialization metadata ---
    /// Schema version for forward-compatible migration (current: 1).
    pub snapshot_version: u32,
    pub created_at: DateTime<Utc>,
}

impl AgentSnapshot {
    /// Current schema version. Increment when AgentSnapshot fields change in a
    /// backwards-incompatible way. Register a migration function in
    /// SnapshotMigrationRegistry for `(old_version, CURRENT_SNAPSHOT_VERSION)`.
    pub const CURRENT_SNAPSHOT_VERSION: u32 = 1;
}

// ---------------------------------------------------------------------------
// Core traits
// ---------------------------------------------------------------------------

/// The primary agent trait. Implemented by every agent persona.
///
/// Each agent is a ractor actor whose message handler calls these trait methods.
/// Method bodies are `todo!()` — implementation is deferred to Phase 1/2.
pub trait Agent: Send + Sync + 'static {
    /// Returns the agent's unique identifier.
    fn agent_id(&self) -> &AgentId;

    /// Returns the agent's current lifecycle state.
    fn lifecycle_state(&self) -> AgentLifecycleState;

    /// Returns the agent's configuration (capabilities, TTL, tier, etc.).
    fn config(&self) -> &AgentConfig;

    /// Handle an incoming message dispatched via the Flume inbox.
    ///
    /// This method is called inside `std::panic::catch_unwind` at the ractor
    /// message boundary — panics are caught and converted to `AgentError::Panic`.
    ///
    /// Returns a `TaskResult` if the message contained a task that completed
    /// synchronously. Long-running tasks should return `None` here and publish
    /// the result via the L2 broadcast channel when done.
    fn handle_message(
        &mut self,
        msg: AgentMessage,
    ) -> impl std::future::Future<Output = Result<Option<TaskResult>, AgentError>> + Send;

    /// Called by the ractor actor after spawning, to allow the agent to acquire
    /// resources (re-subscribe to L2 topics, reconnect tools, etc.).
    fn on_start(&mut self) -> impl std::future::Future<Output = Result<(), AgentError>> + Send;

    /// Called by the ractor actor before the actor is dropped (graceful shutdown).
    /// The agent should release resources but NOT serialize state here — that is
    /// the `Freeze` trait's responsibility.
    fn on_stop(&mut self) -> impl std::future::Future<Output = Result<(), AgentError>> + Send;
}

/// Trait for loading an agent's durable state from a snapshot.
///
/// Called during the Hydrating → Active transition.
pub trait Hydrate: Agent {
    /// Restore the agent's in-memory state from a `AgentSnapshot` loaded from
    /// the Dapr state store.
    ///
    /// Implementors should:
    /// 1. Restore `working_memory` and `conversation_history`.
    /// 2. Re-enqueue `pending_tasks` into the Flume inbox.
    /// 3. Re-subscribe to any required L2 broadcast topics.
    /// 4. Call `on_start()` to acquire live resources.
    fn hydrate(
        &mut self,
        snapshot: AgentSnapshot,
    ) -> impl std::future::Future<Output = Result<(), HydrationError>> + Send;
}

/// Trait for serializing an agent's durable state to a snapshot.
///
/// Called during the Draining → Freezing → Frozen transition.
pub trait Freeze: Agent {
    /// Serialize the agent's current state to an `AgentSnapshot` for storage
    /// in the Dapr state store.
    ///
    /// Implementors should:
    /// 1. Serialize `working_memory`, `conversation_history`, and `pending_tasks`.
    /// 2. Set `lifecycle_state` to `AgentLifecycleState::Frozen` in the snapshot.
    /// 3. NOT include transient state (Flume inbox, tokio handles, tool connections).
    fn freeze(&self) -> impl std::future::Future<Output = Result<AgentSnapshot, FreezeError>> + Send;

    /// Emergency freeze: called on crash before quarantine (§1.3).
    ///
    /// Should complete in <100ms. Best-effort: if it panics, the last successfully
    /// written Dapr snapshot is used as the recovery checkpoint.
    ///
    /// See [OQ-RI-6]: decide whether the orchestrator needs notification of
    /// potentially lost task progress when emergency freeze falls back to
    /// last-good-checkpoint.
    fn emergency_freeze(
        &self,
    ) -> impl std::future::Future<Output = Result<AgentSnapshot, FreezeError>> + Send;
}

/// Trait for typed message handling at the actor dispatch level.
///
/// Separates the ractor message dispatch boilerplate from agent business logic.
/// The ractor `handle()` method calls the appropriate `MessageHandler` method
/// based on the variant of `AgentMessage`.
pub trait MessageHandler: Agent + Hydrate + Freeze {
    fn handle_task(
        &mut self,
        task: TaskDispatch,
    ) -> impl std::future::Future<Output = Result<Option<TaskResult>, AgentError>> + Send;

    fn handle_direct_message(
        &mut self,
        msg: DirectMessage,
    ) -> impl std::future::Future<Output = Result<(), AgentError>> + Send;

    fn handle_broadcast(
        &mut self,
        event: std::sync::Arc<BroadcastEvent>,
    ) -> impl std::future::Future<Output = Result<(), AgentError>> + Send;

    fn handle_lifecycle_command(
        &mut self,
        cmd: LifecycleCommand,
    ) -> impl std::future::Future<Output = Result<(), AgentError>> + Send;
}

// ---------------------------------------------------------------------------
// Backpressure status
// ---------------------------------------------------------------------------

// --- Backpressure threshold constants ---
// These values are the named thresholds checked by BackpressureStatus methods.
// Changing them requires updating docs/architecture/design/rust-runtime-internals.md §6.2.

/// Throttle new dispatches when active agents exceed this fraction of ACTIVE_AGENT_LIMIT.
/// 0.80 × 100 = 80 active agents triggers throttle.
pub const THROTTLE_AGENT_RATIO: f32 = 0.80;

/// Throttle new dispatches when L3 NATS egress buffer exceeds this fill fraction.
pub const THROTTLE_L3_FILL: f32 = 0.85;

/// Enter critical mode (reject new tasks) when active agents exceed this fraction.
/// 0.95 × 100 = 95 active agents triggers critical.
pub const CRITICAL_AGENT_RATIO: f32 = 0.95;

/// Enter critical mode when L3 NATS egress buffer exceeds this fill fraction.
pub const CRITICAL_L3_FILL: f32 = 0.95;

/// Maximum number of seconds allowed for Hydrating → Active transition.
/// Covers p99 Dapr state read + snapshot deserialize + ractor spawn.
/// Supervisor emits FM-22 and moves agent to Quarantined if this elapses.
pub const HYDRATION_TIMEOUT_SECS: u64 = 15;

/// Backpressure snapshot published by SwarmSupervisor.
/// The OrchestratorActor polls this before dispatching new tasks.
#[derive(Debug, Clone)]
pub struct BackpressureStatus {
    pub active_agent_count: usize,
    pub active_agent_limit: usize,
    pub l3_egress_buffer_fill_pct: f32,
    pub pending_hydrations: usize,
    pub memory_pressure: MemoryPressureLevel,
}

impl BackpressureStatus {
    /// Returns `true` if the runtime should pause new task dispatches.
    ///
    /// Throttle conditions (either is sufficient):
    /// - Active agents > `THROTTLE_AGENT_RATIO` (0.80) × `active_agent_limit`
    /// - L3 NATS egress buffer fill > `THROTTLE_L3_FILL` (0.85)
    ///
    /// See docs/architecture/design/rust-runtime-internals.md §6.2 for threshold rationale.
    pub fn should_throttle(&self) -> bool {
        let agent_threshold = (self.active_agent_limit as f32 * THROTTLE_AGENT_RATIO) as usize;
        self.active_agent_count > agent_threshold
            || self.l3_egress_buffer_fill_pct > THROTTLE_L3_FILL
    }

    /// Returns `true` if the runtime has reached critical pressure.
    ///
    /// Critical conditions (either is sufficient) — orchestrator pauses dispatch entirely:
    /// - Active agents > `CRITICAL_AGENT_RATIO` (0.95) × `active_agent_limit`
    /// - L3 NATS egress buffer fill > `CRITICAL_L3_FILL` (0.95)
    ///
    /// See docs/architecture/design/rust-runtime-internals.md §6.2 for threshold rationale.
    pub fn is_critical(&self) -> bool {
        let agent_threshold = (self.active_agent_limit as f32 * CRITICAL_AGENT_RATIO) as usize;
        self.active_agent_count > agent_threshold
            || self.l3_egress_buffer_fill_pct > CRITICAL_L3_FILL
    }
}

// ---------------------------------------------------------------------------
// State client interface
// OQ-9 (#144): renamed from DaprStateClient. Concrete implementation swapped
// from Dapr state store (Redis/Postgres) to Turso embedded replica (libSQL
// local file). Trait shape is unchanged — callers require no update beyond
// the type name. `StateError` is a type alias for `DaprError` pending a full
// split in Phase 1 (chief-architect #136 to confirm scope).
// ---------------------------------------------------------------------------

/// Interface for reading and writing agent state.
///
/// OQ-9 (#144): previously `DaprStateClient`, backed by Dapr state store.
/// Now backed by Turso embedded replica (local SQLite file, ~1-10µs reads).
/// A mock implementation is used in tests — mock type name unchanged.
pub trait StateClient: Send + Sync + 'static {
    /// Read an agent's snapshot from the state store.
    /// Returns `(snapshot, etag)` — the ETag is required for subsequent writes.
    fn read_agent_snapshot(
        &self,
        agent_id: &AgentId,
    ) -> impl std::future::Future<Output = Result<(AgentSnapshot, ETag), StateError>> + Send;

    /// Write an agent's snapshot using optimistic concurrency.
    /// `etag` must match the current version in the store. Returns the new ETag.
    fn write_agent_snapshot(
        &self,
        agent_id: &AgentId,
        snapshot: &AgentSnapshot,
        etag: &ETag,
    ) -> impl std::future::Future<Output = Result<ETag, StateError>> + Send;

    /// Read an agent's lifecycle state (fast path — no full snapshot deserialization).
    fn read_lifecycle_state(
        &self,
        agent_id: &AgentId,
    ) -> impl std::future::Future<Output = Result<AgentLifecycleState, StateError>> + Send;

    /// Write an agent's lifecycle state with ETag-based concurrency control.
    fn write_lifecycle_state(
        &self,
        agent_id: &AgentId,
        state: AgentLifecycleState,
        etag: &ETag,
    ) -> impl std::future::Future<Output = Result<ETag, StateError>> + Send;

    /// List all currently active agent IDs.
    fn list_active_agents(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<AgentId>, StateError>> + Send;

    /// Register an agent as active in the swarm registry.
    fn register_active(
        &self,
        agent_id: &AgentId,
    ) -> impl std::future::Future<Output = Result<(), StateError>> + Send;

    /// Remove an agent from the active registry (called on freeze or quarantine).
    fn deregister_active(
        &self,
        agent_id: &AgentId,
    ) -> impl std::future::Future<Output = Result<(), StateError>> + Send;
}

/// Type alias for state client errors.
///
/// OQ-9 (#144): `StateError` is the public name for errors from `StateClient`.
/// Currently aliases `DaprError` to avoid a larger rename while `DaprWorkflowClient`
/// still shares the enum. Phase 1: split into `StateError` and `WorkflowError`
/// once chief-architect (#136) defines the final error hierarchy.
pub type StateError = DaprError;

/// An opaque version token used for optimistic concurrency on state writes.
///
/// OQ-9 (#144): with Turso, this wraps a SQLite rowid or millis-based value
/// rather than a Dapr-provided string. The opaque wrapper is intentional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ETag(pub String);

// ---------------------------------------------------------------------------
// Snapshot migration
// ---------------------------------------------------------------------------

/// Registry of migration functions for forward-compatible snapshot deserialization.
///
/// When loading a snapshot with `snapshot_version < AgentSnapshot::CURRENT_SNAPSHOT_VERSION`,
/// the migration chain is applied: `raw_json → v1 → v2 → ... → current`.
///
/// Migrations are registered as:
/// ```ignore
/// registry.register(1, 2, |v1_json| { /* transform */ v2_json });
/// ```
pub struct SnapshotMigrationRegistry {
    // migrations: HashMap<(u32, u32), Box<dyn Fn(serde_json::Value) -> serde_json::Value + Send + Sync>>
    // (field elided for clarity — see implementation)
    _private: (),
}

impl SnapshotMigrationRegistry {
    pub fn new() -> Self {
        todo!()
    }

    /// Register a migration function from `from_version` to `to_version`.
    pub fn register<F>(&mut self, from_version: u32, to_version: u32, f: F)
    where
        F: Fn(serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
    {
        todo!()
    }

    /// Apply all registered migrations to bring `raw` from `from_version`
    /// up to `AgentSnapshot::CURRENT_SNAPSHOT_VERSION`.
    pub fn migrate(
        &self,
        raw: serde_json::Value,
        from_version: u32,
    ) -> Result<serde_json::Value, MigrationError> {
        todo!()
    }
}

impl Default for SnapshotMigrationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors produced by agent message handlers.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent panicked during message handling: {backtrace}")]
    Panic { backtrace: String },

    #[error("task failed: {reason}")]
    TaskFailed { reason: String },

    #[error("lifecycle command failed: {reason}")]
    LifecycleCommandFailed { reason: String },

    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

/// Errors produced by the hydration process.
#[derive(Debug, thiserror::Error)]
pub enum HydrationError {
    #[error("failed to read snapshot from state store: {0}")]
    StateReadFailed(#[from] DaprError),

    #[error("failed to deserialize snapshot (version {snapshot_version}): {reason}")]
    DeserializationFailed {
        snapshot_version: u32,
        reason: String,
    },

    #[error("snapshot migration failed from v{from_version} to v{to_version}: {reason}")]
    MigrationFailed {
        from_version: u32,
        to_version: u32,
        reason: String,
    },

    #[error("hydration timeout after {elapsed_ms}ms")]
    Timeout { elapsed_ms: u64 },
}

/// Errors produced by the freeze process.
#[derive(Debug, thiserror::Error)]
pub enum FreezeError {
    #[error("failed to serialize snapshot: {reason}")]
    SerializationFailed { reason: String },

    #[error("failed to write snapshot to state store: {0}")]
    StateWriteFailed(#[from] DaprError),

    #[error("ETag conflict on freeze — concurrent write detected")]
    ETagConflict,
}

/// Shared error type for Dapr-backed operations (state store and workflows).
///
/// OQ-9 (#144): `StateClient` uses this via the `StateError` type alias.
/// `DaprWorkflowClient` uses it directly (workflows still go through Dapr).
/// Phase 1: split into `StateError` + `WorkflowError` once the Turso
/// implementation removes the `Transport(tonic::Status)` variant from the
/// state path.
#[derive(Debug, thiserror::Error)]
pub enum DaprError {
    #[error("ETag mismatch: concurrent write detected for agent {agent_id}")]
    ETagMismatch { agent_id: String },

    #[error("agent not found in state store: {agent_id}")]
    NotFound { agent_id: String },

    #[error("state store unavailable: {reason}")]
    Unavailable { reason: String },

    #[error("transport error: {0}")]
    Transport(#[from] tonic::Status),

    #[error("internal error: {0}")]
    Internal(String),
}

/// Errors from snapshot version migration.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("no migration path from v{from_version} to v{to_version}")]
    NoMigrationPath { from_version: u32, to_version: u32 },

    #[error("migration step failed (v{from_version} → v{to_version}): {reason}")]
    StepFailed {
        from_version: u32,
        to_version: u32,
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Dapr workflow client interface
// ---------------------------------------------------------------------------

/// Interface for invoking Dapr workflows from the OrchestratorActor.
pub trait DaprWorkflowClient: Send + Sync + 'static {
    /// Start a new workflow instance.
    fn start_workflow(
        &self,
        workflow_name: &str,
        instance_id: &str,
        input: serde_json::Value,
    ) -> impl std::future::Future<Output = Result<String, DaprError>> + Send;

    /// Query the status of a running workflow instance.
    fn get_status(
        &self,
        instance_id: &str,
    ) -> impl std::future::Future<Output = Result<WorkflowStatus, DaprError>> + Send;

    /// Terminate a workflow instance.
    fn terminate_workflow(
        &self,
        instance_id: &str,
    ) -> impl std::future::Future<Output = Result<(), DaprError>> + Send;
}

/// Status of a Dapr workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStatus {
    pub instance_id: String,
    pub runtime_status: WorkflowRuntimeStatus,
    pub created_at: DateTime<Utc>,
    pub last_updated_at: DateTime<Utc>,
    pub output: Option<serde_json::Value>,
}

/// Dapr workflow runtime status values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum WorkflowRuntimeStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Terminated,
    Pending,
    Suspended,
}
