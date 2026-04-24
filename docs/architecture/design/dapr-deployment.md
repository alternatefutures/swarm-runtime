# Dapr Virtual Actor Deployment Substrate

**Track:** Infrastructure — Issue [#130](https://github.com/alternatefutures/admin/issues/130)
**Parent Epic:** [#129](https://github.com/alternatefutures/admin/issues/129) — 1000-Agent Parallel Design Phase
**RFC Reference:** [RFC-001 §1.2](../reviews/1000-agent-amendment.md) — Virtual Actor Substrate: Dapr
**Base Plan:** [rust-swarm-runtime.md v3.0](../rust-swarm-runtime.md) — DO NOT MODIFY
**Author:** Senku (infrastructure track)
**Date:** 2026-04-16
**Status:** DRAFT — pending argus (#133) + quinn (#134) review

---

## 1. Summary

This document specifies everything **below the Rust runtime process**: Dapr sidecar configuration, state store selection and tiering, pub/sub backend wiring, K8s deployment manifests, Akash SDL for multi-provider deployment, scale-to-zero policy, and Prometheus observability wiring.

Ground truth: RFC-001 §1.2 selects **Dapr** over Rust-native `ractor` as the virtual actor substrate. This document implements that decision without relitigating it.

### 1.1 What Dapr Provides to the Swarm Runtime

| Dapr Building Block | Swarm Runtime Use |
|---|---|
| Actor state management | Per-agent durable state — hydrate on task, freeze on idle |
| Pub/sub (NATS) | Agent-to-agent event fan-out, task dispatch |
| State store (Turso L1 + SurrealDB L2) | Embedded SQLite replicas per pod (L1, hot) + SurrealDB cluster (L2, durable checkpoints); per OQ-9 resolution (#144) |
| Workflow | Phase 8 orchestrator DAG execution (partial replacement) |
| Secret store | Infisical integration; secret injection into agent prompts |
| Observability sidecar | Dapr metrics → Prometheus → Grafana pipeline |

### 1.2 What Dapr Does NOT Replace

- **Ractor in-process supervision tree** — retained for local actor supervision within a single runtime pod (v3.0 §4.1). Dapr manages the distributed actor address space; Ractor manages in-process lifecycle.
- **Flume/tokio channels (L1/L2 messaging)** — in-process, zero serialization path untouched.
- **NATS cross-machine transport (L3)** — Dapr pub/sub sits on top of NATS; raw NATS is still used for non-actor messaging and the existing Discord/code/marketing worker traffic.
- **FHE/TFHE-rs** — encryption primitives are Rust-layer; Dapr state store receives already-encrypted blobs for RESTRICTED data.

---

## 2. Dapr Sidecar Configuration

### 2.1 Deployment Model

One Dapr sidecar (`daprd`) per **swarm runtime pod** (not per agent). Each pod runs one instance of the Rust swarm binary, which hosts N virtual actors (agents). The sidecar:

- Handles actor placement, activation/deactivation (scale-to-zero)
- Proxies state store reads/writes
- Publishes/subscribes to NATS topics on behalf of actors
- Exposes local HTTP/gRPC on `localhost:3500` (HTTP) and `localhost:50001` (gRPC internal)

```
┌─────────────────────────────────────────────┐
│  swarm-runtime Pod                          │
│                                             │
│  ┌──────────────────┐  ┌─────────────────┐  │
│  │  Rust Runtime    │  │  daprd sidecar  │  │
│  │  (ractor actors) │◄─►│  :3500 HTTP    │  │
│  │  axum REST       │  │  :50001 gRPC    │  │
│  │  :8080           │  │  :9090 metrics  │  │
│  └──────────────────┘  └────────┬────────┘  │
│                                 │           │
└─────────────────────────────────┼───────────┘
                                  │  mTLS
                    ┌─────────────┼──────────────┐
                    │             │              │
                    ▼               ▼              ▼
               Turso L1       SurrealDB L2    NATS
               (embedded      (durable L2,   (pub/sub;
               SQLite, L1)    AWS→AF B300)    JetStream +
                                              Core)
```

### 2.2 Sidecar Resource Limits

These limits are calibrated for Akash Tier C (Zanthem, Subangle) standard provider capacity. Adjust for Tier A/B dedicated pods.

```yaml
# Per sidecar — adjust via Dapr annotations on the Deployment
resources:
  requests:
    cpu: "50m"
    memory: "64Mi"
  limits:
    cpu: "200m"
    memory: "256Mi"
```

**Rationale:**
- At 1,000 agents spread across ~10 pods (100 actors/pod), each sidecar handles ~100 concurrent actor activations
- 256Mi peak covers Dapr's in-memory actor placement table + component client connection pools
- 200m CPU limit prevents sidecar from starving the Rust runtime under bursty activation

### 2.3 TLS: Sidecar ↔ Runtime

Dapr Sidecar-to-app communication is **localhost-only** (same pod network namespace). No TLS is applied on the `localhost:3500` path — the pod network boundary is the isolation unit here.

Sidecar-to-sidecar (control plane, Placement service) uses **Dapr's built-in mTLS** with SPIFFE/SPIRE-compatible certificates managed by the Dapr control plane:

```yaml
# In dapr-system namespace
apiVersion: dapr.io/v1alpha1
kind: Configuration
metadata:
  name: swarm-runtime-config
  namespace: swarm-runtime
spec:
  mtls:
    enabled: true
    workloadCertTTL: "24h"
    allowedClockSkew: "15m"
```

For state store connections:
- Turso L1 (libSQL/SQLite): embedded per-pod, no network TLS — isolation is the pod boundary; PVC encrypted at rest by the provider's storage class. No external connection string.
- SurrealDB L2: TLS 1.3 with client cert over internal cluster network (AWS VPC Phase A; AF bare-metal Phase B). Connection string injected via Infisical secret store.
- NATS: mTLS 1.3 + NKey auth (existing EC2 NATS config, per MEMORY.md)

### 2.4 Actor Placement and Reminder Config

```yaml
spec:
  actors:
    # How long an idle actor stays activated before Dapr deactivates it
    actorIdleTimeout: "5m"
    # How often Dapr scans for actors to deactivate
    actorScanInterval: "30s"
    # Max concurrent active actors per sidecar
    # At 100 actors/pod × 10 pods = 1,000 target
    drainOngoingCallTimeout: "30s"
    drainRebalancedActors: true
    # Reentrancy: allow an actor to call itself (needed for recursive DAG traversal)
    reentrancy:
      enabled: true
      maxStackDepth: 32
    # Reminder storage: use Redis (hot tier) for low-latency reminder delivery
    remindersStoragePartitions: 0  # TODO: tune after load testing
```

---

## 3. State Store Selection

> **OQ-9 Resolution (#144):** The original Redis (hot) + PostgreSQL (cold) design is superseded by
> **Turso L1 (embedded libSQL/SQLite, per-pod)** + **SurrealDB L2 (cluster, durable checkpoints)**.
> Redis StatefulSet and PostgreSQL component are removed. Dapr sidecar is retained for pub/sub and
> workflow; the state path bypasses Dapr's state store API and goes directly through the Rust runtime's
> `StateClient` / `RichStateClient` traits (per integration-contract OQ-9 addendum §9.3).

### 3.1 Tier Description

| Tier | Technology | Latency | Durability | Substrate |
|---|---|---|---|---|
| **L1 (hot)** | Turso libSQL (embedded SQLite) | <0.1ms (local file) | Per-pod PVC, AOF via WAL | Akash pod PVC (co-located with runtime) |
| **L2 (durable)** | SurrealDB 3-node Raft cluster | ~1-3ms | ACID, Raft quorum, NVMe | AWS Phase A → AF bare-metal Phase B (§10) |

**Why Turso L1 over Redis:**
- Sub-millisecond reads from a local SQLite file — no network hop to a separate Redis pod
- No eviction risk (deterministic PVC storage, not memory-pressure LRU)
- SQLite WAL mode provides ACID durability for the hot working state
- Zero additional infrastructure within the execution pod — libSQL is a Rust library dependency

**Why SurrealDB L2 over PostgreSQL:**
- Multi-model: graph + document + relational in one store — accommodates the capability graph,
  temporal facts, and structured agent snapshots without schema explosion
- Raft-native HA without external Sentinel/Patroni layer
- Per issue #168 Phase B: runs on AF bare-metal bare-metal NVMe — not on Akash (state stores are
  stable-tier workloads; see §10)

**Redis and PostgreSQL are fully removed from this design.** OQ-I5 (Redis Sentinel vs Cluster) is
closed — no longer applicable.

### 3.2 Hydration Flow

```
Actor Activation Request
         │
         ▼
┌────────────────────────────┐
│  Turso L1 (embedded libSQL)│  ← Working state: conversation turns, tool call cache,
│  per-pod SQLite file       │    working memory slots, routing metadata
│  /data/turso/actor.db      │    TTL: 5 min WAL flush → L2 on idle
│  (StateClient interface)   │
└────────┬───────────────────┘
         │ MISS (actor not in local file, or pod rescheduled)
         ▼
┌────────────────────────────┐
│  SurrealDB L2 (cluster)    │  ← Durable checkpoints: full AgentSnapshot blobs,
│  3-node Raft               │    FHE-encrypted RESTRICTED state, audit log entries,
│  swarm.actor_state table   │    capability graph, temporal facts
│  (RichStateClient trait)   │    No TTL; retained per data retention policy
└────────────────────────────┘
```

**Write path:** The Rust runtime writes working state to the local Turso L1 SQLite file. On every
`actorIdleTimeout` expiry (or explicit checkpoint trigger), the runtime flushes an `AgentSnapshot`
to SurrealDB L2 via `RichStateClient::checkpoint()`. Clean actor deactivation always flushes before
eviction; the Dapr `OnDeactivate` hook triggers the final checkpoint.

**Read path (hydration):** Check Turso L1 first. On miss (cold pod or rescheduled pod with no local
DB), fetch the most recent `AgentSnapshot` from SurrealDB L2 and hydrate into L1.

**Dapr state store API bypass:** Because Turso L1 is an embedded library (not a network service),
Dapr's `state.*` component API is not used for the hot path. Dapr sidecar remains for pub/sub,
workflow, and secret store. The `StateClient` trait in `crates/af-swarm-state` abstracts the L1/L2
dispatch layer (renamed from `DaprStateClient` per integration-contract CL-001 / OQ-9 §9.3).

**Checkpoint policy (aligned with quinn #134 PR #143):**
- Mandatory on-freeze (every clean actor deactivation)
- Periodic: every 20 turns of conversation
- Pre-RESTRICTED-task: snapshot before executing any RESTRICTED-tagged task
- Recovery: torn-write detection via `version` mismatch + `integrity_hmac` verification (§3.3)

### 3.3 actor_state Schema (Turso L1 DDL + SurrealDB L2)

RESTRICTED-tier agent state is **always stored as FHE ciphertext** before write. The decryption key
never leaves the RESTRICTED POD (Tier A hardware). Both L1 and L2 store opaque ciphertext blobs for
RESTRICTED actors — the storage layer never sees plaintext.

**Turso L1 DDL (per-pod embedded SQLite):**

```sql
-- Per-pod SQLite schema — created on first pod startup
-- Coordination: quinn (#134) confirms column types; argus (#133) confirms key_id format
CREATE TABLE actor_state (
    actor_id         TEXT     NOT NULL,  -- "{agent_type}::{tenant_id}::{instance_id}"
    tenant_id        TEXT     NOT NULL,
    sensitivity_tier TEXT     NOT NULL,  -- 'PUBLIC' | 'INTERNAL' | 'SENSITIVE' | 'RESTRICTED'
    state_key        TEXT     NOT NULL,
    state_value      BLOB     NOT NULL,  -- prost-serialized bytes OR FHE ciphertext (RESTRICTED)
    is_encrypted     INTEGER  NOT NULL DEFAULT 0,   -- 1 if FHE-encrypted
    key_id           TEXT,               -- argus: FHE key reference for RESTRICTED tier
    -- OCC integrity layer (PR #141 §4.6) --
    version          INTEGER  NOT NULL DEFAULT 0,
    -- Monotonic u64 counter; incremented on every write.
    -- Writers must supply the current version; writes with a stale version are rejected
    -- (SQLite: UPDATE ... WHERE version = :expected_version; rows_affected == 0 → conflict).
    integrity_hmac   BLOB     NOT NULL,
    -- HMAC-SHA256 over (state_value || version_le64 || actor_id_bytes).
    -- Computed by the Rust runtime before write; verified on every read.
    -- Guards against file-level rollback attacks (Akash operator replaces PVC snapshot).
    -- argus (#133) owns the HMAC key derivation hierarchy (tenant root → agent subkey via HKDF).
    created_at       INTEGER  NOT NULL DEFAULT (unixepoch()),
    updated_at       INTEGER  NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (actor_id, state_key)
);

CREATE INDEX idx_actor_state_tenant ON actor_state(tenant_id, sensitivity_tier);
CREATE INDEX idx_actor_state_updated ON actor_state(actor_id, updated_at DESC);
```

**OCC and integrity_hmac verification protocol:**

*On write:*
1. `BEGIN IMMEDIATE` transaction (SQLite WAL exclusive write lock)
2. Read current `version` for `(actor_id, state_key)`
3. Compute `integrity_hmac = HMAC-SHA256(key_agent, state_value || le64(version+1) || actor_id)`
4. `UPDATE actor_state SET state_value=?, version=version+1, integrity_hmac=?, updated_at=?
   WHERE actor_id=? AND state_key=? AND version=:expected_version`
5. If `rows_affected == 0`: conflict → retry with fresh read (OCC retry loop, max 3 attempts)
6. `COMMIT`

*On read:*
1. Fetch `(state_value, version, integrity_hmac)`
2. Recompute `expected_hmac = HMAC-SHA256(key_agent, state_value || le64(version) || actor_id)`
3. Constant-time compare `expected_hmac == integrity_hmac`; reject if mismatch (raise FM-24: state
   integrity failure → escalate to MAST)

**The `etag` column from the original design is removed.** Dapr's ETag-based OCC is no longer
applicable because the Dapr state store API is bypassed for L1. The `version` + `integrity_hmac`
pair is the sole concurrency and integrity mechanism for L1 state.

**SurrealDB L2 schema (durable cluster — abbreviated):**

```surql
-- SurrealDB L2 — agent checkpoint table
DEFINE TABLE actor_checkpoint SCHEMAFULL;
DEFINE FIELD actor_id         ON actor_checkpoint TYPE string ASSERT $value != NONE;
DEFINE FIELD tenant_id        ON actor_checkpoint TYPE string ASSERT $value != NONE;
DEFINE FIELD sensitivity_tier ON actor_checkpoint TYPE string;
DEFINE FIELD snapshot_bytes   ON actor_checkpoint TYPE bytes;   -- prost AgentSnapshot blob
DEFINE FIELD is_encrypted     ON actor_checkpoint TYPE bool DEFAULT false;
DEFINE FIELD key_id           ON actor_checkpoint TYPE option<string>;
DEFINE FIELD version          ON actor_checkpoint TYPE int DEFAULT 0;
DEFINE FIELD integrity_hmac   ON actor_checkpoint TYPE bytes;   -- same HMAC scheme as L1
DEFINE FIELD created_at       ON actor_checkpoint TYPE datetime VALUE $before OR time::now();
DEFINE FIELD updated_at       ON actor_checkpoint TYPE datetime VALUE time::now();
DEFINE INDEX idx_tenant       ON actor_checkpoint FIELDS tenant_id, sensitivity_tier;
DEFINE INDEX idx_actor_time   ON actor_checkpoint FIELDS actor_id, updated_at;
```

**Open Question OQ-I1:** Actor ID format — should `actor_id` embed `tenant_id` as a prefix for ACL
separation, or rely solely on SurrealDB namespace/database scoping? The two approaches have different
performance characteristics under high-tenancy load. Coordination needed with argus (#133).

### 3.4 State Store Component Notes

With Turso L1 embedded in the Rust runtime binary (libSQL crate) and SurrealDB L2 accessed via the
`surrealdb` Rust client crate, **no Dapr `state.*` Component CRDs are required for the hot or cold
state path.** The K8s components.yaml and Akash SDL no longer include `statestore-hot` or
`statestore-cold` entries.

Connection config for SurrealDB L2 is injected via the Infisical secret store (see §2.3 and
components.yaml `swarm-secretstore`). The Turso L1 SQLite file path is set via the
`AF_SWARM_L1_DB_PATH` environment variable (default: `/data/turso/actor.db`), which maps to the
pod's persistent volume claim.

---

## 4. Pub/Sub: NATS Component Config

> **PR #141 §3.1 resolution:** NATS topics are split into two classifications. A single JetStream
> Dapr component is insufficient — JetStream enforces durability and ack semantics on every topic,
> which is wrong for advisory/ephemeral subjects (`broadcast`, `backpressure`, `heartbeat`). Two
> Dapr Component CRDs are defined: one JetStream binding for durable topics, one Core NATS binding
> for advisory topics.

RFC-001 confirms reuse of existing NATS cluster (EC2, `44.223.242.234:4222`, auth: af/***). The
Dapr NATS Streaming component is **not** used.

### 4.1 Topic Classification

| Class | Subjects | Binding | Durability | Delivery guarantee |
|---|---|---|---|---|
| **Durable** (JetStream) | `agent.task.*` | `pubsub-nats-js` | JetStream stream `SWARM`, Raft | At-least-once, ack required |
| **Durable** (JetStream) | `agent.result.*` | `pubsub-nats-js` | JetStream stream `SWARM`, Raft | At-least-once, ack required |
| **Durable** (JetStream) | `agent.audit.*` | `pubsub-nats-js` | JetStream stream `SWARM`, Raft | At-least-once, ack required |
| **Durable** (JetStream) | `agent.lifecycle.*` | `pubsub-nats-js` | JetStream stream `SWARM`, Raft | At-least-once, ack required |
| **Advisory** (Core) | `agent.broadcast` | `pubsub-nats-core` | None (fire-and-forget) | Best-effort, no ack |
| **Advisory** (Core) | `agent.backpressure.*` | `pubsub-nats-core` | None | Best-effort, no ack |
| **Advisory** (Core) | `agent.heartbeat.*` | `pubsub-nats-core` | None | Best-effort, no ack |

**Rationale for the split:**
- Durable topics (`task`, `result`, `audit`, `lifecycle`) are state-changing operations that must
  survive pod restarts and network partitions. JetStream ack semantics prevent silent message loss.
- Advisory topics (`broadcast`, `backpressure`, `heartbeat`) are ephemeral signals. Missed heartbeats
  are tolerated (KEDA backpressure recovers on the next message). JetStream durability would add
  unnecessary storage overhead and head-of-line blocking for these high-frequency signals.

**Client-side enforcement:** The Rust runtime's `NatsEnvelope` (`pubsub-nats.proto`) carries a
`topic_class` field (`DURABLE | ADVISORY`). The runtime dispatcher rejects attempts to publish a
durable-class topic via `pubsub-nats-core` and vice versa. This is a hard runtime assertion, not a
convention.

### 4.2 JetStream Component (Durable Topics)

```yaml
apiVersion: dapr.io/v1alpha1
kind: Component
metadata:
  name: pubsub-nats-js
  namespace: swarm-runtime
spec:
  type: pubsub.jetstream
  version: v1
  metadata:
    - name: natsURL
      secretKeyRef:
        name: swarm-nats-secret
        key: url     # nats://af:***@44.223.242.234:4222
    - name: name
      value: "swarm-runtime-js"
    - name: durableName
      value: "swarm-actor-events"
    - name: streamName
      value: "SWARM"
    - name: deliverNew
      value: "true"
    - name: flowControl
      value: "true"
    - name: ackWaitTime
      value: "30s"
    - name: maxInFlight
      value: "100"    # Cap in-flight messages per sidecar; tune based on load test
    # TODO: Set backoff strategy for message redelivery (coordinate with MAST FM taxonomy)
auth:
  secretStore: swarm-secretstore
```

### 4.3 Core NATS Component (Advisory Topics)

```yaml
apiVersion: dapr.io/v1alpha1
kind: Component
metadata:
  name: pubsub-nats-core
  namespace: swarm-runtime
spec:
  type: pubsub.nats
  version: v1
  metadata:
    - name: natsURL
      secretKeyRef:
        name: swarm-nats-secret
        key: url     # Same NATS cluster; Core NATS protocol (no stream persistence)
    - name: name
      value: "swarm-runtime-core"
    # No durableName, streamName, or ackWaitTime — Core NATS is fire-and-forget
    - name: natsQueueGroupName
      value: "swarm-advisory"   # Shared queue group for load balancing heartbeat/backpressure
auth:
  secretStore: swarm-secretstore
```

### 4.4 Topic Routing Table

| Dapr topic | NATS subject | Component | Purpose |
|---|---|---|---|
| `agent.task.dispatch` | `SWARM.task.dispatch` | `pubsub-nats-js` | New task to any agent (orchestrator → swarm) |
| `agent.task.{agent_type}` | `SWARM.task.{agent_type}` | `pubsub-nats-js` | Targeted dispatch |
| `agent.result.{task_id}` | `SWARM.result.{task_id}` | `pubsub-nats-js` | Task completion payload |
| `agent.audit.event` | `SWARM.audit.event` | `pubsub-nats-js` | SEC-00x audit log entries |
| `agent.lifecycle.activate` | `SWARM.lifecycle.activate` | `pubsub-nats-js` | Actor activation notice |
| `agent.lifecycle.deactivate` | `SWARM.lifecycle.deactivate` | `pubsub-nats-js` | Actor deactivation + checkpoint ack |
| `agent.broadcast` | `SWARM.broadcast` | `pubsub-nats-core` | System-wide events (config reload, shutdown) |
| `agent.backpressure.{pod_id}` | `SWARM.backpressure.{pod_id}` | `pubsub-nats-core` | Pod-level backpressure signals |
| `agent.heartbeat.{agent_id}` | `SWARM.heartbeat.{agent_id}` | `pubsub-nats-core` | Liveness keepalive |

**Compatibility note:** Existing `code.tasks.*` and `marketing.tasks.*` NATS subjects used by the
current TypeScript worker system are on different subjects and are not affected. The swarm runtime
uses the `SWARM.*` namespace exclusively.

---

## 5. Workflow Component

The Dapr Workflow component replaces the **basic DAG planner** portion of Phase 8 (RFC-001 phase delta: +20-30h savings applied here). It does NOT replace:
- W&D scheduler (Phase 13, still Rust)
- LAMaS CPO (Phase 13, still Rust)
- MAST failure detector (Phase 13, still Rust)

Dapr Workflow handles: sequential task chains, fan-out/fan-in patterns, retry with backoff, and timeout enforcement at the DAG level.

```yaml
apiVersion: dapr.io/v1alpha1
kind: Component
metadata:
  name: workflow-backend
  namespace: swarm-runtime
spec:
  type: workflow.dapr
  version: v1
  # No additional metadata needed — uses actor state store (statestore-hot)
```

**Workflow integration surface (Rust skeleton):**

```rust
// TODO Phase 8: Implement DaprWorkflowActivity trait for agent task execution
// This replaces the basic DAG planner described in v3.0 §4.4
//
// The Rust runtime calls Dapr workflow API via HTTP at localhost:3500/v1.0-beta1/workflows/
// W&D scheduler and MAST remain pure Rust; Dapr Workflow is the execution harness.

pub trait DaprWorkflowActivity: Send + Sync {
    fn activity_name(&self) -> &str;
    async fn execute(&self, input: WorkflowActivityInput) -> Result<WorkflowActivityOutput, WorkflowError>;
}

pub struct WorkflowActivityInput {
    pub workflow_id: String,
    pub activity_sequence: u32,
    pub agent_id: ActorId,
    pub task: AgentTask,
    pub context: WorkflowContext,
}

// OQ-I3: Boundary between Dapr Workflow (execution harness) and W&D scheduler
// (Phase 13) needs to be defined before Phase 8 implementation starts.
// Current assumption: Dapr Workflow = task graph execution; W&D = scheduling policy.
```

---

## 6. Scale-to-Zero Policy

### 6.1 Actor Idle Timeout

RFC-001 OQ-5 sets the load test SLO at: 1,000 agents × 10-token classification, P99 hydration <500ms.

| Agent Tier | Idle Timeout | Rationale |
|---|---|---|
| Orchestrator (Kimi K2.5 coordinator) | 30m | Frequently active; high re-activation cost |
| Code swarm agents (senku, lain, atlas, etc.) | 10m | Medium frequency, tolerate 30s warmup |
| Marketing swarm agents (nori, lyra, kai, etc.) | 5m | Lower frequency, batch-oriented |
| Routing/classification (Qwen2.5-0.5B) | 1m | Stateless, near-zero cold start |

**Open Question OQ-I2:** Should marketing and code swarm agents have different idle TTLs? Marketing batch tasks run on a schedule (overnight); keeping agents warm during the day wastes compute. Code agents respond to developer events that can happen any time. A per-agent-type TTL config in the agent TOML config (v3.0 §4.2) would resolve this.

### 6.2 Pod-Level Scale-to-Zero (HPA + KEDA)

Pod-level scaling uses **KEDA** (Kubernetes Event-Driven Autoscaling) with NATS JetStream as the trigger:

```yaml
apiVersion: keda.sh/v1alpha1
kind: ScaledObject
metadata:
  name: swarm-runtime-scaler
  namespace: swarm-runtime
spec:
  scaleTargetRef:
    name: swarm-runtime
  minReplicaCount: 0          # True scale-to-zero
  maxReplicaCount: 10         # 10 pods × 100 actors/pod = 1,000 target
  cooldownPeriod: 300         # 5 min after last message before scaling to 0
  triggers:
    - type: nats-jetstream
      metadata:
        natsServerMonitoringEndpoint: "44.223.242.234:8222"
        account: "$G"
        stream: "SWARM"
        consumer: "swarm-actor-events"
        lagThreshold: "100"   # Scale up when 100+ messages are pending
        activationLagThreshold: "1"  # Scale from 0 when any message arrives
```

**Cold-start budget (P99 target <500ms):**
| Step | Budget |
|---|---|
| KEDA scale-from-zero trigger | ~10s pod scheduling (Akash) |
| Pod pull + init containers | ~15s |
| Rust binary startup | <50ms (v3.0 target) |
| Dapr sidecar init | ~2s |
| Actor hydration from Redis | <1ms |
| **Total (warm pod)** | **<5ms** |
| **Total (cold pod, Akash)** | **~30s** |

**Implication:** P99 <500ms SLO is met for **warm pods** only. Cold pod startup (from 0 replicas) on Akash is ~30s — this is an Akash infrastructure constraint, not a software constraint. Recommendation: keep `minReplicaCount: 1` for the orchestrator pod in production; allow true scale-to-zero only for batch worker pods.

**Grace period and in-flight actor migration on pod termination:**

When KEDA scales a pod to zero (or Akash provider reclaims the pod), Dapr triggers `OnDeactivate`
for each active actor. The following protocol ensures no in-flight activations are silently dropped:

1. **Draining window:** KEDA's `cooldownPeriod: 300` means no scale-to-zero event fires while
   messages are in flight. However, a provider-initiated eviction (Akash lease close) does not
   respect cooldown — the pod receives `SIGTERM` and has `terminationGracePeriodSeconds` to complete.
2. **terminationGracePeriodSeconds = 60s** is set on the Deployment (see deployment.yaml). This
   gives active actors 60 seconds to finish their current task invocation before eviction. In-flight
   Dapr actor calls that exceed 60s will receive a 503 from the sidecar; the caller (JetStream
   consumer) will NACK and the message will be redelivered per `ackWaitTime: 30s`.
3. **Actor migration on scale-down:** When Dapr detects a pod is draining, the Placement service
   redistributes actor addresses to surviving pods. Actors with active reminders are re-registered
   on the new host; reminder delivery is temporarily paused during the migration window (~5s typical).
   Actors with no reminder (idle) are simply evicted and will re-hydrate from SurrealDB L2 on next
   activation.
4. **Checkpoint-before-eviction:** The `OnDeactivate` hook calls `StateClient::checkpoint_sync()`
   which flushes the Turso L1 state to SurrealDB L2 before the pod exits. If the flush does not
   complete within 55s (5s before SIGKILL), the runtime logs a WARNING and emits MAST FM-25
   (incomplete checkpoint on eviction); recovery on next activation reads the last successful L2
   checkpoint.

**MAST FM-25 (new):** Pod evicted with in-flight checkpoint flush incomplete. Recovery: activate
from last SurrealDB L2 checkpoint; replay any un-acked JetStream messages from the durable consumer
offset. Maximum data loss: one periodic checkpoint interval (≤20 conversation turns).

---

## 7. Observability

### 7.1 Dapr Metrics → Prometheus

Dapr exposes Prometheus metrics on `:9090/metrics` per sidecar. The swarm runtime's Prometheus config (v3.0 Phase 9) scrapes both the Rust runtime's own metrics and the Dapr sidecar metrics.

```yaml
# prometheus/scrape-configs.yaml (skeleton)
scrape_configs:
  - job_name: 'swarm-runtime'
    kubernetes_sd_configs:
      - role: pod
        namespaces:
          names: ['swarm-runtime']
    relabel_configs:
      - source_labels: [__meta_kubernetes_pod_annotation_dapr_io_enabled]
        action: keep
        regex: 'true'
      - source_labels: [__meta_kubernetes_pod_name]
        target_label: pod
      - source_labels: [__meta_kubernetes_pod_annotation_dapr_io_app_id]
        target_label: dapr_app_id
    # Scrape both app metrics (:8080) and Dapr sidecar metrics (:9090)
    static_configs: []  # Populated by SD
```

### 7.2 Key Dapr Metrics to Alert On

| Metric | Alert Threshold | Meaning |
|---|---|---|
| `dapr_runtime_actor_active_actors` | >900 (90% of target) | Approaching 1,000 capacity |
| `dapr_runtime_actor_timer_fired_total` | Rate drop to 0 for >5min | Reminder delivery failure |
| `dapr_runtime_component_pubsub_ingress_count` | Rate < baseline for >2min | NATS connectivity issue |
| `dapr_runtime_state_get_total` / `dapr_runtime_state_set_total` | P99 >50ms | State store latency regression |
| `dapr_runtime_actor_activation_total` / `dapr_runtime_actor_deactivation_total` | Ratio > 10:1 | Thrashing (TTL too short) |

### 7.3 Distributed Tracing

Dapr propagates W3C `traceparent` headers automatically (v3.0 §3.2 confirms W3C trace in NATS headers). The Rust runtime uses `tracing-opentelemetry` (v3.0 §8 tech stack). Dapr + Rust tracing → OpenTelemetry Collector → Jaeger/Zipkin. No additional config needed; ensure `OTEL_EXPORTER_OTLP_ENDPOINT` is set in the runtime pod env.

---

## 8. Security Integration Points

This section surfaces the interface between this infra track and argus (#133). argus is responsible for defining the specific policies; this doc identifies the hooks.

### 8.1 State Store ACLs (argus #133 owns)

Dapr does not natively support per-tenant state store ACLs at the component level. Isolation options:
1. **Separate Redis keyspace prefix per tenant** — simple, no enforcement (prefix is a convention)
2. **Separate Redis databases per tenant** — enforced by Redis auth, but Redis OSS limits to 16 DBs
3. **PostgreSQL Row-Level Security** — enforced at DB layer, scales to N tenants

**Recommendation (pending argus review):** PostgreSQL RLS for the cold tier (strong enforcement), Redis keyspace prefix for the hot tier (performance, convention-based). argus to define the RLS policy and confirm this is acceptable for SENSITIVE/RESTRICTED tiers.

### 8.2 Dapr Namespace Isolation for RESTRICTED POD

RESTRICTED agents (Tier A hardware, RFC-001 §1.2) run in a **separate Kubernetes namespace** (`swarm-runtime-restricted`) with:
- Dedicated Dapr control plane OR Dapr scoped to namespace with `allowedOrigins`
- No shared state store components with `swarm-runtime` (OPEN POOL)
- NetworkPolicy: no ingress from `swarm-runtime` namespace

**Per PR #138 (merged):** The RESTRICTED POD NetworkPolicy is finalized below. All ingress is
restricted to the RESTRICTED gateway only; egress is restricted to the RESTRICTED-pool Dapr
placement service and the FHE service. All other intra-cluster traffic is denied by default.

```yaml
# RESTRICTED POD isolation — NetworkPolicy (PR #138, merged)
# Namespace: swarm-runtime-restricted
# Default-deny: no Ingress/Egress unless explicitly allowed below.
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: restricted-pod-isolation
  namespace: swarm-runtime-restricted
spec:
  podSelector: {}        # Applies to ALL pods in swarm-runtime-restricted namespace
  policyTypes:
    - Ingress
    - Egress

  ingress:
    # Only allow inbound traffic from the RESTRICTED gateway pod
    # (the A2A gateway running in swarm-runtime-restricted with label role=restricted-gateway)
    - from:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: swarm-runtime-restricted
          podSelector:
            matchLabels:
              role: restricted-gateway
      ports:
        - protocol: TCP
          port: 8080    # Rust runtime app port
        - protocol: TCP
          port: 50051   # gRPC actor reminders
    # Allow Dapr sidecar ↔ Dapr control plane (placement service, within restricted namespace only)
    - from:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: swarm-runtime-restricted
          podSelector:
            matchLabels:
              app.kubernetes.io/name: dapr-placement
      ports:
        - protocol: TCP
          port: 50005   # Dapr placement gRPC

  egress:
    # Allow outbound only to RESTRICTED-pool Dapr placement service (within restricted namespace)
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: swarm-runtime-restricted
          podSelector:
            matchLabels:
              app.kubernetes.io/name: dapr-placement
      ports:
        - protocol: TCP
          port: 50005
    # Allow outbound to FHE service (TFHE-rs worker, restricted namespace)
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: swarm-runtime-restricted
          podSelector:
            matchLabels:
              role: fhe-service
      ports:
        - protocol: TCP
          port: 50052   # FHE compute gRPC
    # Allow outbound to SurrealDB L2 (RESTRICTED instance, stable tier — AWS VPC Phase A)
    # Phase A: SurrealDB L2 runs in aws-stable namespace (separate cluster or VPC peering)
    # Phase B: Update to AF bare-metal IP range per §10
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: aws-stable
          podSelector:
            matchLabels:
              role: surrealdb-restricted
      ports:
        - protocol: TCP
          port: 8000    # SurrealDB HTTP/WS
    # Allow NATS JetStream (durable topics only — via pubsub-nats-js)
    # NATS server is in the stable tier; allow egress to it
    - to:
        - ipBlock:
            cidr: 44.223.242.234/32   # EC2 NATS server (Phase A); update to AF bare-metal Phase B
      ports:
        - protocol: TCP
          port: 4222
    # Allow DNS resolution
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: kube-system
          podSelector:
            matchLabels:
              k8s-app: kube-dns
      ports:
        - protocol: UDP
          port: 53
        - protocol: TCP
          port: 53
```

**What this policy enforces (PR #138 §3.5 fail-closed proof):**
- A compromised OPEN POOL pod in `swarm-runtime` namespace **cannot** reach any pod in
  `swarm-runtime-restricted` — the ingress rule requires both namespace AND pod label match.
- A RESTRICTED POD **cannot** exfiltrate data to arbitrary external IPs — egress is whitelisted to
  placement, FHE service, SurrealDB L2 (stable tier), and NATS only.
- The FHE service (`role: fhe-service`) runs inside `swarm-runtime-restricted` — ciphertext never
  leaves the namespace boundary unencrypted.

### 8.3 FHE Ciphertext in State Store

FHE ciphertext blobs (TFHE-rs, v3.0 §5.4) are treated as opaque BYTEA in PostgreSQL. The state store does not need to understand ciphertext structure. The invariant:

- RESTRICTED actor state: **always** serialized as FHE ciphertext before the `statestore-cold` write call
- `is_encrypted = TRUE` and `key_id` are set by the Rust runtime, not Dapr
- Dapr state store sees only opaque bytes — key derivation and FHE context stay in the Rust runtime

This means **no FHE-specific Dapr component** is needed. The FHE layer is below the Dapr state API.

---

## 9. Open Questions

These must be resolved before Phase 1 implementation (RFC-001 phase delta table).

| ID | Question | Owner | Default if Unresolved |
|---|---|---|---|
| OQ-I1 | Actor ID format: embed `tenant_id` as prefix vs. rely solely on SurrealDB namespace/database scoping for ACL separation? | argus (#133) + this track | Embed `tenant_id` prefix AND use SurrealDB namespace scoping (defense in depth) |
| OQ-I2 | Per-agent-type idle TTL config — add to agent TOML schema or runtime config? | quinn (#134) | Add to agent TOML config |
| OQ-I3 | Phase 8 boundary: Dapr Workflow (execution harness) vs W&D scheduler (policy) — exact split? | Chief Architect + this track | Dapr Workflow = task DAG execution; W&D = scheduling priority |
| OQ-I4 | KEDA `minReplicaCount` for orchestrator pod in production — 0 or 1? | Human decision | 1 (keep warm for P99 SLO) |
| ~~OQ-I5~~ | ~~Redis cluster vs Redis Sentinel for hot tier HA on Akash?~~ | **CLOSED** — Redis removed (OQ-9 resolution #144). Turso L1 is per-pod embedded; no HA topology needed for L1. SurrealDB L2 is Raft-native HA. | N/A |
| OQ-I6 | Does RFC §1.2 "sub-50ms cold start" refer to actor hydration (Turso L1 read, <0.1ms) or full pod cold start (~30s on Akash)? The two are very different. **Possible RFC contradiction if intended as pod cold start.** | RFC author (chief-architect, #128) | Interpret as actor hydration on a warm pod; escalated via GitHub comment on #128 |
| OQ-I7 | Turso L1 PVC data survival on pod rescheduling: if a pod is moved to a different Akash node, the PVC may not follow (Akash PVC portability is provider-dependent). What is the acceptable data loss window if L1 is lost and L2 is the recovery point? | This track + quinn (#134) | Accept loss of up to one checkpoint interval (≤20 turns); document as FM-25 |

**Note on OQ-I6:** Escalated to RFC author via GitHub comment on issue #128 (see §9 note below and
non-blocker §9). With Turso L1, actor hydration latency is <0.1ms (local file read) — even better
than the original Redis <1ms. The "sub-50ms cold start" claim is unambiguously met for actor
hydration. Pod cold start on Akash (~30s) is a separate concern and does not change with this
redesign.

---

## 10. Substrate Migration per Issue #168

> This section acknowledges the Phase A → Phase B substrate migration defined in issue #168
> and constrains which workloads are permitted on Akash vs stable tier.

### 10.1 Target Topology

| Workload | Phase A (Q2 2026) | Phase B (Q3 2026) | Akash role |
|---|---|---|---|
| NATS JetStream (durable bus) | AWS EC2 (existing, 44.223.242.234) | AF B300 bare-metal, 3-node Raft | Advisory only (Core NATS) |
| SurrealDB L2 (state cluster) | AWS (new deployment) | AF B300 bare-metal, 3-node Raft, NVMe | Not on Akash |
| Dapr placement service | AWS (co-located with NATS/SurrealDB) | AF B300 bare-metal | Not on Akash |
| PG audit log | AWS RDS (existing) | AF bare-metal + off-site replica | Not on Akash |
| Inference (Kimi K2.5, vLLM) | AWS GPU instances | AF B300 (192GB HBM3e, NVLink5) | Not on Akash (OPEN POOL execution only) |
| FHE service (TFHE-rs) | AWS GPU (Nitro) | AF B300 bare-metal (GPU TFHE kernels) | Not on Akash |
| **Agent execution pods (OPEN POOL)** | **Akash Tier C (Zanthem, Subangle)** | **Akash primary + AF bare-metal burst** | **Akash owns execution pods** |
| SSL proxy / edge / static | Akash leet.haus (existing) | Akash leet.haus (unchanged) | Akash |
| DR / backup | AWS S3 + Glacier | AWS S3 + Glacier (unchanged) | N/A |

### 10.2 Akash SDL Scope Constraint

**Akash deploys execution-tier workloads only.** Specifically:

- `swarm-runtime` (Rust binary + Dapr sidecar): ✅ on Akash
- `dapr-sidecar`: ✅ on Akash (companion to swarm-runtime)
- State stores (Turso L1 is embedded — no separate pod): ✅ inherently on Akash pod PVC
- SurrealDB L2: ❌ NOT on Akash — runs on AWS (Phase A) then AF bare-metal (Phase B)
- NATS JetStream server: ❌ NOT on Akash — stable tier only
- Dapr placement service: ❌ NOT on Akash — stable tier only
- Redis: ❌ REMOVED from this design entirely

The `infrastructure/akash/swarm-runtime.sdl` is updated accordingly — the `redis` service block is
removed, and a comment documents that SurrealDB L2 and NATS JetStream run outside Akash.

### 10.3 Phase A Exit Criteria

- SurrealDB L2 3-node Raft cluster running on AWS, passing `swarm.actor_checkpoint` round-trip tests
- NATS JetStream migrated to separate EC2 instances (or retained on existing EC2, evaluated for Phase B)
- Dapr placement service deployed to AWS (separate from Akash execution pods)
- Akash SDL updated to remove all stable-tier services (SDL = execution pods only)
- Benchmark: SurrealDB L2 P99 checkpoint write <5ms from an Akash execution pod (cross-AZ acceptable latency)

### 10.4 Phase B Exit Criteria

- AF B300 bare-metal cluster provisioned (k3s or full k8s + NVIDIA GPU Operator)
- NATS JetStream 3-node Raft migrated to AF bare-metal; existing EC2 NATS decommissioned
- SurrealDB L2 migrated to AF bare-metal NVMe; AWS SurrealDB decommissioned
- Dapr placement service co-located with NATS/SurrealDB on AF bare-metal
- Akash SDL unchanged (execution pods only; their L2 endpoint env var is updated to AF bare-metal IP)

### 10.5 Impact on This Document

Sections that reference "AWS" or "EC2" as the SurrealDB/NATS substrate will be updated in a
follow-up PR at Phase B cutover. The Akash SDL (`swarm-runtime.sdl`) must be updated at Phase A
cutover to remove `redis` and add `AF_SWARM_L2_URL` env var pointing to the AWS SurrealDB endpoint.

---

## 11. Coordination Checklist

### For argus (#133 — security)
- [ ] Review §3.3 FHE ciphertext schema and confirm `key_id` format + HMAC key derivation hierarchy
- [ ] Review `integrity_hmac` HMAC-SHA256 scheme (§3.3) — confirm HKDF sub-key derivation is correct
- [ ] Confirm RESTRICTED POD NetworkPolicy (§8.2) — rules finalized per PR #138 (merged)
- [ ] Sign off on OQ-I1 (actor ID format vs SurrealDB namespace scoping)

### For quinn (#134 — state)
- [ ] Review §3.2 Turso L1 + SurrealDB L2 hydration flow
- [ ] Confirm Turso L1 SQLite DDL column types and indexes (§3.3) — aligned with PR #143
- [ ] Confirm checkpoint intervals in §3.2 match PR #143 checkpoint policy (20 turns + on-freeze)
- [ ] Sign off on OQ-I2 (per-agent-type TTL in TOML schema)
- [ ] Confirm `version` OCC + `BEGIN IMMEDIATE` aligns with FM-21 (torn write) definition in PR #143

### For chief-architect (#128 — RFC author)
- [ ] OQ-I6 escalated via GitHub comment on #128 — awaiting response on "sub-50ms cold start" scope
- [ ] Confirm Phase 8 boundary (OQ-I3): Dapr Workflow vs W&D scheduler split

---

## Appendix A: Akash Provider Notes

From MEMORY.md confirmed deployments:

| Provider | DSEQ | Use in Swarm Runtime |
|---|---|---|
| Zanthem | 25465123 (staging) | Primary Tier C GPU node for OPEN POOL inference |
| Subangle | 24756776 (auth) | Tier C standard compute for swarm runtime pods |
| Europlots | 24672527 (secrets) | Tier C for Infisical secrets service (existing) |

**Provider selection in SDL:** Akash SDL `placement` blocks will name these providers explicitly for deterministic scheduling on known-good hardware. New provider discovery (e.g. GPU-capable Tier B nodes for RESTRICTED POD) is deferred to infrastructure team per RFC OQ-3.

**Yggdrasil note:** DNS broken (provider.provider.yggdrasil-compute.com), excluded from SDL targets per MEMORY.md.

**Platform requirement:** All Rust binaries built with `--platform linux/amd64` for lem0n.cc and Akash x86-64 hosts.
