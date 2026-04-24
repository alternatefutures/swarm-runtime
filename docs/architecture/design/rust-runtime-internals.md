# AF Swarm Runtime — Rust Runtime Internals Design

**Track:** Runtime Internals (#131)
**Author:** Lain
**Date:** 2026-04-16
**Status:** DRAFT — pending coordination with quinn (#134) and chief-architect (#136)
**Parent Epic:** #129 (1000-agent Rust Swarm Runtime)
**RFC Ground Truth:** [1000-agent-amendment.md](../reviews/1000-agent-amendment.md) (PR #128)
**v3.0 Plan:** [rust-swarm-runtime.md](../rust-swarm-runtime.md) — DO NOT MODIFY
**Reviewers:** wonderwomancode, mavisakalyan

---

## 0. Scope and Purpose

This document designs the internal architecture of the Rust runtime process that sits above the Dapr sidecar. It covers:

- Ractor supervision tree topology
- Flume channel layout (L1/L2/L3)
- Agent lifecycle state machine (hydrate/freeze protocol)
- Dapr SDK bindings (state, actor registry, workflow, pub/sub)
- Orchestrator-to-agent message format (serde types)
- Backpressure and concurrency limits
- Error boundaries and panic isolation

**What this document does NOT cover:**
- The Ray Serve / vLLM inference cluster (separate infrastructure track)
- Serialization format for inter-node wire protocol (quinn, #134)
- Cross-component integration contracts (chief-architect, #136)
- Frontend, CLI, API gateway, or web-app code (out of scope for this phase)

**RFC alignment:** All design decisions below are consistent with RFC §1.2 (Dapr as virtual actor substrate, ractor for in-process) and v3.0 §3.2 (Flume L1, tokio::broadcast L2, NATS L3). Where a tension arises between the RFC and this design, it is marked **[OPEN QUESTION]** and not silently resolved.

---

## 1. Ractor Supervision Tree

### 1.1 Overview

The supervision tree has three main branches: the **process supervisor** at the root, the **swarm supervisor** that owns all agent instances, and the **message router supervisor** that owns the channel infrastructure. These are separate branches so that a message router failure does not cascade to agents and vice versa.

```
ProcessSupervisor (root)
├── SwarmSupervisor
│   ├── AgentSupervisor[agent_id_0]
│   │   └── AgentActor[agent_id_0]     (ractor actor, in-process)
│   ├── AgentSupervisor[agent_id_1]
│   │   └── AgentActor[agent_id_1]
│   └── ... (up to ACTIVE_AGENT_LIMIT hydrated at once)
├── MessageRouterSupervisor
│   ├── L1RouterActor                  (Flume channel dispatcher)
│   ├── L2BroadcastActor               (tokio::broadcast fan-out)
│   └── L3NatsBridgeActor              (NATS async-nats client)
├── DaprActivationListenerActor        (HTTP server — Dapr calls IN here)
└── OrchestratorActor                  (Kimi K2.5 client, top-level planner)
```

### 1.2 Supervisor Strategies

Each supervisor level has a distinct restart strategy:

| Supervisor | Strategy | Max Restarts | Window | On Exhaustion |
|------------|----------|-------------|--------|---------------|
| `ProcessSupervisor` | One-for-one | 5 | 60s | Process exit (OS restart via systemd/Akash) |
| `SwarmSupervisor` | One-for-one | 20 | 60s | Freeze all agents, alert, wait for `ProcessSupervisor` |
| `AgentSupervisor[id]` | One-for-one | 3 | 30s | Quarantine agent (freeze to Dapr, remove from active registry, log FM-class failure) |
| `MessageRouterSupervisor` | All-for-one | 3 | 30s | All router actors restart together (channel topology must be consistent) |
| `DaprActivationListenerActor` | One-for-one | 10 | 60s | Alert + reject inbound Dapr activations with 503 |
| `OrchestratorActor` | One-for-one | 5 | 60s | Enter degraded mode (drain in-flight tasks, reject new tasks) |

**Rationale for all-for-one on `MessageRouterSupervisor`:** L1/L2/L3 share state (pending message counts, backpressure signals). If L3 (NATS bridge) crashes while L1 is mid-route, messages queued for L3 forwarding would be orphaned. Restarting all three together guarantees channel invariants.

### 1.3 Agent Isolation Contract

Each `AgentActor` runs inside its own `AgentSupervisor`. The contract:

1. **Panic boundary:** Every message handler in `AgentActor` is wrapped with `std::panic::catch_unwind`. A panicking handler produces a `AgentError::Panic(backtrace)` result rather than unwinding the ractor thread pool.
2. **Crash isolation:** An `AgentActor` crash restarts only that actor. Peer agents are unaffected — they continue processing via the `MessageRouter`.
3. **State safety on crash:** Before restart, the supervisor attempts an emergency freeze: serialize current agent state to Dapr. If the freeze itself panics, the Dapr state from the last successful freeze is used (last-good-checkpoint semantics).
4. **Quarantine:** After 3 restarts within 30s, the supervisor quarantines the agent: marks it `AgentLifecycleState::Quarantined` in the Dapr actor registry, increments the `agent_crash_total` Prometheus counter, and publishes a `AgentQuarantined` event to the L2 broadcast channel so the orchestrator can reroute pending tasks.

### 1.4 DaprActivationListenerActor — The Inversion Point

**[OPEN QUESTION OQ-RI-1]** RFC §1.2 states "Dapr replaces the durable state layer and cross-process activation." In Dapr's virtual actor model, Dapr does not wait for the Rust process to pull activation — **Dapr calls INTO the process** when a message arrives for a frozen actor. This requires the Rust runtime to expose an HTTP/gRPC endpoint that Dapr can invoke.

The `DaprActivationListenerActor` is a dedicated ractor actor that owns an `axum` HTTP server. When Dapr delivers an actor invocation, the listener:
1. Receives the `PUT /actors/{actorType}/{actorId}/method/{methodName}` call from Dapr
2. Checks if the agent is already active (consults `ActiveAgentRegistry`)
3. If frozen: sends a `HydrateRequest` to `SwarmSupervisor` (synchronous inside the runtime, async to the HTTP caller which awaits the response)
4. If active: forwards the message as a Flume L1 message to the already-running `AgentActor`
5. Returns the response body to Dapr

**Design decision (for OQ-RI-1):** This inversion means the Rust runtime is a Dapr actor host, not a Dapr actor client. The official [`dapr` Rust SDK](https://github.com/dapr/rust-sdk) provides an actor host abstraction. We should use it rather than hand-rolling the HTTP server. This needs confirmation from chief-architect (#136) on whether the SDK's actor host interface is compatible with the ractor supervision tree.

#### Hydration race serialization

**Guarantee relied upon (Option A — preferred):** Dapr's virtual actor model provides a single-threaded, turn-based execution guarantee per `(actorType, actorId)`. When Dapr delivers two concurrent invocations for the same `actorId`, Dapr serializes them at the placement layer — only one call enters the actor host handler at a time. The runtime relies on this guarantee as the primary serialization mechanism for concurrent activation events targeting the same agent. No additional per-agent mutex is required in Phase 0.

**Failure mode:** If Dapr placement loses actor state and re-activates the same agent concurrently on a different pod (split-brain scenario), both pods may independently issue a `HydrateRequest`. The backstop is ETag/`version` conflict during freeze (§4.2, FM-21): the second writer receives `DaprError::ETagMismatch`, re-reads the current ETag, and retries. This provides last-writer-wins semantics with no silent data loss.

**Option B (not adopted for Phase 0):** An explicit per-agent mutex keyed on `agent_id` in the `SwarmSupervisor` hydration path would provide belt-and-suspenders protection against Dapr placement bugs. Deferred to Phase 1 if placement instability is observed in load testing.

---

## 2. Flume Channel Layout (L1 / L2 / L3)

### 2.1 Channel Tiers (from v3.0 §3.2)

| Tier | Transport | Latency | Serialization | When |
|------|-----------|---------|--------------|------|
| **L1** | Flume MPMC (bounded) | ~80 ns | None — native Rust types | Agent A → Agent B, same process |
| **L2** | `tokio::broadcast` (bounded) | ~300 ns | None — cloned Arc<T> | Pub/sub fan-out within process |
| **L3** | NATS (`async-nats`) + prost | ~50–80 µs | Protobuf (prost) | Cross-machine, persistence, queue groups |

### 2.2 Channel Topology Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                    Runtime Process (single OS process)           │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │                    L1RouterActor                             │ │
│  │                                                              │ │
│  │   AgentActor[A] ──Flume(tx)──►  DispatchTable  ──Flume(rx)──► AgentActor[B]  │
│  │   AgentActor[B] ──Flume(tx)──►  DispatchTable  ──...        │ │
│  │                                                              │ │
│  │   Each AgentActor gets:                                      │ │
│  │     inbox_tx: flume::Sender<AgentMessage>                    │ │
│  │     inbox_rx: flume::Receiver<AgentMessage>                  │ │
│  │                                                              │ │
│  │   L1RouterActor holds:                                       │ │
│  │     dispatch_table: DashMap<AgentId, flume::Sender<AgentMessage>> │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │                  L2BroadcastActor                            │ │
│  │                                                              │ │
│  │   topics: HashMap<TopicId, tokio::broadcast::Sender<Arc<BroadcastEvent>>> │
│  │                                                              │ │
│  │   Agents subscribe via L2BroadcastActor.subscribe(topic_id) │ │
│  │   Returns: tokio::broadcast::Receiver<Arc<BroadcastEvent>>  │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │                  L3NatsBridgeActor                           │ │
│  │                                                              │ │
│  │   nats_client: async_nats::Client                            │ │
│  │   egress_tx: flume::Sender<OutboundNatsMessage>              │ │
│  │   ingress_rx: flume::Receiver<InboundNatsMessage>            │ │
│  │                                                              │ │
│  │   Translates:                                                │ │
│  │     AgentMessage (native) ←→ NatsEnvelope (prost-serialized) │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
└─────────────────────────────────────────────────────────────────┘
```

### 2.3 Channel Sizing and Backpressure

All Flume channels are **bounded**. Bounded channels provide natural backpressure: a sender that cannot make progress blocks (or receives `TrySendError::Full` for non-blocking sends).

| Channel | Default Capacity | Rationale |
|---------|----------------|-----------|
| `AgentActor` inbox | 256 messages | Enough for burst; back-pressure kicks in if agent is slow |
| `L1RouterActor` routing queue | 4,096 messages | Central dispatcher; higher capacity to absorb bursts from many agents |
| `L2BroadcastActor` topic channel | 1,024 per topic | `tokio::broadcast` requires fixed capacity at creation |
| `L3NatsBridgeActor` egress | 8,192 messages | NATS publish can have variable latency; larger buffer to absorb |
| `L3NatsBridgeActor` ingress | 8,192 messages | Symmetric |

**Backpressure propagation strategy:**
- L1: if the destination `AgentActor` inbox is full, the `L1RouterActor` applies send backpressure to the source agent's inbox (async send, awaited). The source agent pauses sending until capacity is available. This prevents unbounded queue growth.
- L2: `tokio::broadcast` drops old messages on overflow (lagged subscriber behavior). L2 is used for advisory/observability events only — message loss is acceptable.
- L3: NATS egress uses a bounded Flume buffer. If NATS is unavailable, the egress buffer fills, causing L3 sends to block. The `L3NatsBridgeActor` exposes a `backpressure_status()` method that the orchestrator polls to decide whether to delay dispatching new tasks that require cross-machine messaging.

**Active agent limit:** The runtime enforces `ACTIVE_AGENT_LIMIT` (default: 100 in Phase 1, raised toward 1,000 as Dapr hydration is validated). When the limit is reached, new activation requests queue in `SwarmSupervisor`. The LRU eviction policy (§4.4) determines which active agents are frozen to make room.

---

## 3. Agent Lifecycle State Machine

### 3.1 States

```
                  ┌──────────────────────────────────────────────┐
                  │              AgentLifecycleState              │
                  └──────────────────────────────────────────────┘

  ┌─────────┐   HydrateRequest    ┌───────────┐
  │ Frozen  │ ──────────────────► │ Hydrating │
  └─────────┘                     └─────┬─────┘
       ▲                                │ HydrationComplete
       │                                ▼
  ┌────────────┐  TaskComplete     ┌────────┐
  │  Freezing  │ ◄──────────────── │ Active │
  └────────────┘                   └───┬────┘
       │ FreezeComplete                │ IdleTimeout / EvictionNeeded
       ▼                               ▼
  ┌─────────┐         drain     ┌──────────┐
  │ Frozen  │                   │ Draining │
  └─────────┘ ◄──FreezeStart─── └──────────┘

  ┌─────────────┐  (separate terminal state)
  │ Quarantined │  ← entered after AgentSupervisor exhausts restart budget
  └─────────────┘
```

| State | Description | Dapr State | Ractor Actor |
|-------|-------------|-----------|-------------|
| `Frozen` | Not in memory. State persisted in Dapr store. | `FROZEN` | Not spawned |
| `Hydrating` | Dapr state being read; ractor actor being spawned | `HYDRATING` (transient) | Spawning |
| `Active` | Processing messages. Flume inbox active. | `ACTIVE` | Running |
| `Draining` | Finishing in-flight work before freeze | `DRAINING` (transient) | Running, inbox closed to new work |
| `Freezing` | Serializing state to Dapr; actor about to be dropped | `FREEZING` (transient) | Running, wrapping up |
| `Quarantined` | Crashed past restart budget; needs human/orchestrator intervention | `QUARANTINED` | Not spawned |

### 3.2 Lifecycle Transitions

**Frozen → Hydrating (triggered by):**
- Dapr activation call received by `DaprActivationListenerActor`
- Orchestrator dispatches a task to this agent while it is frozen (pre-hydration)

**Hydrating → Active:**
1. `SwarmSupervisor` reads agent snapshot from Dapr state store (key: `agent:{agent_id}:snapshot`)
2. Deserializes `AgentSnapshot` (see §5 for type definition)
3. Spawns `AgentActor` via ractor, passing deserialized state
4. Registers agent inbox in `L1RouterActor` dispatch table
5. Sets Dapr actor state to `ACTIVE`
6. Delivers queued messages (if any accumulated while `Hydrating`)

**HYDRATING timeout (`HYDRATION_TIMEOUT_SECS` = 15 s):**
`SwarmSupervisor` starts a 15-second deadline timer when a `HydrateRequest` is issued. This window covers p99 Dapr state read + snapshot deserialization + ractor actor spawn, and sits below the FM-22 "actor stuck hydrating" detection threshold. If the agent does not reach `Active` within this deadline:
1. The supervisor cancels the pending hydration future
2. Moves the agent to `QUARANTINED` state in the Dapr actor registry
3. Emits an FM-22 `HydrationTimeout` event on the L2 broadcast channel
4. Retries hydration up to **3× with exponential backoff** (delays: 1 s, 2 s, 4 s)
5. If all 3 retries fail, quarantine is permanent — requires explicit `RecoverAgent` command from the orchestrator

**Concurrent activation serialization:** Dapr's actor model serializes concurrent activation calls for the same `agent_id` (single-threaded, turn-based per `actorId`). No per-agent mutex is needed in Phase 0. See §1.4 for the full race prevention guarantee and the ETag-based failure-mode backstop.

**Active → Draining (triggered by):**
- Idle TTL expired (default 5 minutes since last message processed) — timer owned by `AgentActor`
- `SwarmSupervisor` LRU eviction: active agent count exceeds `ACTIVE_AGENT_LIMIT`
- Graceful shutdown signal (`SIGTERM` propagated to `SwarmSupervisor`)

**Draining → Freezing:**
- `AgentActor` finishes processing current message
- Closes its Flume inbox to new L1 messages (router updates dispatch table: removes agent)
- Emits `AgentDrainComplete` event on L2 broadcast

**Freezing → Frozen:**
1. `AgentActor` serializes current state to `AgentSnapshot`
2. Writes snapshot to Dapr state store with ETag (optimistic concurrency)
3. Sets Dapr actor state to `FROZEN`
4. `AgentSupervisor` drops the `AgentActor` (ractor actor stops)

**Active → Quarantined (crash path):**
- See §1.3: after 3 restart attempts within 30s, `AgentSupervisor` quarantines
- Emergency freeze attempted before quarantine (best-effort)
- Quarantine is sticky: requires explicit `RecoverAgent` command from orchestrator

### 3.3 Idle TTL and LRU Eviction

Every `AgentActor` starts an idle timer on its `tokio::time::Instant` when it finishes processing a message. The timer resets on each new message.

```
idle_ttl_secs: u64 = 300   // default: 5 minutes
```

**LRU eviction under memory pressure:**
- `SwarmSupervisor` maintains an LRU cache of active agents sorted by last-message timestamp
- High watermark: `ACTIVE_AGENT_LIMIT` (runtime config, default 100 initially)
- When active count reaches the watermark, the LRU agent (longest idle) is evicted (Draining → Freezing → Frozen) before activating the new agent
- Memory pressure signal: if process RSS exceeds `MEMORY_PRESSURE_THRESHOLD_MB` (default: 2GB), the watermark is temporarily lowered by 10% to accelerate eviction

---

## 4. Hydrate/Freeze Protocol

### 4.1 What Is Durable vs Transient

The freeze/hydrate protocol distinguishes between durable state (survives freeze) and transient state (rebuilt on hydration).

| State Component | Durable? | Storage | Notes |
|----------------|---------|---------|-------|
| Agent identity (`agent_id`, `persona`, `capabilities`) | Yes | Dapr state store | Set at agent creation, rarely changes |
| Working memory (current task context, scratchpad) | Yes | Dapr state store | Serialized in `AgentSnapshot.working_memory` |
| Pending task queue | Yes | Dapr state store | Tasks survive freeze; re-enqueued on hydration |
| Conversation history (token window) | Yes | Dapr state store | Bounded by `MAX_CONTEXT_TOKENS` |
| LanceDB memory embeddings | Yes | LanceDB (external) | Reference stored; LanceDB owns persistence |
| Flume inbox (in-flight messages) | No | Discarded on freeze | Messages already in inbox are drained first (Draining state) |
| Active tool handles | No | Discarded | Tools are re-acquired on hydration if pending work requires them |
| tokio task handles | No | Dropped on freeze | Any async subtasks must complete before Draining exits |
| L2 broadcast subscriptions | No | Re-subscribed on hydration | `AgentActor::hydrate()` re-calls `L2BroadcastActor.subscribe()` |

### 4.2 Freeze Snapshot Format

**[COORDINATION WITH QUINN #134]** The `AgentSnapshot` struct is the serialization boundary between this runtime design and the serialization format design (quinn, #134). The field names and types below represent the logical schema. Quinn's track owns the wire encoding (likely JSON via `serde_json` or Protobuf via `prost`). The field structure must be agreed before Phase 1 implementation.

```rust
// Logical schema — see crates/af-swarm-runtime/src/agent.rs for trait definitions
struct AgentSnapshot {
    // Identity
    agent_id: AgentId,
    persona: AgentPersona,
    capabilities: Vec<Capability>,

    // State
    lifecycle_state: AgentLifecycleState,
    last_active_at: chrono::DateTime<chrono::Utc>,

    // Task context
    pending_tasks: Vec<PendingTask>,
    working_memory: WorkingMemory,

    // Context window
    conversation_history: Vec<ConversationTurn>,
    context_token_count: u32,

    // Configuration
    config: AgentConfig,

    // Serialization metadata
    snapshot_version: u32,    // for forward-compat deserialization
    created_at: chrono::DateTime<chrono::Utc>,
}
```

**ETag / optimistic concurrency:** Dapr state writes use ETags to prevent lost updates when two concurrent hydration events race for the same agent. If an ETag conflict occurs on freeze, the supervisor retries with the current ETag (re-reads from Dapr, merges working memory, retries write).

### 4.3 Dapr State Keys

All agent state is stored under a deterministic key scheme in the Dapr state store:

| Key | Value | Notes |
|-----|-------|-------|
| `agent:{agent_id}:snapshot` | `AgentSnapshot` (serialized) | Main durable state |
| `agent:{agent_id}:lifecycle` | `AgentLifecycleState` (string enum) | Fast lookup without deserializing full snapshot |
| `agent:{agent_id}:last_active` | ISO-8601 timestamp | For LRU ordering without loading snapshots |
| `swarm:active_agents` | `Vec<AgentId>` (JSON array) | Registry of currently active agents |
| `swarm:frozen_agents` | `Vec<AgentId>` (JSON array) | Registry of frozen (but not quarantined) agents |

---

## 5. Dapr SDK Bindings

### 5.1 SDK Choice

We use the official [dapr Rust SDK](https://github.com/dapr/rust-sdk) (`dapr` crate, currently 0.x). The SDK provides:
- State store client (read/write/delete with ETag support)
- Actor host registration (exposes the HTTP endpoint Dapr calls)
- Pub/sub client (backed by NATS in our configuration)
- Service invocation (gRPC via tonic)

**[OPEN QUESTION OQ-RI-2]** The `dapr` Rust SDK is pre-1.0 as of Apr 2026. Evaluate if the actor host abstraction is sufficient for our activation listener (§1.4), or if we need to hand-roll the Dapr actor HTTP API endpoint. Hand-rolling is ~200 LOC but removes SDK coupling risk. Decision needed before Phase 1 starts.

### 5.2 State Store Operations

The runtime wraps the Dapr SDK into a `StateClient` trait (renamed from `DaprStateClient` in PR #145) that all components use. Key operations:

```
StateClient::read_agent_snapshot(agent_id) -> Result<(AgentSnapshot, ETag)>
StateClient::write_agent_snapshot(agent_id, snapshot, etag) -> Result<ETag>
StateClient::read_lifecycle_state(agent_id) -> Result<AgentLifecycleState>
StateClient::write_lifecycle_state(agent_id, state, etag) -> Result<ETag>
StateClient::list_active_agents() -> Result<Vec<AgentId>>
StateClient::register_active(agent_id) -> Result<()>
StateClient::deregister_active(agent_id) -> Result<()>
```

All writes use ETags. All reads return the ETag for use in subsequent writes.

### 5.3 Pub/Sub Bindings (L3 → Dapr Pub/Sub)

The `L3NatsBridgeActor` bridges Flume L3 messages to Dapr pub/sub, which is backed by NATS in our deployment. This means L3 messages go through two hops: Flume (in-process) → Dapr SDK → NATS. The Dapr pub/sub layer adds:
- Message delivery guarantees (at-least-once)
- Topic subscriptions managed by Dapr (not hardcoded NATS subjects)
- Tracing spans on each publish/subscribe

**Dapr pub/sub topics (runtime-owned):**

| Topic | Publisher | Subscriber | Message Type |
|-------|-----------|-----------|-------------|
| `agent.task.dispatch` | Orchestrator | Agents (by capability filter) | `TaskDispatch` |
| `agent.task.result` | Agents | Orchestrator | `TaskResult` |
| `agent.lifecycle.changed` | `SwarmSupervisor` | Orchestrator, Observability | `LifecycleEvent` |
| `agent.message.direct` | Any agent | Specific agent (filtered by `agent_id`) | `AgentMessage` |
| `swarm.orchestrator.command` | External / Orchestrator | `OrchestratorActor` | `OrchestratorCommand` |

### 5.4 Workflow Invocation

Dapr workflows are used for multi-step agent task sequences that must survive process restarts (e.g., a task that involves multiple inference calls with state checkpointing between calls).

The `OrchestratorActor` calls the Dapr workflow API to start/query/terminate workflows:

```
DaprWorkflowClient::start_workflow(workflow_name, instance_id, input) -> Result<WorkflowInstanceId>
DaprWorkflowClient::get_status(instance_id) -> Result<WorkflowStatus>
DaprWorkflowClient::terminate_workflow(instance_id) -> Result<()>
```

Workflow definitions live outside this runtime (they are Dapr workflow YAML or code-based, depending on the Dapr SDK). The runtime only calls the workflow API; it does not define workflows.

---

## 6. Orchestrator-to-Agent Message Format

### 6.1 Message Types

The message types below are the **native Rust types** used on the L1 Flume path. They are the source of truth. L3 serialization (prost/JSON) is a projection of these types — see quinn (#134) for the wire format.

**[COORDINATION WITH CHIEF-ARCHITECT #136]** The `AgentId`, `TaskId`, and `MessageEnvelope` newtypes below should be reviewed by chief-architect for cross-component compatibility. If the integration design requires these types in a shared crate, chief-architect should move them. Until then they live in `crates/af-swarm-runtime/src/agent.rs`.

```
AgentMessage (enum — all messages an AgentActor can receive):
  ├── Task(TaskDispatch)
  ├── DirectMessage(DirectMessage)
  ├── BroadcastEvent(Arc<BroadcastEvent>)     // from L2, forwarded to actor
  ├── LifecycleCommand(LifecycleCommand)      // Drain, Freeze, Quarantine
  └── Ping                                    // health check, returns Pong

TaskDispatch:
  ├── task_id: TaskId
  ├── workflow_instance_id: Option<WorkflowInstanceId>
  ├── payload: TaskPayload
  ├── priority: TaskPriority           // High / Normal / Low
  ├── deadline: Option<Instant>
  ├── inference_tier: InferenceTier    // Open / Restricted (maps to DSE tier)
  ├── max_tokens: Option<u32>
  └── requester_id: Option<AgentId>   // which agent/orchestrator sent this

DirectMessage:
  ├── from: AgentId
  ├── to: AgentId
  ├── correlation_id: Uuid
  └── payload: serde_json::Value      // flexible; structure defined per-protocol

LifecycleCommand (enum):
  ├── Drain
  ├── Freeze
  ├── Quarantine { reason: String }
  └── Recover
```

### 6.2 Backpressure Signal Format

The `OrchestratorActor` checks backpressure before dispatching tasks:

```
BackpressureStatus:
  ├── active_agent_count: usize
  ├── active_agent_limit: usize
  ├── l3_egress_buffer_fill_pct: f32      // 0.0 – 1.0
  ├── pending_hydrations: usize
  └── memory_pressure: MemoryPressureLevel  // None / Low / High / Critical
```

**Backpressure thresholds (named constants in `agent.rs`):**

| Constant | Value | Condition | Orchestrator action |
|----------|-------|-----------|---------------------|
| `THROTTLE_AGENT_RATIO` | 0.80 | `active_agent_count > 0.80 × active_agent_limit` (default: > 80) | Throttle: slow new task dispatch |
| `THROTTLE_L3_FILL` | 0.85 | `l3_egress_buffer_fill_pct > 0.85` | Throttle: slow new task dispatch |
| `CRITICAL_AGENT_RATIO` | 0.95 | `active_agent_count > 0.95 × active_agent_limit` (default: > 95) | Critical: pause dispatch entirely |
| `CRITICAL_L3_FILL` | 0.95 | `l3_egress_buffer_fill_pct > 0.95` | Critical: pause dispatch entirely |

`BackpressureStatus::should_throttle()` returns `true` if either throttle condition is met. `BackpressureStatus::is_critical()` returns `true` if either critical condition is met. The orchestrator checks `is_critical()` first; if false, it checks `should_throttle()`.

If `memory_pressure == Critical` or `l3_egress_buffer_fill_pct > 0.9`, the orchestrator applies a configurable dispatch rate limit (default: halve dispatch rate, no new hydrations until pressure drops).

**L1 inbox fairness (FIFO, Phase 0):**
Each agent's L1 Flume inbox is a plain FIFO queue — there are no priority classes within the per-agent inbox in Phase 0. Task priority (`TaskPriority::High / Normal / Low`) is used by the orchestrator for *dispatch ordering* before messages enter the inbox, not for intra-inbox reordering. If a single slow agent starves its `AgentSupervisor`'s message loop, the supervisor detects the stall and restarts the agent (FM-11 tie-in). Starvation is therefore a **restart concern, not a scheduling concern** in this design. Priority lanes within the L1 inbox are deferred to Phase 2.

---

## 7. Error Boundaries and Panic Isolation

### 7.1 Panic Boundary Layers

Three layers of panic containment, innermost to outermost:

**Layer 1: Message handler boundary (`std::panic::catch_unwind`)**
Each ractor `handle()` implementation calls the business logic inside `catch_unwind`. A panic in the business logic becomes `AgentError::Panic { backtrace }` and is logged + counted in Prometheus. The actor remains alive and processes the next message.

**Layer 2: Actor crash (`AgentSupervisor` restart)**
If a panic escapes the message handler (e.g., during actor initialization), the ractor actor crashes and the `AgentSupervisor` applies its restart strategy (§1.2). Up to 3 restarts; then quarantine.

**Layer 3: Process supervisor (`ProcessSupervisor`)**
If a supervisor itself crashes (bug in supervision code), `ProcessSupervisor` restarts it. If `ProcessSupervisor` exhausts its budget (5 crashes in 60s), the process exits. Systemd/Akash restarts the process. Dapr state survives (Dapr sidecar is separate process).

### 7.2 Fault Modes (MAST Extension)

The v3.0 plan specifies 18 MAST failure modes (FM-1 through FM-18). The RFC adds FM-19 and FM-20. This runtime design adds two more:

| FM | Name | Description | Detection | Recovery |
|----|------|-------------|-----------|----------|
| FM-21 | Dapr state write conflict | ETag mismatch on agent freeze | `StateClient` returns `ETagMismatch` | Re-read ETag, merge, retry (max 3 attempts) |
| FM-22 | Actor stuck hydrating | Agent remains in `HYDRATING` state past `HYDRATION_TIMEOUT_SECS` (15 s), **or** `AgentSnapshot` fails to deserialize (schema mismatch / corruption) | `SwarmSupervisor` hydration deadline timer fires; or `serde` deserialization error | Move agent to `QUARANTINED`; emit FM-22 event on L2 broadcast; retry hydration max 3× with exponential backoff (1 s / 2 s / 4 s). `snapshot_version` field (§4.2) enables migration before deserialization fails. Permanent quarantine after 3 retries requires `RecoverAgent` command. |

**FM-22 mitigation:** `snapshot_version` field (§4.2) allows the runtime to detect schema version mismatches and apply a migration function before deserialization fails. Migration functions are registered in a `SnapshotMigrationRegistry` keyed by `(from_version, to_version)`.

### 7.3 Observability

All error/crash events emit to Prometheus and the L2 broadcast channel:

| Counter/Gauge | Labels | Description |
|--------------|--------|-------------|
| `agent_panic_total` | `agent_id`, `persona` | Panics caught at message handler boundary |
| `agent_crash_total` | `agent_id`, `persona` | Actor crashes (escaped panics) |
| `agent_quarantined_total` | `agent_id`, `persona` | Agents quarantined (restart budget exhausted) |
| `agent_hydration_duration_ms` | `agent_id`, `persona` | Histogram: time from HydrateRequest to Active |
| `agent_freeze_duration_ms` | `agent_id`, `persona` | Histogram: time from DrainComplete to Frozen |
| `active_agent_count` | — | Gauge: currently hydrated agents |
| `frozen_agent_count` | — | Gauge: agents in Dapr store, not hydrated |
| `l3_backpressure_events_total` | — | Counter: times L3 egress buffer exceeded 90% |

---

## 8. Concurrency Limits Summary

| Parameter | Default | Config Key | Notes |
|-----------|---------|-----------|-------|
| `ACTIVE_AGENT_LIMIT` | 100 | `runtime.active_agent_limit` | Target 1,000 after Dapr validation; start at 100 |
| `AGENT_INBOX_CAPACITY` | 256 | `runtime.agent_inbox_capacity` | Per-agent Flume inbox |
| `L1_ROUTER_CAPACITY` | 4,096 | `runtime.l1_router_capacity` | Dispatch table queue |
| `L3_EGRESS_CAPACITY` | 8,192 | `runtime.l3_egress_capacity` | NATS egress Flume buffer |
| `AGENT_IDLE_TTL_SECS` | 300 | `runtime.agent_idle_ttl_secs` | 5 minutes |
| `MEMORY_PRESSURE_THRESHOLD_MB` | 2,048 | `runtime.memory_pressure_threshold_mb` | RSS threshold for emergency eviction |
| `MAX_CONCURRENT_HYDRATIONS` | 16 | `runtime.max_concurrent_hydrations` | Parallel Dapr reads during burst |
| `MAX_TOOLS_PER_AGENT` | 16 | `agent.max_tools` | Per RFC §1.4 / v3.0 §3.4 (Google/MIT study cap) |
| `AGENT_RESTART_MAX` | 3 | `runtime.agent_restart_max` | Per AgentSupervisor |
| `AGENT_RESTART_WINDOW_SECS` | 30 | `runtime.agent_restart_window_secs` | Window for restart budget |

---

## 9. Open Questions

These are design tensions or decisions that require input from other tracks or human decision. They must NOT be silently resolved in implementation.

| # | Question | Context | Owner |
|---|----------|---------|-------|
| **OQ-RI-1** | Dapr actor host inversion: Rust runtime must expose HTTP endpoint for Dapr to call. Does the `dapr` Rust SDK actor host abstraction compose with ractor supervision? Or do we hand-roll the activation listener? | §1.4 — affects supervisor tree structure | chief-architect (#136) |
| **OQ-RI-2** | `dapr` Rust SDK pre-1.0 stability. Use it or hand-roll? | §5.1 | chief-architect (#136) |
| **OQ-RI-3** | Freeze snapshot serialization format: `serde_json` vs `prost` vs `bincode`? | §4.2 — must match quinn (#134) serialization design | quinn (#134) |
| **OQ-RI-4** | `AgentId`, `TaskId`, `MessageEnvelope` as shared crate types vs. local newtype in `af-swarm-runtime`? | §6.1 — cross-component type sharing | chief-architect (#136) |
| **OQ-RI-5** | `MAX_CONCURRENT_HYDRATIONS` = 16: is this the right default for Dapr state reads? Dapr state read latency under load needs empirical measurement. | §8 | Phase 1 load testing |
| **OQ-RI-6** | Emergency freeze on crash (§1.3): if the freeze itself panics, we fall back to last-good-checkpoint. Is last-good-checkpoint semantics acceptable for task state, or does the orchestrator need to be notified of potentially lost task progress? | §3.2 crash path | Orchestrator design (quinn / chief-architect) |

---

## 10. References

- [RFC 001: 1000-Agent Amendment](../reviews/1000-agent-amendment.md) — §1.2 (Dapr virtual actors), §1.4 (topology), §3.2 (v3.0 channel tiers preserved)
- [rust-swarm-runtime.md v3.0](../rust-swarm-runtime.md) — §3.2 (Flume/NATS tiers), §5.4 (FHE), MAST failure taxonomy
- [Dapr Rust SDK](https://github.com/dapr/rust-sdk) — actor host, state store, pub/sub bindings
- [ractor crate](https://github.com/slawlor/ractor) — Rust actor framework used for in-process supervision
- [Flume](https://github.com/zesterer/flume) — zero-unsafe MPMC bounded channels
- Issue #131 — this track
- Issue #134 — quinn, serialization format (coordination: §4.2, §6.1)
- Issue #136 — chief-architect, cross-component types (coordination: §1.4, §5.1, §6.1)
- Issue #144 — OQ-9 tiered state architecture (Turso L1 + SurrealDB L2)

---

## Addendum: OQ-9 Tiered State Architecture

_Added 2026-04-20 — responding to issue #144 (OQ-9: Turso L1 + SurrealDB L2 replacing Redis/PostgreSQL/LanceDB)._

### Impact Summary

**Severity: Minor.** The runtime internals design is largely unaffected because the `DaprStateClient` trait was always an abstraction boundary. Swapping the concrete implementation behind that boundary — from Dapr state store (Redis/Postgres) to Turso embedded replica — does not change any supervision tree logic, channel topology, hydrate/freeze state machine, or message format.

Two concrete changes are required:

1. **Rename `DaprStateClient` → `StateClient`** in `crates/af-swarm-runtime/src/agent.rs`. The trait's method signatures are unchanged. The associated error type `DaprError` is renamed `StateError` (see §A.2 below).
2. **Confirm `AgentSnapshot` works with SQLite storage.** It does — see §A.3 below.

### A.1 What Changes in the Runtime Design

| Design element | Before (RFC) | After (OQ-9) | Change required |
|---|---|---|---|
| `DaprStateClient` trait | Backed by Dapr state store → Redis/Postgres via sidecar | Backed by Turso embedded replica (local SQLite file) | Rename trait to `StateClient`. Swap concrete impl. Trait shape unchanged. |
| Hydration read path | `dapr.GetState()` over gRPC sidecar → ~500µs–2ms | `rusqlite` / `libsql` local file read → ~1–10µs | Concrete impl change only. `Hydrate` trait method unchanged. |
| Freeze write path | `dapr.SaveState()` with ETag optimistic concurrency | Turso UPSERT with rowid-based conflict detection | ETag type replaced — see §A.2. |
| `MAX_CONCURRENT_HYDRATIONS` limit (§8) | Set to 16 to avoid Dapr/Redis connection pool contention | Can be relaxed — local file reads have zero contention | Config default may increase in Phase 1 load testing; OQ-RI-5 updated below. |
| FM-21 (Dapr state write conflict) | ETag mismatch on Dapr SaveState | SQLite write conflict on UPSERT | FM-21 description updated — see §A.4. |
| FM-22 (Hydration deserialization failure) | Unchanged | Unchanged | No change. |
| Dapr pub/sub, workflows | Unchanged | Unchanged | Dapr sidecar retained for pub/sub and workflows per OQ-9 spec. |

### A.2 Trait Rename: `DaprStateClient` → `StateClient`

The trait is renamed. The method signatures are preserved exactly — callers throughout the supervision tree (`AgentActor`, `AgentSupervisor`, test mocks) require no change beyond updating the type name.

```rust
/// Interface for reading and writing agent state.
///
/// OQ-9 (issue #144): concrete implementation uses Turso embedded replica
/// (libSQL local file). Previously named `DaprStateClient` when Dapr state
/// store was the backing store. Mock implementation unchanged.
pub trait StateClient: Send + Sync + 'static {
    fn read_agent_snapshot(
        &self,
        agent_id: &AgentId,
    ) -> impl std::future::Future<Output = Result<(AgentSnapshot, ETag), StateError>> + Send;

    fn write_agent_snapshot(
        &self,
        agent_id: &AgentId,
        snapshot: &AgentSnapshot,
        etag: &ETag,
    ) -> impl std::future::Future<Output = Result<ETag, StateError>> + Send;

    fn read_lifecycle_state(
        &self,
        agent_id: &AgentId,
    ) -> impl std::future::Future<Output = Result<AgentLifecycleState, StateError>> + Send;

    fn write_lifecycle_state(
        &self,
        agent_id: &AgentId,
        state: AgentLifecycleState,
        etag: &ETag,
    ) -> impl std::future::Future<Output = Result<ETag, StateError>> + Send;

    fn list_active_agents(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<AgentId>, StateError>> + Send;

    fn register_active(
        &self,
        agent_id: &AgentId,
    ) -> impl std::future::Future<Output = Result<(), StateError>> + Send;

    fn deregister_active(
        &self,
        agent_id: &AgentId,
    ) -> impl std::future::Future<Output = Result<(), StateError>> + Send;
}

/// Opaque row version token used for optimistic concurrency on state writes.
///
/// OQ-9: with Turso, ETag wraps a SQLite rowid or last_write_millis value
/// rather than a Dapr-provided string. The opaque wrapper is intentional —
/// callers must not inspect or construct ETags directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ETag(pub String);

/// Errors from the StateClient (renamed from DaprError in OQ-9).
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("agent snapshot not found: {agent_id}")]
    NotFound { agent_id: AgentId },
    #[error("write conflict (ETag mismatch): {agent_id}")]
    ETagMismatch { agent_id: AgentId },
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("storage error: {0}")]
    Storage(String),
}
```

> **Coordination note:** `DaprError` in `agent.rs` is renamed `StateError` in the same commit. `chief-architect (#136)` must update any integration contract references to `DaprError`.

### A.3 `AgentSnapshot` Compatibility with SQLite Storage

`AgentSnapshot` is `#[derive(Serialize, Deserialize)]` with all fields serializable to standard Rust primitives and `serde_json`-compatible types (`String`, `Vec<T>`, `DateTime<Utc>`, enums). No embedded raw pointers or non-serializable fields exist.

**SQLite storage approach (for Turso concrete implementation):**

The snapshot is stored as a single `BLOB` column (JSON-encoded via `serde_json`) keyed by `agent_id`. This matches the read-by-ID access pattern on hydration:

```sql
CREATE TABLE agent_snapshots (
    agent_id    TEXT PRIMARY KEY,
    snapshot    BLOB NOT NULL,       -- serde_json::to_vec(&AgentSnapshot)
    etag        TEXT NOT NULL,       -- rowid or millis-based version token
    updated_at  INTEGER NOT NULL     -- Unix millis
);
```

- `snapshot_version` field already present in `AgentSnapshot` supports forward-compatible migration (§4.2 of original doc — `SnapshotMigrationRegistry` unchanged).
- `bincode` encoding is viable as an optimization if JSON proves too slow at 1,000 concurrent freeze/hydrate cycles. Coordinate with quinn (#134) on final encoding choice (OQ-RI-3 still open).
- No schema change to `AgentSnapshot` fields is required for SQLite storage.

**Conclusion: `AgentSnapshot` is SQLite-compatible with zero field changes.**

### A.4 Updated Fault Modes

| FM | Name | Updated description |
|----|------|---------------------|
| FM-21 | State write conflict | **Updated:** SQLite UPSERT conflict when two concurrent freeze operations attempt to update the same agent's row. `StateClient` returns `StateError::ETagMismatch`. Recovery: re-read rowid, retry (max 3 attempts). Note: with per-node local SQLite, two concurrent writes to the same agent are only possible within a single node (not cross-node), making this substantially less likely than with shared Redis. |
| FM-22 | Hydration deserialization failure | Unchanged. `snapshot_version` migration path unaffected by storage backend change. |

### A.5 Updated Open Questions

| # | Question | Update |
|---|----------|--------|
| **OQ-RI-5** | `MAX_CONCURRENT_HYDRATIONS` default | **Revised:** With Turso embedded replica, local file reads have zero connection pool contention. The limit of 16 was conservative due to Dapr/Redis pool saturation risk. Can be raised significantly (e.g., 128 or uncapped) pending Phase 1 benchmarks. qa-engineer (#135) should add a Turso concurrent-hydration benchmark per issue #144. |
| **OQ-RI-2** | `dapr` Rust SDK stability | **Partially resolved:** Dapr SDK is no longer used for state read/write. It is retained only for pub/sub and workflow bindings, which reduces the surface area of pre-1.0 SDK risk. chief-architect (#136) to confirm remaining Dapr SDK usage scope. |

### A.6 What Is Unchanged

The following design elements are **unaffected** by OQ-9 and require no revision:

- Ractor supervision tree topology (§1)
- Flume channel topology L1/L2/L3 (§2)
- Agent lifecycle state machine (§3)
- `Agent`, `Hydrate`, `Freeze`, `MessageHandler` trait shapes (§4)
- Dapr pub/sub and workflow bindings (§5 — state store bypassed, pub/sub and workflows unchanged)
- Message format and shared types (§6)
- Error boundaries and panic isolation (§7)
- MAST FM-1 through FM-20 (§7.2 — only FM-21 updated above)
- Observability metrics (§7.3)
- OQ-RI-1, OQ-RI-3, OQ-RI-4, OQ-RI-6 (open questions not affected by state backend change)
