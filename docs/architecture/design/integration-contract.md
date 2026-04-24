# AF Swarm Runtime — Integration Contract

**Document:** `docs/architecture/design/integration-contract.md`
**Track:** Integration Contract (#136)
**Author:** Chief Architect
**Date:** 2026-04-16
**Status:** DRAFT — sibling PRs #137 (Lain/runtime internals) and #138 (Argus/security) landed; #132 (atlas/inference), #130 (senku/state), #134 (quinn/serialization) pending. PR #141 merge-blockers resolved 2026-04-21 per CEO review.
**Issue:** [alternatefutures/admin#136](https://github.com/alternatefutures/admin/issues/136)
**Epic:** [#129](https://github.com/alternatefutures/admin/issues/129) — 1000-Agent Rust Swarm Runtime
**RFC Ground Truth:** [1000-agent-amendment.md](../reviews/1000-agent-amendment.md) (PR #128)
**v3.0 Plan:** [rust-swarm-runtime.md](../rust-swarm-runtime.md) — DO NOT MODIFY
**Reviewers:** wonderwomancode, mavisakalyan

---

## 0. Purpose and Scope

This document is the integration layer that makes the six parallel design tracks cohere into one system. It is the final merge before implementation begins. It does NOT rewrite the RFC or the v3.0 plan — it cross-links, reconciles, and surfaces conflicts.

**Scope:**
- Canonical glossary of overloaded terms
- Component dependency graph and topological implementation order
- Cross-component gRPC contracts (proto skeletons under `proto/af-swarm/`)
- Cross-cutting concerns: OTel tracing, versioning, error codes, timeouts
- Conflict-resolution log: wherever two sibling designs disagree, the decision is here
- Phase-to-component map: RFC phase plan with owning design docs

**Sibling design tracks (all landing in parallel):**

| Track | Issue | PR | Author | Status |
|-------|-------|-----|--------|--------|
| Rust runtime internals (ractor, Flume, hydrate/freeze) | #131 | #137 | Lain | OPEN |
| Security (FHE boundary, RESTRICTED POD, TEE.fail) | #133 | #138 | Argus | OPEN |
| Inference cluster (Ray Serve, vLLM, KV cache) | #132 | pending | Atlas | NOT YET OPEN |
| Serialization formats (prost wire, AgentSnapshot) | #134 | pending | Quinn | NOT YET OPEN |
| Dapr state / Rust SDK integration | #130 | pending | Senku | NOT YET OPEN |
| QA / load tests | #135 | pending | QA Engineer | NOT YET OPEN |

---

## 1. Canonical Glossary

The following terms are overloaded across the RFC, v3.0 plan, and sibling design docs. This glossary is the authoritative disambiguation. All sibling docs should defer to these definitions when they conflict.

### 1.1 Agent vs Actor

| Term | Canonical Definition | NEVER means |
|------|---------------------|-------------|
| **agent** | An addressable identity that can receive tasks and produce results. Described in TOML config. Can be frozen (no memory footprint) or hydrated (resident ractor actor). | A resident OS process or thread (that was v3.0 pre-RFC). |
| **ractor actor** | A Rust object managed by the `ractor` framework running within the Rust runtime process. An agent is represented as a ractor actor *only while it is active (hydrated)*. When the agent freezes, its ractor actor is garbage-collected. | The Dapr virtual actor (a separate concept; see below). |
| **Dapr virtual actor** | The Dapr framework's representation of an addressable entity with durable state. An agent's identity in Dapr. NOT a ractor actor — it is a registry entry + state store record that the Dapr sidecar manages. | An in-process ractor actor. |

**Key rule:** An agent is *both* a Dapr virtual actor (identity + durable state) and, when hydrated, a ractor actor (in-process execution). They are the same agent in two different views. When someone says "the agent is frozen," it means the ractor actor does not exist but the Dapr virtual actor identity does.

### 1.2 Orchestrator (three meanings)

| Term | Canonical Definition |
|------|---------------------|
| **Kimi K2.5 model** | The open-source LLM trained with PARL for parallel task planning. Runs in the shared inference cluster (Ray Serve + vLLM). This is a model, not a Rust process. |
| **OrchestratorActor** | The ractor actor inside the Rust runtime process that calls the Kimi K2.5 model. Owns the DAG planner, task dispatch logic, and topology enforcement. One per runtime node. |
| **orchestration** (as a verb) | The act of decomposing a task into a parallel sub-agent work graph and dispatching it. Done by the OrchestratorActor using the Kimi K2.5 model. |

**Disambiguation rule:** When a doc says "the orchestrator does X," check whether it means the OrchestratorActor (Rust process) or the Kimi K2.5 model (inference cluster). These have different failure modes and latency profiles. This contract always qualifies: "OrchestratorActor" or "Kimi K2.5."

### 1.3 Tier (two orthogonal dimensions)

**Hardware trust tier** (infrastructure security level):

| Tier | Meaning | Inference pool |
|------|---------|---------------|
| **Tier A** | Controlled hardware (AWS/GCP/colo). Highest TEE value. | RESTRICTED POD |
| **Tier B** | Audited DePIN (Phala Proof-of-Cloud). Moderate TEE. | Not assigned to a pool by default |
| **Tier C** | Unaudited DePIN (Akash). Low TEE value. | OPEN POOL |

**DSE sensitivity tier** (data classification level):

| Level | Meaning | Min transport |
|-------|---------|--------------|
| **PUBLIC** | Safe for external exposure | TLS 1.3 |
| **INTERNAL** | Internal use only | TLS 1.3 |
| **SENSITIVE** | PII, credentials, financial | mTLS 1.3 |
| **RESTRICTED** | Cryptographic keys, raw secrets, regulated data | mTLS 1.3 + FHE mandatory |

**CRITICAL disambiguation:** The word "RESTRICTED" alone is ambiguous. When referring to data, it means the DSE sensitivity level `RESTRICTED`. When referring to the inference cluster partition, it means the `RESTRICTED POD` (Tier A hardware). Always write "RESTRICTED data" or "RESTRICTED sensitivity level" for the DSE meaning, and "RESTRICTED POD" or "RESTRICTED POD (Tier A)" for the infrastructure meaning.

**Rule:** The DSE `RESTRICTED` sensitivity level routes data to the `RESTRICTED POD` (Tier A). This coupling is intentional but the two concepts must not be conflated.

### 1.4 Other Overloaded Terms

| Term | Canonical Definition |
|------|---------------------|
| **sub-agent** | An agent that receives a delegated task from the OrchestratorActor. Functionally identical to any other agent — "sub-agent" is a role, not a type. |
| **tenant** | An organization or user account. The unit of billing isolation, key namespace, and DSE taint boundary. A tenant owns one or more agents. |
| **task** | A discrete unit of work: a prompt + context + metadata dispatched to an agent. Identified by `TaskId` (UUID v7). |
| **workflow** | A Dapr Workflow instance. A sequence of tasks with durable state that survives process restarts. Workflows may span multiple agent hydration/freeze cycles. Not the same as a "task." |
| **hydrate** | Load an agent's durable state from the Dapr state store and spawn a ractor actor for it. The agent transitions from `Frozen → Hydrating → Active`. |
| **freeze** | Serialize an agent's durable state to the Dapr state store and garbage-collect its ractor actor. The agent transitions from `Active → Draining → Freezing → Frozen`. |
| **ACTIVE_AGENT_LIMIT** | The maximum number of simultaneously hydrated agents (ractor actors resident in memory) per runtime node. Default 100; target 1,000. Separate from the number of addressable agents (unlimited via Dapr virtual actors). |
| **effective tier** | The DSE sensitivity level computed for a specific task, considering agent defaults, tool manifests, runtime scanner, and taint propagation. Determines which inference pool the task routes to. |
| **taint propagation** | The monotonic escalation of effective tier: once a task is tainted to RESTRICTED, it cannot be declassified within that task graph without an explicit DeclassifyGate (Argus §2.3.2). |
| **inference request** | An HTTP/gRPC call from the OrchestratorActor or an AgentActor to the shared inference cluster. Contains: prompt, model ID, tier claim, OTel trace context, cost attribution metadata. |

---

## 2. Component Dependency Graph

### 2.1 Directed Dependency Graph

```
                            ┌─────────────────────┐
                            │  af-swarm-types     │ ◄── shared crate (see §5.1)
                            │  (AgentId, TaskId,  │
                            │   SensitivityLevel, │
                            │   TraceContext, etc.)│
                            └──────────┬──────────┘
                                       │ used by all components below
          ┌────────────────────────────┼────────────────────────────────┐
          ▼                            ▼                                ▼
┌─────────────────┐         ┌──────────────────┐           ┌───────────────────┐
│ DSE / Taint     │         │ Dapr sidecar     │           │ Flume topology    │
│ Engine          │         │ (infrastructure) │           │ (L1/L2/L3)        │
│ (v3.0 §5.3)    │         │ No code dep —    │           │ (Lain #137 §2)    │
│ (Argus #138 §2) │         │ sidecar process  │           │                   │
└────────┬────────┘         └────────┬─────────┘           └────────┬──────────┘
         │                           │                               │
         │             ┌─────────────┘                               │
         │             ▼                                              │
         │   ┌──────────────────────┐                                │
         │   │ Agent lifecycle      │ ◄──────────────────────────────┘
         │   │ (hydrate/freeze,     │
         │   │  state machine)      │
         │   │ (Lain #137 §3–4)     │
         │   │ (Senku #130)         │
         │   └──────────┬───────────┘
         │              │
         │    ┌─────────┴──────────────────────────┐
         │    ▼                                     ▼
         │  ┌──────────────────────┐   ┌───────────────────────┐
         │  │ Ractor supervision   │   │ Serialization         │
         │  │ tree                 │   │ (AgentSnapshot prost, │
         │  │ (Lain #137 §1)       │   │  wire format)         │
         │  └──────────┬───────────┘   │ (Quinn #134)          │
         │             │               └───────────────────────┘
         │    ┌────────┴─────────┐
         ▼    ▼                  ▼
  ┌──────────────────┐    ┌─────────────────────────────────────────┐
  │ Inference cluster│    │  OrchestratorActor                      │
  │ (Ray Serve +     │ ◄──│  (Kimi K2.5 client, DAG planner,        │
  │  vLLM/SGLang)    │    │   topology, dispatch gate)              │
  │ (Atlas #132)     │    │  (RFC §1.3–1.5, v3.0 §4.4)             │
  └──────────────────┘    └───────────────┬─────────────────────────┘
                                          │
          ┌───────────────────────────────┼──────────────────┐
          ▼                               ▼                   ▼
┌──────────────────┐     ┌─────────────────────┐  ┌──────────────────┐
│ A2A Gateway      │     │ Security / FHE /     │  │ Tool System      │
│ (v3.0 §3.3)      │     │ RESTRICTED POD       │  │ (Wasmtime, MCP)  │
│ (RFC §1.2, 14)   │     │ (Argus #138)         │  │ (v3.0 §4.5)      │
└──────────────────┘     └─────────────────────┘  └──────────────────┘
```

### 2.2 Topological Implementation Order

Derived from the dependency graph. Each phase must not begin until its dependencies are implemented and tested.

| Order | Component | Depends On | RFC Phase | Notes |
|-------|-----------|-----------|-----------|-------|
| 1 | `af-swarm-types` shared crate | — | Phase 0 | AgentId, TaskId, SensitivityLevel, TraceContext must exist before anything else. See §5.1. |
| 2 | Dapr sidecar (infrastructure) | — | Phase 0 | Deploy sidecar alongside runtime. No Rust code change. |
| 3 | DSE taint engine | `af-swarm-types` | Phase 4 | Must exist before orchestrator dispatch; used by both Orchestrator and Argus routing. |
| 4 | Flume L1/L2/L3 topology | `af-swarm-types` | Phase 3 | Channel infrastructure before any actor uses it. |
| 5 | Agent lifecycle (hydrate/freeze) | Flume, Dapr sidecar, DSE | Phase 1 | Core virtual actor substrate. Depends on Senku's Dapr SDK integration. |
| 6 | Serialization (AgentSnapshot) | Agent lifecycle types | Phase 1 | Quinn's track; defines the Dapr state wire format. Must land before freeze/hydrate can be tested end-to-end. |
| 7 | Ractor supervision tree | Agent lifecycle, Flume | Phase 1 | Supervision before agents can run. |
| 8 | Inference cluster (Ray Serve + vLLM) | DSE (for tier routing) | NEW phase | Parallel workstream; can be tested independently with a stub orchestrator. |
| 9 | OrchestratorActor | Ractor supervision, DSE, Inference cluster | Phase 8 | K2.5 client integration. Depends on inference cluster being available. |
| 10 | Security / RESTRICTED POD | Inference cluster, DSE, Dapr state | Phase 6/6b | FHE + pod tiering. Depends on both inference cluster and agent state. |
| 11 | Tool system (Wasmtime) | Agent lifecycle | Phase 7 | Tools execute inside agents; agent lifecycle must be stable first. |
| 12 | A2A Gateway | OrchestratorActor | Phase 14 | External-facing; depends on orchestrator being functional. |

### 2.3 Critical Path to Milestone (Phases 0–10)

The critical path to the production milestone (3 agents running on Rust + Dapr + shared inference) is:

```
af-swarm-types → Agent lifecycle → Ractor tree → OrchestratorActor → Milestone
                       ↑
                 Inference cluster (parallel workstream — must be ready by Phase 8)
```

The inference cluster is a parallel workstream that has NO dependency on the Rust runtime. Atlas (#132) can develop and test it against a stub client. The integration point is the `runtime_inference.proto` contract (§3.2).

---

## 3. Cross-Component gRPC Contracts

Proto skeletons are under `proto/af-swarm/`. All proto files use `proto3` syntax. All services use the `af.swarm.v1` package. Wire encoding is prost (Rust).

Full skeleton files are at:
- [`proto/af-swarm/common.proto`](../../../proto/af-swarm/common.proto)
- [`proto/af-swarm/runtime_dapr.proto`](../../../proto/af-swarm/runtime_dapr.proto)
- [`proto/af-swarm/runtime_inference.proto`](../../../proto/af-swarm/runtime_inference.proto)
- [`proto/af-swarm/orchestrator_agent.proto`](../../../proto/af-swarm/orchestrator_agent.proto)
- [`proto/af-swarm/a2a_gateway.proto`](../../../proto/af-swarm/a2a_gateway.proto)

### 3.1 Runtime ↔ Dapr Sidecar

The Dapr sidecar exposes its own gRPC API (Dapr proto). The Rust runtime uses the `dapr` Rust SDK to call the sidecar. However, the **inversion direction** — Dapr calling INTO the Rust process to activate an agent — requires the Rust runtime to expose an HTTP endpoint conforming to Dapr's actor host specification.

**Dapr actor host HTTP contract** (Rust runtime exposes, Dapr calls):

```
PUT  /actors/{actorType}/{actorId}/method/{methodName}
     Body: JSON { "data": <base64 payload> }
     Response: JSON { "data": <base64 result> }

DELETE /actors/{actorType}/{actorId}
     Called by Dapr when the actor is being deactivated (idle TTL).
     Triggers the Freezing state transition.

GET  /dapr/config
     Returns actor configuration (actorIdleTimeout, actorScanInterval, etc.)

GET  /healthz
     Returns 200 when the runtime is ready to receive activations.
```

**Actor type registrations:**

| `actorType` | Purpose | Dapr Idle TTL |
|------------|---------|--------------|
| `SwarmAgent` | All agent personas (persona encoded in actor state) | 5 minutes (matches `AGENT_IDLE_TTL_SECS`) |
| `WorkflowCoordinator` | Long-running multi-agent workflows | 30 minutes |

**Resolution of OQ-RI-1 (Lain #137 §1.4):** The `DaprActivationListenerActor` in Lain's design is accepted. It owns an axum HTTP server for the actor host endpoint. Use the `dapr` Rust SDK's actor host abstraction — this is specified in the `DaprStateClient` proto skeleton comment. The SDK's `Actor` trait maps cleanly to the ractor `AgentActor`: the SDK's `on_activate` → `HydrateRequest`, `on_deactivate` → `Freeze` (see §6.1, Conflict CL-001).

**NATS delivery guarantee (INT-002 RESOLVED):** L3 NATS subjects are split by durability requirement:

| Transport | Subjects | Rationale |
|-----------|----------|-----------|
| **JetStream** (durable, exactly-once via ack + dedup) | `af.swarm.tasks`, `af.swarm.results`, `af.swarm.audit`, `af.swarm.lifecycle` | Task loss is unacceptable. Core NATS broker restart loses in-flight tasks — unacceptable given PR #143 integrity model. JetStream cost accepted. |
| **Core NATS** (fire-and-forget, loss acceptable) | `af.swarm.backpressure`, `af.swarm.broadcast` | Advisory signals only. Loss is tolerable; these are not task-bearing subjects. |

Configure Dapr pub/sub component (`pubsub.yaml`) to use the `jetstream` component for task/result/audit/lifecycle subjects. Backpressure and broadcast subjects may use the `nats` (Core) component. See `runtime_dapr.proto` `NatsEnvelope` comment block for per-subject classification.

### 3.2 Runtime ↔ Inference Cluster

The inference cluster (Atlas #132) exposes a gRPC service. The OrchestratorActor and AgentActors are clients. See `proto/af-swarm/runtime_inference.proto`.

**Key contracts:**
- Every inference request carries an `InferenceIdentityClaim` (agent ID + tenant ID + effective tier + Ed25519 signature over {task_id, agent_id, tier, timestamp})
- The Ray Serve routing layer verifies the signature before routing (required by Argus §2.4)
- `effective_tier` determines routing: `RESTRICTED` → RESTRICTED POD, others → OPEN POOL
- If RESTRICTED POD is unavailable, the service returns `UNAVAILABLE` (never falls back to OPEN POOL)
- OTel trace context is propagated via the `trace_context` field in every request

**Substrate note:** The inference cluster substrate migrates to AF B300 bare-metal (TDX/SEV attestation) H2 per issue [#168](https://github.com/alternatefutures/admin/issues/168) Phase A. Atlas (#132) targets that substrate as the production target; interim validation runs on CoreWeave/Lambda B200 burst capacity per Rio sales outreach. See §6 Phase-to-Component Map for substrate column.

### 3.3 Orchestrator ↔ Sub-agent

The OrchestratorActor dispatches tasks to AgentActors via Flume L1 (in-process) or NATS L3 (cross-machine). The proto types in `proto/af-swarm/orchestrator_agent.proto` are the canonical source of truth for the L3 wire encoding. L1 Flume uses native Rust types that mirror these proto messages (no serialization on L1).

**Canonical type ownership:** Per Lain #137 §6.1's coordination request, `AgentId`, `TaskId`, and `MessageEnvelope` are defined in `af-swarm-types` (new shared crate, §5.1) and reflected in `common.proto`. Lain's `crates/af-swarm-runtime/src/agent.rs` re-exports from `af-swarm-types`.

### 3.4 A2A Gateway

The A2A gateway (v3.0 §3.3) is specified in `proto/af-swarm/a2a_gateway.proto`. The v3.0 spec is preserved unchanged per the RFC. Key points:
- `GET /.well-known/agent.json` — Agent Card discovery (REST, not gRPC)
- `POST /a2a/tasks/send` — Task submission (REST)
- `GET /a2a/tasks/{id}` — SSE streaming results
- `POST /a2a/tasks/{id}/cancel` — Cancellation

The A2A gateway is the only external-facing component. It translates external A2A requests into internal `TaskDispatch` messages (via OrchestratorActor). The internal task format is agnostic to whether the request came from A2A or an internal Discord/CLI adapter.

---

## 4. Cross-Cutting Concerns

### 4.1 Distributed Tracing (OpenTelemetry)

OTel W3C trace context (`traceparent` + `tracestate`) MUST be propagated across ALL process boundaries. The `TraceContext` message in `common.proto` is the canonical cross-language representation.

| Boundary | How Context Propagates | Notes |
|----------|----------------------|-------|
| HTTP request (axum, A2A gateway) | W3C `traceparent` header | Incoming requests extract context; outgoing inject it |
| Dapr sidecar → Rust runtime (activation) | Dapr passes gRPC metadata | `DaprActivationListenerActor` extracts from HTTP headers on actor invocation |
| Rust runtime → Dapr (state r/w) | Dapr SDK propagates via gRPC metadata | SDK handles this transparently |
| Rust runtime → Inference cluster (gRPC) | `trace_context` field in `InferenceRequest` | Atlas must propagate into Ray Serve spans |
| Ray Serve → vLLM | OpenTelemetry SDK in Python | Atlas's responsibility |
| Rust runtime → NATS L3 | `trace_context` field in `NatsEnvelope` | prost-serialized in message header |
| Cross-machine (NATS) | Extracted from `NatsEnvelope.trace_context` on receive | Continues the same trace |

**Span naming convention:**
- `af.swarm.agent.{agent_id}.handle_task` — task execution span
- `af.swarm.agent.{agent_id}.hydrate` — hydration span
- `af.swarm.agent.{agent_id}.freeze` — freeze span
- `af.swarm.orchestrator.dispatch` — OrchestratorActor dispatch span
- `af.swarm.inference.request` — inference request span (from client side)
- `af.swarm.dapr.state.read` / `af.swarm.dapr.state.write` — Dapr state spans

**Required attributes on all spans:**
- `af.agent.id` — AgentId (string)
- `af.tenant.id` — TenantId (string)
- `af.task.id` — TaskId (string, when applicable)
- `af.sensitivity.tier` — effective DSE tier (PUBLIC/INTERNAL/SENSITIVE/RESTRICTED)

The `af.sensitivity.tier` attribute on spans MUST NOT leak RESTRICTED data into trace payloads. Only the tier label is recorded, never the data itself.

### 4.2 API Versioning

**Proto versioning:** All proto services are in package `af.swarm.v1`. When a breaking change is needed, bump to `af.swarm.v2`. Non-breaking changes (new optional fields) can be made within a version. Proto field numbers are IMMUTABLE once assigned.

**AgentSnapshot versioning:** The `snapshot_version` field in `AgentSnapshot` (Lain #137 §4.2) uses a monotonically increasing integer. The `SnapshotMigrationRegistry` maps `(from_version, to_version)` pairs to migration functions. Migrations must be idempotent.

**Dapr actor config versioning:** The Dapr actor type name (`SwarmAgent`) is version-stable. If a major schema migration requires incompatible state format, a new actor type (`SwarmAgentV2`) is registered and the old type is drained.

**A2A protocol versioning:** The `version` field in AgentCard is `"1.0"` (v3.0 spec). This doc does not change it. Future versions follow semantic versioning; the URL path remains `/a2a/tasks/send` with version negotiated via the Agent Card.

### 4.3 Canonical Error Codes

All components use a shared error taxonomy. gRPC status codes plus AF-specific error detail:

| gRPC Status | AF Error Code | Meaning |
|-------------|---------------|---------|
| `OK` | — | Success |
| `INVALID_ARGUMENT` | `AF-001` | Malformed request (missing required field, invalid ID format) |
| `NOT_FOUND` | `AF-002` | Agent/task/workflow not found |
| `RESOURCE_EXHAUSTED` | `AF-003` | Token budget exhausted |
| `RESOURCE_EXHAUSTED` | `AF-004` | ACTIVE_AGENT_LIMIT reached; hydration backpressure |
| `FAILED_PRECONDITION` | `AF-005` | Inference tier mismatch (tier claim signature failed) |
| `UNAVAILABLE` | `AF-006` | RESTRICTED POD unavailable — REJECT (no fallback) |
| `UNAVAILABLE` | `AF-007` | Inference cluster unavailable — retry with backoff |
| `PERMISSION_DENIED` | `AF-008` | Agent not authorized for requested sensitivity tier |
| `ABORTED` | `AF-009` | Task cancelled by orchestrator |
| `INTERNAL` | `AF-099` | Unexpected internal error (include backtrace in dev; suppress in prod) |

`AF-006` is CRITICAL: it must never be retried against the OPEN POOL. The caller (OrchestratorActor dispatch gate) MUST propagate this as a task failure, not a retry.

### 4.4 Timeout Taxonomy

| Operation | Default Timeout | Config Key | Notes |
|-----------|----------------|-----------|-------|
| Dapr state read (hydration) | 2s | `dapr.state_read_timeout_ms` | Includes Dapr sidecar + Redis/Postgres round-trip |
| Dapr state write (freeze) | 5s | `dapr.state_write_timeout_ms` | Longer for larger snapshots |
| Inference request (first token) | 30s | `inference.first_token_timeout_ms` | Time-to-first-token SLA |
| Inference request (streaming total) | 300s | `inference.stream_total_timeout_ms` | Full response timeout |
| RESTRICTED POD first token | 60s | `inference.restricted_first_token_timeout_ms` | Slower due to Tier A network |
| Orchestrator task dispatch | 5s | `orchestrator.dispatch_timeout_ms` | Time to accept task (not complete it) |
| Agent hydration (end-to-end) | 10s | `runtime.hydration_timeout_ms` | State read + ractor spawn + ready signal |
| A2A task submission response | 10s | `a2a.submission_timeout_ms` | Returns task ID immediately; result via SSE |
| NATS publish | 1s | `nats.publish_timeout_ms` | L3 egress; fail-fast |

### 4.5 Shared Crate: `af-swarm-types`

**Decision (see §5.1 / CL-002):** A new shared crate `crates/af-swarm-types` is introduced. It contains:
- Newtype wrappers: `AgentId(Uuid)`, `TaskId(Uuid)`, `TenantId(Uuid)`, `WorkflowInstanceId(Uuid)`, `SnapshotVersion(u32)`
- `SensitivityLevel` enum (PUBLIC / INTERNAL / SENSITIVE / RESTRICTED)
- `InferenceTier` enum (Open / Restricted) — maps directly from `SensitivityLevel`
- `TraceContext` struct (W3C trace-parent + trace-state)
- `TaskPriority` enum (High / Normal / Low)
- Error codes (AF-001 through AF-099)

**No business logic in `af-swarm-types`.** Only type definitions and their `serde`/`prost` derives.

### 4.6 Integrity Layer

Every durable state record carries two orthogonal correctness mechanisms:

| Mechanism | Field | Purpose |
|-----------|-------|---------|
| `snapshot_version` (OCC counter) | `AgentSnapshot.snapshot_version` | Optimistic concurrency — detects torn writes (FM-21) |
| `integrity_hmac` (HMAC-SHA256) | `AgentSnapshot.integrity_hmac` (field 9) | Tamper detection — detects silent state corruption |

**Rules:**
- `DaprStateClient` MUST verify `integrity_hmac` before returning any snapshot to the caller.
- HMAC is computed over the canonical prost serialization of all snapshot fields **excluding** `etag` and `integrity_hmac` itself.
- HMAC mismatch → `AF-099` error + agent quarantined immediately. FM-class: **FM-HMAC-001** (placeholder — Quinn to assign canonical number in PR [#143](https://github.com/alternatefutures/admin/pull/143); flag for followup before Phase 1 implementation).
- Key rotation follows Argus #138 §3.4. See PR #143 for the canonical HMAC key derivation hierarchy and rotation policy.
- `integrity_hmac` is set by `DaprStateClient` on every write and re-verified on every read. The caller never sets it directly.

### 4.7 Accepted Constraints

These are resolved design decisions that carry known risks. They are not open questions. An RFC amendment is required to change them.

**AC-1: Dapr as virtual actor substrate (resolves OQ-1)**

Dapr is the virtual actor substrate for AF Swarm Runtime. This is a **RESOLVED** decision per the v3.0 plan and this integration contract; no alternative is considered in Phase 0–14. The ractor actor is the in-process execution model (hydrated state); Dapr is the identity and durable state registry (frozen state). They are complementary, not competing.

If Dapr proves inadequate (pre-1.0 SDK issue, sidecar latency SLA breach, etc.), an **RFC amendment is required** — not a silent substitution. The migration path is: introduce a `DaprActorHost` trait (see CL-001), implement an alternative backend behind the trait, and RFC-amend to switch.

**AC-2: `dapr` Rust SDK pre-1.0 (resolves OQ-RI-2)**

The `dapr` Rust SDK is pre-1.0. This is an accepted constraint, not an open question. **Mitigation:** SDK calls are wrapped behind the `DaprActorHost` trait (CL-001). If the SDK's API changes, only the trait adapter changes, not any agent code. Pin SDK version in `Cargo.lock`. Monitor `dapr/rust-sdk` releases; evaluate stability at each Phase milestone.

**AC-3: Kimi K2.5 approved for self-hosting (resolves OQ-2)**

Kimi K2.5 is approved for self-hosting on AF infrastructure under the Moonshot AI Modified MIT License. **CEO decision 2026-04-21.** Attribution requirement (100M MAU or $20M/mo revenue) is not in scope for the current sprint. Export-controls posture is owned by the CEO directly. Model assignment per CL-005 is confirmed: Kimi K2.5 → OPEN POOL, Claude API → RESTRICTED POD, Qwen2.5-0.5B → DSE routing in OPEN POOL.

---

## 5. Conflict-Resolution Log

This section records every inter-component inconsistency found during the integration review, the decision made, and the rationale. Decisions here override sibling docs.

### CL-001: Dapr Actor Host Direction (Lain OQ-RI-1)

**Conflict:** Lain #137 §1.4 raises OQ-RI-1: the RFC says "Dapr replaces the durable state layer and cross-process activation" but does not specify that Dapr calls INTO the Rust process (actor host inversion). Lain asks whether to use the `dapr` Rust SDK's actor host abstraction or hand-roll axum.

**Decision: Use the `dapr` Rust SDK actor host abstraction.**

**Rationale:** The `dapr/rust-sdk` Actor trait (`on_activate`, `on_invoke`, `on_deactivate`) maps cleanly to the lifecycle transitions in Lain's state machine. Specifically:
- `on_activate` → triggers `HydrateRequest` in `SwarmSupervisor`
- `on_invoke(method, payload)` → delivers `AgentMessage` to active `AgentActor` via Flume L1
- `on_deactivate` → triggers `Freeze` `LifecycleCommand`

The SDK handles the HTTP server (Warp-based internally) so `DaprActivationListenerActor` can delegate to it rather than owning an axum server. The ractor actor still wraps the SDK's actor host, maintaining the supervision tree invariants.

**Risk (Lain OQ-RI-2):** The `dapr` Rust SDK is pre-1.0. Mitigation: wrap SDK calls behind a `DaprActorHost` trait in `af-swarm-types`. If the SDK's API changes, only the adapter changes, not the agent code. Pin SDK version in `Cargo.lock`.

**Action:** Lain: update §1.4 to reference this decision. Senku (#130): confirm the SDK's actor host HTTP is compatible with the sidecar port configuration.

---

### CL-002: Shared Types Crate (Lain Coordination Request)

**Conflict:** Lain #137 §6.1 requests that `AgentId`, `TaskId`, and `MessageEnvelope` newtypes be reviewed by chief-architect for cross-component compatibility. Currently they live in `crates/af-swarm-runtime/src/agent.rs`.

**Decision: Move to a new `crates/af-swarm-types` shared crate (see §4.5).**

**Rationale:** At minimum three components need these types: `af-swarm-runtime` (Lain), serialization (Quinn), and the A2A gateway. A shared crate prevents circular dependencies. The types belong to no component — they are the vocabulary of the system.

**Action:** Create `crates/af-swarm-types` in Phase 0. Lain's `crates/af-swarm-runtime/src/agent.rs` re-exports from it. Quinn's serialization crate depends on it. This is a Phase 0 deliverable, before any other crate is implemented.

---

### CL-003: AgentSnapshot Wire Format (Lain/Quinn Split)

**Conflict:** Lain #137 §4.2 defines `AgentSnapshot` as the logical schema and defers wire encoding to Quinn (#134). Lain states "likely JSON via `serde_json` or Protobuf via `prost`" — this must be resolved before Phase 1 implementation.

**Decision: Dual encoding.**
- **Dapr state store:** JSON via `serde_json`. Dapr state stores (Redis, PostgreSQL) use JSON by default; human-readable for debugging; `snapshot_version` field is top-level in JSON.
- **L3 NATS wire (cross-machine AgentSnapshot transfer):** Protobuf via `prost`. See `proto/af-swarm/orchestrator_agent.proto` `AgentSnapshotTransfer` message.

**Rationale:** These are different use cases. The Dapr state store is "cold storage" — optimized for correctness, not throughput. The NATS wire path (e.g., migrating an agent from Machine A to Machine B mid-task) needs compact binary encoding.

**Action:** Quinn (#134): implement both encodings. Lain: update §4.2 to reference this decision. The JSON schema for Dapr state is the `AgentSnapshot` struct with `serde` derives; the prost schema is in `proto/af-swarm/orchestrator_agent.proto`.

---

### CL-004: KV Cache Namespace Key (Argus/Atlas Coordination)

**Conflict:** Argus #138 §4.2 proposes `KvCacheNamespaceKey = "{tenant_id}:{tier}:{team}"`. Atlas (#132) is not yet open. Argus marks this as a coordination item with atlas.

**Status: RESOLVED.**

**Binding scheme:**
- Key format: `{tenant_id}:{dse_tier}:{team_id}`
- `tenant_id`: UUIDv4 string from `TenantId`
- `dse_tier`: lowercase proto enum name — `public` | `internal` | `sensitive` | `restricted`
- `team_id`: optional team scope; empty string if not applicable
- **RESTRICTED tier:** `kv_cache_namespace = ""` (empty string — NO cache sharing, ever)

**Rationale:** Tenant isolation precedes tier so prefix-based invalidation can target all tiers for a given tenant. Full lowercase proto enum names are used for clarity and forward-compatibility. RESTRICTED cache isolation is absolute — no exceptions.

**Action:** This scheme is binding on atlas (#132) and Argus (#138). Atlas may only amend it with explicit sign-off from the integration-contract owner. Argus: update §4.2 to reference this resolved binding. See also `common.proto` SensitivityLevel comment block.

---

### CL-005: RESTRICTED POD Inference Model (Argus/RFC Alignment)

**Potential conflict:** Argus #138 §1.4 specifies Claude API proxy as the default RESTRICTED POD model. RFC §1.3 says "Claude reserved for high-stakes reasoning paths" and uses Kimi K2.5 as primary orchestrator. Are these in conflict?

**Decision: NOT a conflict — consistent by design.**

**Rationale:** RESTRICTED data = high-stakes reasoning by definition (it involves cryptographic keys, PII, regulated data, or financial PCI). The RFC's clause "Claude reserved for high-stakes reasoning" directly maps to RESTRICTED-tier tasks. Kimi K2.5 handles PUBLIC/INTERNAL/SENSITIVE tasks at scale; Claude handles RESTRICTED-tier tasks. This is the intended model assignment.

**Clarification added to integration contract:** Inference model assignment is:
- Kimi K2.5 → OPEN POOL → PUBLIC/INTERNAL/SENSITIVE tasks
- Claude API → RESTRICTED POD (Tier A) → RESTRICTED tasks
- Qwen2.5-0.5B → DSE routing/classification → runs in OPEN POOL (Tier C) for non-RESTRICTED routing decisions only

---

### CL-006: OrchestratorActor Location in Supervision Tree

**Conflict:** v3.0 §4.1 places the orchestrator under the `Dynamic Supervisor` branch in the ractor tree. Lain #137 §1.1 places `OrchestratorActor` as a direct child of `ProcessSupervisor` (separate from `SwarmSupervisor`). The RFC §1.4 describes the orchestrator as "centralized top."

**Decision: Lain's design is correct. OrchestratorActor is a direct child of ProcessSupervisor, NOT under SwarmSupervisor.**

**Rationale:** The orchestrator is a singleton per runtime node; it has a different restart strategy from agent actors (degraded-mode on failure, not quarantine). Placing it under `ProcessSupervisor` directly gives it the same isolation level as `SwarmSupervisor` and `MessageRouterSupervisor` — appropriate for a critical singleton. This is a minor discrepancy with v3.0 §4.1 but consistent with the RFC's "centralized top" intent.

**Action:** The v3.0 §4.1 actor tree diagram is not modified (per constraint: do not rewrite v3.0). This integration contract is the resolution record.

---

### CL-007: Tools-per-Agent Cap Enforcement Location

**Conflict:** RFC §1.6 states "`max_tools` cap enforcement" is in Phase 7 (Tool System). Lain #137 §8 table lists `MAX_TOOLS_PER_AGENT = 16` as a runtime parameter. Argus #138 §2.1 references the dispatch gate but does not mention tool count. Who enforces it?

**Decision: Two-point enforcement.**
1. **Dispatch time (OrchestratorActor):** Before dispatching a task to an agent, the orchestrator checks `agent_config.max_tools ≤ 16`. Tasks dispatched to over-tooled agents are rejected with `AF-001`.
2. **Registration time (Tool System, Phase 7):** The tool registry rejects `register_tool()` calls that would push an agent over its `max_tools` limit.

**Rationale:** Defense-in-depth. Dispatch-time check catches misconfigured agents before they consume inference budget. Registration-time check prevents invalid configs from being stored.

---

### CL-008: W&D Scheduler vs Kimi K2.5 Fallback Path

**Conflict:** RFC §2 Phase 8 says "W&D scheduler becomes fallback for when K2.5 is unavailable." v3.0 §4.4 describes W&D Scheduler as the primary path. If K2.5 is unavailable and W&D takes over, does LAMaS CPO still run?

**Decision: Yes. When K2.5 is unavailable, the fallback path is: W&D Scheduler → LAMaS CPO post-processing → MAST failure detection. All three remain active in fallback mode.**

**Rationale:** LAMaS CPO and MAST operate on the task graph output, not on the model that produced it. They are model-agnostic post-processors. Removing them from the fallback path would degrade reliability precisely when K2.5 is unavailable (a higher-stress scenario).

---

## 6. Phase-to-Component Map

Restates the RFC phase plan with links to the design doc that owns each phase's deliverables. "Phase" numbers follow RFC §2 and v3.0 §7.

**Substrate split (issue [#168](https://github.com/alternatefutures/admin/issues/168)):** Phase B migrates stable-tier workloads to AF B300 bare-metal. Phase C provisions the RESTRICTED POD on AF B300 with hardware attestation (TDX/SEV). See issue #168 for the full capacity and substrate split plan across H1/H2. The phase rows below note where substrate changes apply.

| Phase | Name | Primary Owner | Design Doc | Key RFC Changes |
|-------|------|---------------|-----------|----------------|
| **0** | Scaffolding | Chief Architect (this doc) | `integration-contract.md` + shared `af-swarm-types` crate | Dapr actor config, `max_tools`, Ray Serve client config, K2.5 provider type |
| **1** | Core Runtime | Lain | `rust-runtime-internals.md` (#137) | Dapr virtual actor lifecycle (hydrate/freeze), ACTIVE_AGENT_LIMIT=100→1K |
| **2** | Agent Definitions | Senku | (PR #130, pending) | `max_tools: 16` default, `data_tier` routing hints for K2.5 vs Claude |
| **3** | Messaging | Lain | `rust-runtime-internals.md` §2 (#137) | No change; Flume L1/L2/L3 unchanged |
| **4** | Basic Security | Argus | `security-multi-tenant.md` (#138) | Inference identity binding (+5–8h); DSE tier routing to inference |
| **5** | Basic API | Senku / Chief Architect | (pending) | Unchanged; REST + WebSocket + Discord adapter |
| **6** | Memory + FHE | Quinn + Argus | (Quinn #134 pending; Argus #138) | FHE at Dapr state + inference boundary; RESTRICTED POD provisioning. RESTRICTED POD hardware: AWS Nitro Enclaves H1 sprint → AF B300 bare-metal TDX/SEV attestation H2 Phase C per issue #168 and PR #138 §11.2. |
| **6b** | TEE.fail Mitigations | Argus | `security-multi-tenant.md` §5 (#138) | Multi-party attestation extended to RESTRICTED POD; Secret Proxy for inference |
| **7** | Tool System | Lain + Chief Architect | (pending) | `max_tools` enforcement in tool registry (§§ CL-007) |
| **8** | Orchestrator | Chief Architect + Atlas | `integration-contract.md` §3.3 + Atlas (#132) | K2.5 client integration from Phase 8; W&D as fallback (§§ CL-008) |
| **9** | Observability | Lain + Atlas | `rust-runtime-internals.md` §7.3 (#137) | Ray Serve / Dapr spans; per-agent inference cost attribution |
| **10** | Migration | QA Engineer | (QA #135, pending) | 1K agent shadow-mode load test before cutover |
| **11** | Remaining Agents | Senku | (pending) | Port all agents to Dapr virtual actor lifecycle; `max_tools` enforcement |
| **12** | Advanced Security | Argus | (pending amendment to #138) | DSE taint on inference path; S9 egress control for K2.5 cluster |
| **13** | Advanced Orchestrator | Chief Architect | (pending) | Irregular peer topology formation; K2.5 primary, W&D fallback confirmed |
| **14** | A2A Gateway | Chief Architect | `proto/af-swarm/a2a_gateway.proto` | Unchanged from v3.0; bridges TS marketplace to Rust runtime |
| **NEW** | Inference Cluster Setup | Atlas | (#132, pending) | Ray Serve + vLLM on Akash GPU; K2.5 weights; SGLang for Qwen2.5; load test |

---

## 7. Open Questions (Requiring Human Decision)

These questions are not resolved in this document. They are flagged for human decision per the RFC's OQ table.

Resolved questions have been removed from this table. See §4.7 (Accepted Constraints) for OQ-1, OQ-2, and OQ-RI-2. See §5 CL-004 for KV cache namespace resolution. See §3.1 for INT-002 (JetStream) resolution. See §6 for OQ-4 (RESTRICTED POD hardware) note.

| ID | Severity | Source | Question | Default If Not Resolved |
|----|----------|--------|----------|------------------------|
| **OQ-3** | MEDIUM | RFC / Argus DQ-8 | Who owns the inference cluster infrastructure? Infra team or dedicated MLOps? | Infra team; flag if capacity is insufficient |
| **OQ-5** | MEDIUM | RFC | Load test criteria: what does "1,000 concurrent agents" mean as a test? | 1,000 agents × 10-token classification task, P99 hydration <500ms |
| **INT-001** | LOW | This doc | `af-swarm-types` crate path: `crates/af-swarm-types` or `local-packages/@af-swarm-types`? Repo currently uses `local-packages/` for shared packages. | `crates/af-swarm-types` (new Rust-only pattern; `local-packages/` is for JS/TS packages) |

---

## 8. Amendments Log

This section records amendments made as sibling PRs land. Start date: 2026-04-16.

| Date | Amendment | Trigger |
|------|-----------|---------|
| 2026-04-16 | Initial draft. Reflects #137 (Lain) and #138 (Argus). | PR #136 initial commit |
| 2026-04-21 | **MB1** Resolve OQ-1: Dapr substrate confirmed, moved to §4.7 Accepted Constraints (AC-1). | PR #141 CEO review — merge-blocker |
| 2026-04-21 | **MB2** Bind CL-004: KV cache namespace key scheme RESOLVED (`{tenant_id}:{dse_tier}:{team_id}`; RESTRICTED = empty string). | PR #141 CEO review — merge-blocker |
| 2026-04-21 | **MB3** Resolve INT-002: JetStream for task/result/audit/lifecycle; Core NATS for backpressure/broadcast. §3.1 and `runtime_dapr.proto` updated. | PR #141 CEO review — merge-blocker |
| 2026-04-21 | **MB4** Integrity layer: `AgentSnapshot.integrity_hmac` (field 9) added; §4.6 Integrity Layer added; `DaprStateReadResponse` comment updated. FM-class placeholder FM-HMAC-001 (Quinn #143 to assign canonical number). | PR #141 CEO review — merge-blocker |
| 2026-04-21 | **Amendment (i)** OQ-4 resolved: RESTRICTED POD hardware path added to §6 Phase 6 row (Nitro Enclaves H1 → B300 TDX/SEV H2 per issue #168). | PR #141 CEO review |
| 2026-04-21 | **Amendment (ii)** §3.2 substrate cross-reference added (B300 bare-metal H2, interim CoreWeave/Lambda). | PR #141 CEO review |
| 2026-04-21 | **Amendment (iii)** §6 substrate paragraph added, referencing issue #168 for Phase B/C substrate split. | PR #141 CEO review |
| 2026-04-21 | **Amendment (iv)** OQ-2 severity escalated to HIGH; CEO decision required flag added (blocking Atlas #132). | PR #141 CEO review |
| 2026-04-21 | **Amendment (vii)** OQ-2 resolved: Kimi K2.5 approved for AF self-hosting under Moonshot AI Modified MIT License. Moved to §4.7 Accepted Constraints (AC-3). Model assignment confirmed: Kimi K2.5 → OPEN POOL, Claude API → RESTRICTED POD, Qwen2.5-0.5B → DSE routing in OPEN POOL. | CEO decision 2026-04-21 |
| 2026-04-21 | **Amendment (v)** Proto `// todo!()` comments replaced with `// TODO: implementation in Phase <N>` across all proto skeletons. | PR #141 CEO review |
| 2026-04-21 | **Amendment (vi)** OQ-RI-2 moved from Open Questions to §4.7 Accepted Constraints (AC-2). | PR #141 CEO review |
| — | *Atlas (#132) amendment pending* | Will add inference cluster API shapes to §3.2 when PR opens |
| — | *Quinn (#134) amendment pending* | Will finalize CL-003 (wire format) when PR opens; will assign canonical FM-class for HMAC mismatch (currently FM-HMAC-001 placeholder) |
| — | *Senku (#130) amendment pending* | Will confirm CL-001 SDK compatibility when PR opens |
| — | *QA (#135) amendment pending* | Will link load test spec to Phase 10 row in §6 when PR opens |


---

## 9. OQ-9 Addendum — Tiered State Architecture

**Author:** Chief Architect
**Date:** 2026-04-20
**Issue:** [alternatefutures/admin#144](https://github.com/alternatefutures/admin/issues/144)
**Branch:** `feat/issue-144-oq9-chief-architect`
**Status:** DRAFT — pending human decision OQ-9 (see §9.8)

> **Scope of this addendum:** This section records the integration-contract-level impact of the proposed OQ-9 tiered state architecture (Turso L1 + SurrealDB L2 replacing Redis / PostgreSQL / LanceDB). It does NOT rewrite any prior section. Where a prior section's content changes, an amendment note is recorded here and cross-referenced in §8 above. All section numbers below are prefixed §9.x to distinguish them from the original §1–§8.

---

### 9.1 Architecture Summary

Issue #144 proposes replacing the RFC's Redis/PostgreSQL/LanceDB state stack with two purpose-built databases:

| Tier | Engine | What lives here | Hot path? | Latency |
|------|--------|----------------|-----------|---------|
| **L1 State** | Turso / libSQL embedded replica | `AgentSnapshot`, working memory, pending task list | Yes — agent hydrate/freeze | ~1–10µs (local SQLite file) |
| **L2 State** | SurrealDB 3.0 | Agent capability graph, cross-agent relationships, memory embeddings, temporal facts | No — orchestrator planning only | ~0.7–5ms (network) |

**Systems removed:** Redis, PostgreSQL, LanceDB — **3 systems**
**Systems added:** Turso (per-node embedded replica), SurrealDB (cluster) — **2 systems**
**Net:** −1 system, +3 capabilities (graph traversal, native vector search, temporal facts)

**Dapr impact:** Dapr's state store role is bypassed for agent state. Dapr retains:
- Pub/sub (NATS backend) — unchanged
- Workflows (durable multi-step tasks) — unchanged
- Actor activation listener (`/actors/{actorType}/{actorId}/method/{methodName}`) — unchanged

The Dapr virtual actor identity still exists. Only the state I/O path changes.

---

### 9.2 Amended Component Dependency Graph (Amends §2)

The dependency graph in §2.1 changes as follows. The ASCII diagram in §2.1 is preserved as the original record; this section records the delta.

**Removed nodes:**
- Redis (was: Dapr state store backend for hot state)
- PostgreSQL (was: Dapr state store backend for durable state)
- LanceDB (was: vector embedding store, implicitly referenced in Atlas #132 and Quinn #134)

**Added nodes:**
- **Turso embedded replica** — per-node local SQLite file. Sits between `Agent lifecycle (hydrate/freeze)` and disk. Zero network hop; zero connection pool; 1,000 concurrent reads with zero contention.
- **SurrealDB cluster** — cluster-wide, connected only from `OrchestratorActor`. Not on the hydrate/freeze hot path. Used for capability graph queries, vector memory lookups, and temporal fact queries before task dispatch.

**Amended topological order (amends §2.2):**

| Step | Component | Change under OQ-9 |
|------|-----------|-------------------|
| 2 | Dapr sidecar (infrastructure) | **Scope narrowed**: activation + pub/sub + workflows only. State store backend config (Redis/PostgreSQL Dapr components) is removed. |
| 5 | Agent lifecycle (hydrate/freeze) | **State backend changes**: Dapr SDK state methods replaced by `StateClient::load_snapshot()` / `StateClient::save_snapshot()` against Turso L1. No other change. |
| NEW (Phase 0) | Turso embedded replica per node | Must be provisioned on each Akash pod before Phase 1 agent lifecycle. Senku #130 owns deployment. |
| NEW (before Phase 8) | SurrealDB cluster | Must be deployed and schema-migrated before OrchestratorActor planning queries can run. Senku #130 owns deployment. |

**Amended critical path:**
```
af-swarm-types → Turso (L1, per node) → Agent lifecycle → Ractor tree → OrchestratorActor → Milestone
                                               ↑
                         SurrealDB (L2 cluster, parallel — must be ready by Phase 8)
```

---

### 9.3 `DaprStateClient` → `StateClient` Trait Rename

Lain #137 defines a `DaprStateClient` trait for state I/O. Under OQ-9 this trait is renamed and its implementation backend changes.

**Rename:** `DaprStateClient` → `StateClient`

The trait lives in `crates/af-swarm-types/src/state.rs` (per §4.5 / CL-002 — types belong in the shared crate). The method signatures are unchanged; only the name and backend change.

**Canonical trait definitions:**

```rust
// crates/af-swarm-types/src/state.rs

/// L1 State — Turso embedded replica. Used on every hydrate/freeze.
#[async_trait]
pub trait StateClient: Send + Sync {
    /// Load agent snapshot from L1. Returns None if agent has never been frozen.
    async fn load_snapshot(
        &self,
        agent_id: &AgentId,
    ) -> Result<Option<AgentSnapshot>, StateError>;

    /// Persist agent snapshot to L1 atomically.
    async fn save_snapshot(
        &self,
        agent_id: &AgentId,
        snapshot: &AgentSnapshot,
    ) -> Result<(), StateError>;

    /// Remove snapshot on permanent agent deletion.
    async fn delete_snapshot(&self, agent_id: &AgentId) -> Result<(), StateError>;
}

/// L2 State — SurrealDB cluster. Used by OrchestratorActor at planning time only.
/// NOT called on the hydrate/freeze hot path.
#[async_trait]
pub trait RichStateClient: Send + Sync {
    /// Graph traversal: find agents with affinity to a capability label.
    async fn query_agent_affinity(
        &self,
        capability: &str,
        limit: usize,
    ) -> Result<Vec<AgentId>, StateError>;

    /// Vector similarity search over agent memory embeddings.
    async fn vector_search(
        &self,
        embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<MemoryHit>, StateError>;

    /// Temporal fact: agent snapshot state at a point in time.
    async fn agent_at_time(
        &self,
        agent_id: &AgentId,
        at: SystemTime,
    ) -> Result<Option<AgentSnapshot>, StateError>;
}
```

**Implementation mapping:**

| Trait | Impl struct | Crate | Backend |
|-------|-------------|-------|---------|
| `StateClient` | `TursoStateClient` | `crates/af-swarm-runtime` | Turso / libSQL embedded replica |
| `RichStateClient` | `SurrealDbClient` | `crates/af-swarm-runtime` | SurrealDB 3.0 cluster |

Implementations live in `crates/af-swarm-runtime`, not in `af-swarm-types`. This preserves the invariant that `af-swarm-types` contains only trait definitions, newtypes, and pure data structures — no I/O, no network dependencies.

**Concurrency note:** Turso uses SQLite WAL mode. Concurrent reads are lock-free. Writes are serialized per-agent by primary key (`agent_id`). At 1,000 concurrent agents all writing to separate rows, write contention is zero.

---

### 9.4 Amended Conflict Resolution: CL-001 (Activation Scope Narrowing)

**Prior decision (CL-001):** Use the `dapr` Rust SDK actor host abstraction for activation/deactivation. The `DaprActorHost` trait wraps the SDK to isolate the pre-1.0 risk.

**Amendment under OQ-9:** The activation/deactivation decision **STANDS**. The Dapr SDK still owns `on_activate` → `HydrateRequest` and `on_deactivate` → `Freeze`. The amendment is a scope narrowing only:

- Inside `on_activate`, after the ractor actor is spawned, state is read via `TursoStateClient::load_snapshot()` — **not** the Dapr SDK state API (`get_state`).
- Inside `on_deactivate`, state is written via `TursoStateClient::save_snapshot()` — **not** the Dapr SDK state API (`save_state`).
- The `DaprActivationListenerActor` in Lain's design is unchanged in structure. It still wraps the SDK's actor host. The difference is what happens inside the activation/deactivation handlers.

**Updated risk assessment (OQ-RI-2):** The pre-1.0 SDK risk still applies to the activation path. Mitigation (`DaprActorHost` trait) is unchanged. The state I/O path is now fully decoupled from the SDK, which **reduces** the blast radius of any SDK breaking change: state I/O is unaffected even if the SDK's actor host API changes.

**Action for Lain:** Update `rust-runtime-internals.md` §1.4 to note this scope narrowing. The `DaprStateClient` trait reference in that section becomes `StateClient` (Turso backend).

---

### 9.5 Amended Conflict Resolution: CL-003 (L1 Storage Backend)

**Prior decision (CL-003):** Dual encoding — JSON via `serde_json` for Dapr state store (Redis/PostgreSQL); prost for NATS L3 cross-machine transfer.

**Amendment under OQ-9:**

| Encoding path | Prior (CL-003) | Amended (OQ-9) |
|---------------|----------------|----------------|
| L1 storage (agent hydrate/freeze) | JSON blob in Redis/PostgreSQL via Dapr state API | SQLite row in Turso embedded replica via `TursoStateClient` |
| L3 NATS wire (cross-machine `AgentSnapshotTransfer`) | prost | prost — **unchanged** |

**L1 Turso schema:**

```sql
-- crates/af-swarm-runtime/migrations/0001_agent_snapshots.sql
CREATE TABLE IF NOT EXISTS agent_snapshots (
    agent_id          TEXT    PRIMARY KEY,         -- AgentId (UUID v7, canonical string)
    tenant_id         TEXT    NOT NULL,            -- TenantId (UUID string)
    snapshot_version  INTEGER NOT NULL DEFAULT 1,  -- monotone; matches AgentSnapshot.snapshot_version
    sensitivity_level TEXT    NOT NULL DEFAULT 'INTERNAL', -- DSE tier label
    state_blob        BLOB    NOT NULL,            -- bincode-encoded AgentSnapshot
    pending_tasks     TEXT    NOT NULL DEFAULT '[]', -- JSON array of pending TaskId strings
    updated_at        INTEGER NOT NULL             -- Unix epoch ms (for temporal queries)
);

CREATE INDEX IF NOT EXISTS idx_snapshots_tenant
    ON agent_snapshots (tenant_id);
CREATE INDEX IF NOT EXISTS idx_snapshots_updated
    ON agent_snapshots (updated_at);
```

**Quinn's work:** The `AgentSnapshot` struct shape and its `serde` / `prost` derive macros are unchanged. Quinn's CL-003 encoding deliverable (prost schema for L3) remains valid. The only change is the L1 call site: `dapr_client.save_state(key, json_bytes)` becomes `turso_client.save_snapshot(agent_id, &snapshot)`.

**Migration from legacy Dapr state (if any agents have prior JSON state in Redis/Postgres):** The `SnapshotMigrationRegistry` (§4.2) handles a `(from_version=0, to_version=1)` migration that deserializes the legacy JSON from the old Dapr store and re-serializes as a Turso SQLite row. Migration is lazy — each agent migrates on its next activation. No bulk migration required.

---

### 9.6 Amended Timeout Taxonomy (Amends §4.4)

The following §4.4 rows change under OQ-9. Prior values are preserved in §4.4 for historical record; amended values here are binding if OQ-9 is accepted.

| Operation | Old config key | New config key | Prior timeout | Amended timeout | Rationale |
|-----------|---------------|----------------|--------------|-----------------|-----------|
| State read, hydration | `dapr.state_read_timeout_ms` | `state.l1_read_timeout_ms` | 2,000 ms | **10 ms** | Turso local file: ~1–10µs actual. 10 ms covers cold-start replica sync lag. |
| State write, freeze | `dapr.state_write_timeout_ms` | `state.l1_write_timeout_ms` | 5,000 ms | **50 ms** | SQLite WAL write + async replication flush. 50 ms is generous for replica lag. |
| L2 orchestrator query | _(new)_ | `state.l2_query_timeout_ms` | n/a | **20 ms** | SurrealDB 3.0 benchmarks: ~0.7–5 ms for graph/vector queries. 20 ms P99. |

**Updated `runtime.hydration_timeout_ms`:** Reduce default from 10,000 ms to **100 ms**.

```
L1 state read (Turso):    10 ms   (was: up to 2,000 ms)
ractor actor spawn:         5 ms   (unchanged)
ready signal:               5 ms   (unchanged)
────────────────────────────────
Total P99 hydration budget: 20 ms
100 ms timeout = 5× P99 budget (absorbs cold replica sync on first read)
```

---

### 9.7 Amended OTel Span Names (Amends §4.1)

The following span names in §4.1 are renamed. Prior names (`af.swarm.dapr.state.*`) must not appear in new code; they may appear in dashboards created before this addendum lands and should be aliased during a transition window.

| Prior span name | Amended span name |
|-----------------|------------------|
| `af.swarm.dapr.state.read` | `af.swarm.state.l1.read` |
| `af.swarm.dapr.state.write` | `af.swarm.state.l1.write` |
| _(new)_ | `af.swarm.state.l2.query` |

**Required attributes on `af.swarm.state.l1.*` spans:**

| Attribute | Value |
|-----------|-------|
| `af.agent.id` | AgentId (string) |
| `af.tenant.id` | TenantId (string) |
| `af.state.tier` | `"L1"` |
| `af.state.backend` | `"turso"` |

**Required attributes on `af.swarm.state.l2.query` spans:**

| Attribute | Value |
|-----------|-------|
| `af.agent.id` | OrchestratorActor's AgentId |
| `af.tenant.id` | TenantId |
| `af.state.tier` | `"L2"` |
| `af.state.backend` | `"surrealdb"` |
| `af.state.query_type` | One of `"graph"`, `"vector"`, `"temporal"` |

---

### 9.8 New Open Question: OQ-9 (Human Decision Required)

Per the pattern in §7, architecture decisions require explicit human sign-off before implementation begins. OQ-9 is added to the open questions table:

| ID | Source | Question | Default If Not Resolved |
|----|--------|----------|------------------------|
| **OQ-9** | Issue #144 | Accept tiered Turso (L1) + SurrealDB (L2) replacing Redis/PostgreSQL/LanceDB? Or retain RFC Redis/Postgres + LanceDB (with optional DragonflyDB drop-in for Redis)? | **Blocked.** No implementation of §9.3–§9.7 begins until human decision. DragonflyDB as a Redis drop-in (zero code change, config only) is available as an interim performance win regardless of OQ-9 outcome. |

**Decision options and contract consequences:**

| Option | Contract impact | Implementation scope |
|--------|----------------|---------------------|
| **Accept OQ-9** | §9.3 (trait rename), §9.4 (CL-001 scope), §9.5 (CL-003 L1 storage), §9.6 (timeouts), §9.7 (spans) become binding. | Turso + SurrealDB deployment (Senku), `TursoStateClient` + `SurrealDbClient` impls (Lain), Turso schema migration (Quinn), benchmark suite amendment (QA #135). |
| **Reject OQ-9** | This addendum is informational only. §1–§8 remain fully in effect. | Optionally: DragonflyDB as Redis drop-in (zero code change). |
| **Reject + DragonflyDB interim** | §1–§8 unchanged. Add one row to §8: "Redis backend swapped to DragonflyDB (drop-in, config only)." | Config change only in Dapr component YAML. |

---

### 9.9 Summary of Artifacts Changed Under OQ-9 (If Accepted)

| Artifact | Change | Owner |
|----------|--------|-------|
| `crates/af-swarm-types/src/state.rs` | New `StateClient` trait + `RichStateClient` trait (replaces `DaprStateClient`) | Chief Architect / Lain |
| `crates/af-swarm-runtime/src/state/turso.rs` | New `TursoStateClient` impl | Lain (#131 amendment for #144) |
| `crates/af-swarm-runtime/src/state/surrealdb.rs` | New `SurrealDbClient` impl | Lain (#131 amendment for #144) |
| `crates/af-swarm-runtime/migrations/0001_agent_snapshots.sql` | Turso L1 schema (§9.5) | Lain / Quinn |
| `proto/af-swarm/runtime_dapr.proto` | No change — actor host contract unchanged | — |
| Dapr component YAML (state store) | Remove Redis/PostgreSQL state store components; retain pub/sub + workflow components | Senku (#130 amendment for #144) |
| Akash deployment SDLs | Add Turso embedded replica per pod; add SurrealDB cluster; remove Redis/PostgreSQL pods | Senku (#130 amendment for #144) |
| `docs/architecture/design/integration-contract.md` §4.4 | Timeout defaults (this addendum §9.6 is binding) | Chief Architect |
| `docs/architecture/design/integration-contract.md` §4.1 | Span names (this addendum §9.7 is binding) | Chief Architect |
| `docs/architecture/design/state-schema.md` | Quinn's track; see Quinn's OQ-9 addendum (PR referencing #144) | Quinn (#134 amendment) |
| QA benchmark suite | Add: 1,000 concurrent Turso reads vs Redis reads; SurrealDB graph query under load | QA Engineer (#135 amendment for #144) |
