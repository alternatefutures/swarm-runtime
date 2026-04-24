# Durable State Schema Design

**Issue:** [alternatefutures/admin#134](https://github.com/alternatefutures/admin/issues/134)
**Author:** Quinn
**Date:** 2026-04-20
**Status:** ACCEPTED — OQ-9 tiered state architecture (Turso L1 + SurrealDB L2) adopted 2026-04-20; supersedes addendum in #146
**Amends:** [rust-swarm-runtime.md v3.0](https://github.com/alternatefutures/admin/blob/main/docs/architecture/rust-swarm-runtime.md) Phase 1 + Phase 6 state contracts, aligned with [RFC-001](https://github.com/alternatefutures/admin/blob/feat/issue-127-1000-agent-amendment/docs/architecture/reviews/1000-agent-amendment.md) §1.2 (virtual actor model)
**Ground Truth:** RFC-001 §1.2 (agent lifecycle), v3.0 §4.3 (memory), §5.3 (DSE), §5.4 (FHE)
**Architecture decision:** OQ-9 accepted — Turso L1 (embedded SQLite replica, Akash-native, follows agent pods) + SurrealDB L2 (cluster-wide, AWS for stable Raft quorum) replaces Redis/PostgreSQL/LanceDB. Do not reopen this decision.

---

## 1. Design Goals

This document specifies the durable state layer that enables the virtual actor model described in RFC-001 §1.2. Agents are addressable identities that hydrate from and freeze to this store. The store must:

1. **Scale to 1,000+ agents** without per-agent DDL blowup
2. **Turso L1** — embedded SQLite replica per Akash node; zero-network reads for agent hydration
3. **SurrealDB L2** — cluster-wide (AWS, stable Raft quorum); orchestrator-only queries for capability graph + memory embeddings
4. **Support FHE-encrypted payloads** for RESTRICTED-tier agents (v3.0 §5.4)
5. **Maintain sub-50ms cold start** as specified in RFC-001 §1.2
6. **Dapr activation unchanged** — Dapr still handles actor activation/deactivation lifecycle; state read/write bypasses Dapr state API and goes directly to Turso L1

---

## 2. Serialization Format: Protocol Buffers (prost)

**Decision: protobuf via prost.**

| Format | Cross-language | Schema evolution | L3 wire (NATS) | Binary size |
|--------|---------------|-----------------|-----------------|-------------|
| **protobuf (prost)** | ✅ Yes | ✅ Field numbers | ✅ Native | Small |
| MessagePack | ✅ Yes | ⚠️ Manual versioning | ✅ Yes | Small |
| bincode | ❌ Rust-only | ❌ No stability guarantees | ❌ No | Smallest |

**Rationale:**

- The v3.0 plan already mandated `prost` for NATS persistence (§12 tech stack table). This decision is unchanged.
- After OQ-9: protobuf blobs are stored in SQLite `BLOB` columns (Turso L1) rather than as Dapr KV values. The serialization format is identical; only the container changes.
- For L3 NATS wire (`AgentSnapshotTransfer`): prost remains the format (CL-003 from integration-contract.md). The "JSON for Dapr state" half of CL-003 is superseded — Turso L1 SQL rows replace Dapr KV, so there is no Dapr-native JSON encoding. Prost is the single serialization format.
- bincode is ruled out: future Python analytics jobs (Ray Serve cost attribution, RFC-001 §9) cannot consume it.
- Schema evolution via field numbers is required for the migration strategy (§7.4).

> **Coordination note — lain (#131):** Serialization format is protobuf/prost. Ractor actor state snapshots written to Turso L1 must be protobuf blobs. The conversion boundary (if lain's in-memory format differs) lives at the Turso write path inside `af-swarm-state`, not in the actor code. For L3 NATS wire, `AgentSnapshotTransfer` uses prost (CL-003 confirmed).

**Proto files** are under `crates/af-swarm-state/proto/`. The build pipeline uses `prost-build` + `tonic-build` to generate Rust types in `crates/af-swarm-state/src/generated/`.

---

## 3. State Store Schema (Turso L1)

The Dapr flat KV composite key format is replaced by proper SQL tables in the Turso embedded SQLite replica. One database file per Akash node, shared across all agents on that node. All agents share the same five tables, partitioned by `(tenant_id, agent_id)` composite primary key — this preserves the original no-per-agent-DDL constraint at 1,000+ agent scale.

### 3.1 Table Definitions

```sql
-- crates/af-swarm-state/migrations/001_initial.sql

-- Primary agent state (read on every hydration — keep small)
CREATE TABLE agent_state (
    tenant_id           TEXT NOT NULL,
    agent_id            TEXT NOT NULL,
    agent_version       TEXT NOT NULL,
    status              TEXT NOT NULL CHECK(status IN ('FROZEN','ACTIVE','DRAINING')),
    last_frozen_at      INTEGER,            -- Unix epoch ms
    last_hydrated_at    INTEGER,            -- Unix epoch ms
    hydration_count     INTEGER NOT NULL DEFAULT 0,
    current_task_id     TEXT,
    current_task_taint  BLOB,               -- serialized DSE TaintContext proto
    working_memory_ver  INTEGER NOT NULL DEFAULT 0,
    total_turns         INTEGER NOT NULL DEFAULT 0,
    latest_turn_id      TEXT,               -- ULID of most recent turn
    goal_stack          BLOB,               -- protobuf-encoded repeated GoalFrame
    pinned_facts        BLOB,               -- protobuf-encoded repeated PinnedFact (max 64)
    surreal_record_ids  TEXT,               -- JSON array of SurrealDB record IDs (L2 memory index)
    encryption_envelope BLOB,               -- EncryptionEnvelope proto when tier >= SENSITIVE
    schema_version      INTEGER NOT NULL DEFAULT 1,
    version             INTEGER NOT NULL DEFAULT 0,   -- OCC counter (= seq_no for replay protection)
    integrity_hmac      BLOB,               -- HMAC-SHA256(tenant_id||agent_id||version||state_snapshot)
    PRIMARY KEY (tenant_id, agent_id)
);

-- Working memory (lazy-loaded separately from agent_state)
CREATE TABLE working_memory (
    tenant_id       TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    version         INTEGER NOT NULL DEFAULT 0,       -- OCC counter (= seq_no)
    integrity_hmac  BLOB,               -- HMAC-SHA256(tenant_id||agent_id||version||slots)
    slots           BLOB NOT NULL,      -- protobuf-encoded repeated MemorySlot (max 32)
    eviction_cursor INTEGER NOT NULL DEFAULT 0,
    scratchpad      BLOB,               -- protobuf-encoded repeated ScratchpadEntry, max 64KB
    active_goal_ids TEXT,               -- JSON array of goal_ids
    snapshot_at     INTEGER NOT NULL,   -- Unix epoch ms
    schema_version  INTEGER NOT NULL DEFAULT 1,
    expires_at      INTEGER,            -- Unix epoch ms; NULL = no TTL (see §9)
    PRIMARY KEY (tenant_id, agent_id)
);

-- Conversation turns (append-only, one row per turn)
CREATE TABLE conversation_turns (
    tenant_id       TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    turn_id         TEXT NOT NULL,      -- ULID (lexicographically sortable)
    role            TEXT NOT NULL,      -- 'user', 'assistant', 'tool_result'
    content         BLOB NOT NULL,      -- protobuf MessageContent
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    taint_context   BLOB,               -- protobuf TaintContext
    tool_log_ids    TEXT,               -- JSON array of tool_call_log.log_id values
    created_at      INTEGER NOT NULL,   -- Unix epoch ms
    schema_version  INTEGER NOT NULL DEFAULT 1,
    expires_at      INTEGER,            -- Unix epoch ms (compliance TTL)
    PRIMARY KEY (tenant_id, agent_id, turn_id)
);

-- Tool call audit log (append-only, immutable after write)
CREATE TABLE tool_call_log (
    tenant_id       TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    log_id          TEXT NOT NULL,      -- ULID
    turn_id         TEXT NOT NULL,
    tool_name       TEXT NOT NULL,
    input           BLOB,               -- sanitized tool input proto
    output          BLOB,               -- sanitized tool output proto
    status          TEXT NOT NULL,      -- 'SUCCESS','FAILED','TIMEOUT','SANDBOX_VIOLATION'
    duration_ms     INTEGER NOT NULL DEFAULT 0,
    taint_context   BLOB,
    invoked_at      INTEGER NOT NULL,   -- Unix epoch ms
    schema_version  INTEGER NOT NULL DEFAULT 1,
    expires_at      INTEGER,            -- Unix epoch ms (compliance TTL)
    PRIMARY KEY (tenant_id, agent_id, log_id)
);

-- Point-in-time recovery checkpoints (last 5 per agent by default)
CREATE TABLE checkpoints (
    tenant_id           TEXT NOT NULL,
    agent_id            TEXT NOT NULL,
    checkpoint_seq      INTEGER NOT NULL,   -- monotonic, ascending
    trigger             TEXT NOT NULL,      -- 'freeze','periodic','pre_task','manual'
    state_snapshot      BLOB NOT NULL,      -- full AgentStateRecord proto
    memory_snapshot     BLOB NOT NULL,      -- full WorkingMemoryRecord proto
    sha256_digest       BLOB NOT NULL,      -- SHA-256 of (state + memory) bytes
    created_at          INTEGER NOT NULL,
    schema_version      INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (tenant_id, agent_id, checkpoint_seq)
);

-- Indexes for orchestrator and GC queries
CREATE INDEX idx_agent_state_status ON agent_state(status);
CREATE INDEX idx_turns_agent_time ON conversation_turns(tenant_id, agent_id, turn_id);
CREATE INDEX idx_tool_log_turn ON tool_call_log(tenant_id, agent_id, turn_id);
CREATE INDEX idx_checkpoints_agent_seq ON checkpoints(tenant_id, agent_id, checkpoint_seq DESC);
CREATE INDEX idx_working_memory_expires ON working_memory(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX idx_turns_expires ON conversation_turns(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX idx_tool_log_expires ON tool_call_log(expires_at) WHERE expires_at IS NOT NULL;
```

### 3.2 Record Types

| Table | Proto blob columns | Size budget | TTL policy |
|-------|-------------------|-------------|-----------|
| `agent_state` | `goal_stack`, `pinned_facts`, `encryption_envelope` | ≤64 KB total | No TTL — persists until agent deleted |
| `working_memory` | `slots`, `scratchpad` | ≤256 KB | 24h idle (`expires_at`) |
| `checkpoints` | `state_snapshot`, `memory_snapshot` | ≤512 KB | Retain last N (default: 5) via GC |
| `conversation_turns` | `content`, `taint_context` | ≤32 KB | 30-day default (`expires_at`) |
| `tool_call_log` | `input`, `output`, `taint_context` | ≤16 KB | 90-day default (`expires_at`) |

### 3.3 Key Design Notes

- **No per-agent-table DDL blowup.** All 1,000+ agents share five tables partitioned by `(tenant_id, agent_id)` composite PK.
- **`version` column = OCC counter + seq_no.** Monotonically incremented on every write. Replaces Dapr ETags. Satisfies Argus's `seq_no` requirement for replay attack detection (PR #151 §AD-1). See §4.
- **`integrity_hmac` column = per-row tamper detection.** HMAC-SHA256 over `(tenant_id || agent_id || version || primary_blob)` using the tenant's KeyProvider subkey. Required for SENSITIVE/RESTRICTED tiers; applied to all tiers in Phase 6+. Satisfies Argus's per-row HMAC requirement (PR #151 §AD-1).
- **`surreal_record_ids` replaces `lancedb_record_ids`.** JSON array pointing to SurrealDB L2 records.
- **BLOB columns hold protobuf bytes.** Format (§2) unchanged; only the container changes from Dapr KV to SQLite BLOB.
- **One Turso database per node.** Reads are zero-network (embedded replica). Writes sync to Turso remote asynchronously (see OQ-S7).

### 3.4 Protobuf Schema (unchanged)

The proto message definitions (`AgentStateRecord`, `WorkingMemoryRecord`, `ConversationTurnRecord`, `ToolCallLogRecord`, `CheckpointRecord`) are unchanged from the original design — protobuf blobs are stored in the BLOB columns above. See `crates/af-swarm-state/proto/` for the full definitions. Key field mapping:

- `AgentStateRecord.lancedb_record_ids` (field 16) → renamed `surreal_record_ids` (same type: `repeated string`)
- `AgentStateRecord.working_memory_key` (field 10) — no longer a Dapr KV key string; implicit via `(tenant_id, agent_id)` composite key
- All other fields: unchanged

### 3.5 AgentStateRecord (primary state — for reference)

This is the root record read on every agent hydration. It must be small and fast — all bulk data (working memory, conversation history) is lazy-loaded via separate keys.

```protobuf
// See: crates/af-swarm-state/proto/agent_state.proto

message AgentStateRecord {
  // Identity
  string tenant_id = 1;
  string agent_id = 2;
  string agent_version = 3;  // semver of the agent config that produced this state

  // Lifecycle
  AgentLifecycleStatus status = 4;  // FROZEN, ACTIVE, DRAINING
  google.protobuf.Timestamp last_frozen_at = 5;
  google.protobuf.Timestamp last_hydrated_at = 6;
  uint64 hydration_count = 7;

  // Active task context (null when FROZEN)
  string current_task_id = 8;  // empty if FROZEN
  bytes  current_task_taint = 9;  // DSE TaintContext protobuf, empty if FROZEN

  // Working memory reference (lazy-loaded separately)
  // Note: no explicit key field needed — working_memory is looked up by (tenant_id, agent_id) in Turso L1
  uint64 working_memory_version = 11;  // must match working_memory.version column

  // Conversation history summary
  uint64 total_turns = 12;
  string latest_turn_id = 13;  // ULID of most recent ConversationTurnRecord

  // Goal stack
  repeated GoalFrame goal_stack = 14;  // current active goals, ordered outermost-first

  // Pinned facts (agent cross-session memory, bounded by max_pinned_facts)
  repeated PinnedFact pinned_facts = 15;  // max 64 entries

  // Embedding index references (IDs into SurrealDB L2 — no vectors inline)
  repeated string surreal_record_ids = 16;  // reference only; vectors live in SurrealDB L2

  // Encryption metadata
  EncryptionEnvelope encryption = 17;  // present when sensitivity >= SENSITIVE

  // Schema versioning
  uint32 schema_version = 18;  // monotonic integer for migration dispatch
}

enum AgentLifecycleStatus {
  AGENT_LIFECYCLE_STATUS_UNSPECIFIED = 0;
  FROZEN = 1;    // idle, persisted, consuming zero CPU/RAM
  ACTIVE = 2;    // hydrated, processing a task
  DRAINING = 3;  // finishing current task before freeze
}

message GoalFrame {
  string goal_id = 1;
  string description = 2;
  GoalStatus status = 3;
  google.protobuf.Timestamp created_at = 4;
  repeated string sub_goal_ids = 5;
}

enum GoalStatus {
  GOAL_STATUS_UNSPECIFIED = 0;
  PENDING = 1;
  IN_PROGRESS = 2;
  BLOCKED = 3;
  COMPLETED = 4;
  FAILED = 5;
}

message PinnedFact {
  string fact_id = 1;
  string content = 2;  // plain text, max 2KB
  float  relevance_score = 3;  // 0.0–1.0, updated on access
  google.protobuf.Timestamp pinned_at = 4;
  google.protobuf.Timestamp last_accessed_at = 5;
  string source_turn_id = 6;  // which ConversationTurnRecord produced this fact
}
```

### 3.4 WorkingMemoryRecord

Represents MemAgent's 32-slot fixed-size working context (v3.0 §4.3). Stored separately from `AgentStateRecord` to avoid bloating the hot hydration path.

```protobuf
message WorkingMemoryRecord {
  string tenant_id = 1;
  string agent_id = 2;
  uint64 version = 3;  // must match AgentStateRecord.working_memory_version

  // Fixed-capacity context window (MemAgent O(1) model — max 32 slots)
  repeated MemorySlot slots = 4;  // max cardinality: 32
  uint32 eviction_cursor = 5;     // LRU eviction pointer

  // Scratchpad (ephemeral, cleared on task completion)
  bytes scratchpad_bytes = 6;  // serialized ScratchpadEntry list, max 64KB

  // Active goals snapshot (denormalized from AgentStateRecord for working context)
  repeated string active_goal_ids = 7;

  google.protobuf.Timestamp snapshot_at = 8;
  uint32 schema_version = 9;
}

message MemorySlot {
  uint32 slot_index = 1;    // 0–31
  string content = 2;       // text content, max 4KB
  float  relevance_score = 3;
  google.protobuf.Timestamp last_accessed_at = 4;
  bool   is_pinned = 5;     // pinned slots are exempt from LRU eviction
  string source_id = 6;     // which turn_id or tool_log_id produced this content
  // Note: embedding for this slot is stored in SurrealDB L2, referenced
  // via AgentStateRecord.surreal_record_ids — not stored here
}

message ScratchpadEntry {
  string key = 1;
  string value = 2;
  google.protobuf.Timestamp written_at = 3;
}
```

### 3.5 ConversationTurnRecord

Individual conversation turns. Stored per-turn under `conv||{turn_id}`. The turn ULID provides time-ordered iteration without a timestamp field index.

```protobuf
message ConversationTurnRecord {
  string tenant_id = 1;
  string agent_id = 2;
  string turn_id = 3;   // ULID — lexicographically sortable, time-ordered

  string role = 4;      // "user", "assistant", "tool_result"
  bytes  content = 5;   // serialized MessageContent (text, tool_use, tool_result)
  uint64 input_tokens = 6;
  uint64 output_tokens = 7;

  // Taint propagation (DSE)
  bytes  taint_context = 8;  // serialized TaintContext

  // Reference to tool calls made in this turn (lazy-loaded separately)
  repeated string tool_log_ids = 9;

  google.protobuf.Timestamp created_at = 10;
  uint32 schema_version = 11;
}
```

### 3.6 ToolCallLogRecord

Immutable append-only record for tool invocations. Used for audit trail (S7) and DSE taint tracing.

```protobuf
message ToolCallLogRecord {
  string tenant_id = 1;
  string agent_id = 2;
  string log_id = 3;    // ULID
  string turn_id = 4;   // parent ConversationTurnRecord

  string tool_name = 5;
  bytes  input = 6;     // serialized tool input (sanitized — secrets redacted)
  bytes  output = 7;    // serialized tool output (sanitized)
  ToolCallStatus status = 8;
  uint32 duration_ms = 9;

  // DSE taint on this tool invocation
  bytes  taint_context = 10;

  google.protobuf.Timestamp invoked_at = 11;
  uint32 schema_version = 12;
}

enum ToolCallStatus {
  TOOL_CALL_STATUS_UNSPECIFIED = 0;
  SUCCESS = 1;
  FAILED = 2;
  TIMEOUT = 3;
  SANDBOX_VIOLATION = 4;
}
```

### 3.7 CheckpointRecord

Full agent state snapshot for point-in-time recovery. Used on cold start when `AgentStateRecord` is intact but working memory is inconsistent (torn write detection).

```protobuf
message CheckpointRecord {
  string tenant_id = 1;
  string agent_id = 2;
  uint64 checkpoint_seq = 3;  // monotonic sequence number
  string trigger = 4;  // "freeze", "periodic", "pre_task", "manual"

  // Embedded snapshots at checkpoint time
  AgentStateRecord state_snapshot = 5;
  WorkingMemoryRecord memory_snapshot = 6;

  // Integrity
  bytes sha256_digest = 7;  // SHA-256 of (state_snapshot + memory_snapshot) bytes

  google.protobuf.Timestamp created_at = 8;
  uint32 schema_version = 9;
}
```

---

## 4. Optimistic Concurrency: SQLite Transactions

The Dapr ETag read-modify-write pattern is replaced by the `version` column + `BEGIN IMMEDIATE` transactions. Semantic guarantees are identical: last-writer-wins is prevented; concurrent hydration for the same agent is detected.

**Unified integrity model (coordinated with Argus PR #151 §AD-1):**
- `version` INTEGER = OCC counter + monotonic seq_no. Incremented on every write. Replaces Dapr ETags. Satisfies Argus's seq_no requirement for replay attack detection.
- `integrity_hmac` BLOB = HMAC-SHA256(`tenant_id || agent_id || version || primary_blob`) using the tenant's KeyProvider subkey. Verified on every read. Satisfies Argus's per-row HMAC requirement for tamper detection in per-node SQLite files (Tier C Akash).

### 4.1 Pattern

```rust
// crates/af-swarm-state/src/turso_store.rs

/// Read agent state (replaces dapr.get_state + ETag)
pub async fn get_agent_state(db: &LibsqlConn, tenant_id: &str, agent_id: &str)
    -> Result<(AgentStateRecord, i64)>  // (record, version)
{
    let row = db.query(
        "SELECT state_snapshot, version, integrity_hmac FROM agent_state \
         WHERE tenant_id = ?1 AND agent_id = ?2",
        params![tenant_id, agent_id]).await?.next().await?
        .ok_or(StateError::NotFound)?;

    let blob: Vec<u8> = row.get(0)?;
    let version: i64  = row.get(1)?;
    let hmac: Vec<u8> = row.get(2)?;

    // Verify HMAC before handing record to agent process (tamper detection)
    verify_hmac(tenant_id, agent_id, version, &blob, &hmac)?;

    let record = AgentStateRecord::decode(blob.as_slice())?;
    Ok((record, version))
}

/// Write with OCC guard (replaces dapr.save_state + ETag)
/// Returns Err(StateError::VersionConflict) if version has changed since read.
pub async fn save_agent_state(db: &LibsqlConn, tenant_id: &str, agent_id: &str,
    record: &AgentStateRecord, expected_version: i64) -> Result<()>
{
    let blob = record.encode_to_vec();
    let new_version = expected_version + 1;
    let hmac = compute_hmac(tenant_id, agent_id, new_version, &blob)?;

    let rows_affected = db.execute(
        "UPDATE agent_state SET state_snapshot = ?1, version = ?2, integrity_hmac = ?3, \
         status = ?4, last_frozen_at = ?5 \
         WHERE tenant_id = ?6 AND agent_id = ?7 AND version = ?8",
        params![blob, new_version, hmac, record.status().as_str_name(),
                record.last_frozen_at.as_ref().map(|t| t.seconds * 1000),
                tenant_id, agent_id, expected_version]).await?;

    if rows_affected == 0 {
        return Err(StateError::VersionConflict { tenant_id: tenant_id.into(), agent_id: agent_id.into() });
    }
    Ok(())
}
```

On `VersionConflict`: retry from re-read (max 3 retries, exponential backoff). Still failing after 3 → surface as `ConflictError` to the orchestrator.

### 4.2 Multi-record Atomicity (Freeze)

SQLite `BEGIN IMMEDIATE` locks the database file for the duration of the write. A freeze (writing `agent_state` + `working_memory` atomically) uses a single transaction:

```rust
pub async fn freeze_agent(db: &LibsqlConn, state: &AgentStateRecord,
    memory: &WorkingMemoryRecord, state_ver: i64, mem_ver: i64) -> Result<()>
{
    db.execute("BEGIN IMMEDIATE", ()).await?;

    // 1. Write working_memory first (mirrors former Dapr multi-state write order)
    let mem_blob = memory.encode_to_vec();
    let new_mem_ver = mem_ver + 1;
    let mem_hmac = compute_hmac(state.tenant_id(), state.agent_id(), new_mem_ver, &mem_blob)?;
    db.execute(
        "UPDATE working_memory SET slots = ?1, version = ?2, integrity_hmac = ?3 \
         WHERE tenant_id = ?4 AND agent_id = ?5 AND version = ?6",
        params![mem_blob, new_mem_ver, mem_hmac, state.tenant_id(), state.agent_id(), mem_ver]).await?;

    // 2. Write agent_state referencing new memory version
    let state_blob = state.encode_to_vec();
    let new_state_ver = state_ver + 1;
    let state_hmac = compute_hmac(state.tenant_id(), state.agent_id(), new_state_ver, &state_blob)?;
    db.execute(
        "UPDATE agent_state SET state_snapshot = ?1, version = ?2, integrity_hmac = ?3, \
         working_memory_ver = ?4, status = 'FROZEN' \
         WHERE tenant_id = ?5 AND agent_id = ?6 AND version = ?7",
        params![state_blob, new_state_ver, state_hmac, new_mem_ver,
                state.tenant_id(), state.agent_id(), state_ver]).await?;

    db.execute("COMMIT", ()).await?;
    Ok(())
}
```

`BEGIN IMMEDIATE` prevents torn writes at the SQLite level — FM-21 becomes a transaction rollback rather than a detected-after-the-fact version mismatch. The checkpoint recovery protocol (§7.3) is retained as defence-in-depth for crashes between commit and WAL flush.

### 4.3 ACTIVE Agent Guard

When an agent is ACTIVE, `status = 'ACTIVE'` in `agent_state`. Any concurrent hydration attempt:
1. Reads the row → sees `status = 'ACTIVE'`
2. Checks `last_hydrated_at` staleness (configurable, default: 5 minutes)
3. If stale → `BEGIN IMMEDIATE`, set `status = 'FROZEN'`, increment `version`, recompute `integrity_hmac` → then hydrate
4. If not stale → return `AgentBusyError` to the orchestrator

---

## 5. Working Memory Representation

The working memory model follows MemAgent (ICLR 2026 oral, v3.0 §4.3): a **fixed-size 32-slot context window** with LRU + relevance scoring for eviction.

```
┌──────────────────────────────────────────────────────┐
│              Working Memory (32 slots max)             │
│                                                        │
│  Slot 0  [pinned]  "Agent identity and mission"       │
│  Slot 1  [pinned]  "Current task: deploy PR #47"      │
│  Slot 2            "Tool output: git status (turn 8)" │
│  Slot 3            "SYNAPSE retrieval: past deploy"   │
│  ...                                                   │
│  Slot 31           [LRU candidate]                    │
└──────────────────────────────────────────────────────┘
        ↓ eviction                ↓ retrieval miss
┌───────────────┐         ┌─────────────────────────┐
│   LanceDB     │         │ SYNAPSE spreading        │
│  (long-term   │◄────────│ activation → LanceDB ANN │
│   vectors)    │         │ query                    │
└───────────────┘         └─────────────────────────┘
```

### 5.1 Slot Budget Allocation

| Slot range | Reserved for | Eviction eligibility |
|------------|-------------|---------------------|
| 0–3 | Agent identity, current task, pinned facts | ❌ Pinned (never evicted) |
| 4–7 | Active goal frames | ❌ Pinned while goal is IN_PROGRESS |
| 8–23 | Recent conversation context | ✅ LRU eligible |
| 24–31 | Tool outputs and retrieval results | ✅ LRU + low-relevance-first eligible |

### 5.2 Scratchpad

The scratchpad is an ephemeral, unordered key-value store cleared at task completion. It holds intermediate computation state that should not persist across tasks:

- Partial tool outputs awaiting further processing
- Intermediate calculation results
- Temporary flags set by tool calls

Scratchpad is serialized inline in `WorkingMemoryRecord.scratchpad_bytes` (protobuf-encoded `ScratchpadEntry` list). Max size: 64 KB.

### 5.3 Active Goals

Goals are tracked in a stack frame model. The `AgentStateRecord.goal_stack` is the authoritative list; `WorkingMemoryRecord.active_goal_ids` is a denormalized reference for quick lookup during task execution.

Goals survive across task boundaries (a long-running goal persists across multiple freeze/hydrate cycles). Goal completion triggers a SYNAPSE update (new activation edge in the memory graph).

### 5.4 Pinned Facts

Up to 64 cross-session facts. These are high-confidence, frequently-accessed pieces of information that are too important to evict (user preferences, critical project constraints, recurring errors). Pinned facts are stored in `AgentStateRecord.pinned_facts` and replicated into working memory slots 0–3 on every hydration.

Pinned facts are **not** the same as working memory slots — they are persistent across sessions where working memory is ephemeral.

---

## 6. Embedding Storage: SurrealDB L2 (Cluster-Wide)

**OQ-S1 RESOLVED:** LanceDB is removed. The per-node migration gap (LanceDB data stranded when Dapr rebalances an actor) is eliminated — SurrealDB L2 is cluster-wide, so `surreal_record_ids` stored in Turso L1 `agent_state` point to records accessible from any node. No migration protocol needed.

**Embeddings are NOT stored inline in the Turso L1 records.** A single 384-dim all-MiniLM-L6-v2 vector is 1.5 KB; 200 entries = 300 KB bloating every hydration read. SurrealDB L2 is queried only at orchestrator planning time — not on the agent hydration hot path.

**Architecture:**

```
Turso L1 (per-node SQLite)          SurrealDB L2 (cluster-wide, AWS)
──────────────────────────          ────────────────────────────────────
agent_state.surreal_record_ids:     agent_memory table
  ["rec:01HXY...",                  ┌─────────────────────────────────┐
   "rec:01HXZ...",        ◄─────────│ id: rec:01HXY...                │
   ...]                             │ tenant_id: "af-tenant-42"       │
                                    │ agent_id: "senku"               │
                                    │ embedding: [0.12, -0.34, ...]   │
                                    │ content: "..."                  │
                                    │ turn_id: "01HXYZ"               │
                                    └─────────────────────────────────┘
```

**SurrealDB L2 Schema** (defined in `crates/af-swarm-state/src/surreal_memory_schema.rs`):

```surql
-- Namespace: af / {tenant_id} — One SurrealDB cluster for the whole swarm

DEFINE TABLE agent_memory SCHEMAFULL;

DEFINE FIELD tenant_id        ON agent_memory TYPE string ASSERT $value != NONE;
DEFINE FIELD agent_id         ON agent_memory TYPE string ASSERT $value != NONE;
DEFINE FIELD turn_id          ON agent_memory TYPE option<string>;
DEFINE FIELD content          ON agent_memory TYPE string;
DEFINE FIELD embedding        ON agent_memory TYPE array<float>;   -- 384-dim all-MiniLM-L6-v2
DEFINE FIELD relevance_score  ON agent_memory TYPE float DEFAULT 0.5;
DEFINE FIELD access_count     ON agent_memory TYPE int DEFAULT 0;
DEFINE FIELD is_pinned        ON agent_memory TYPE bool DEFAULT false;
DEFINE FIELD data_class       ON agent_memory TYPE string;         -- DSE DataClass
DEFINE FIELD sensitivity      ON agent_memory TYPE string;         -- DSE SensitivityLevel
DEFINE FIELD created_at       ON agent_memory TYPE datetime;
DEFINE FIELD last_accessed_at ON agent_memory TYPE datetime;

-- MTREE vector index (ANN search with cosine similarity)
DEFINE INDEX idx_memory_embedding ON agent_memory FIELDS embedding MTREE DIMENSION 384 DIST COSINE;
DEFINE INDEX idx_memory_agent     ON agent_memory FIELDS tenant_id, agent_id;
```

**Access patterns:**
- **Agent hydration (hot path):** Turso L1 only. `surreal_record_ids` read as JSON array from `agent_state`. SurrealDB is NOT queried on hydration unless the agent explicitly requests memory retrieval.
- **Orchestrator planning:** Queries SurrealDB L2 for capability graph traversal, temporal fact queries, and ANN vector search. Not latency-critical.

> **Security constraint — argus (#133, PR #151 §OQ-9-A3):** RESTRICTED-tier agents must NOT have their embeddings in SurrealDB L2 (Tier C/B hardware). RESTRICTED vector search requires a dedicated isolated store. This is tracked as OQ-S9.

---

## 7. Checkpoint Policy

### 7.1 When to Checkpoint

| Trigger | Condition | Priority |
|---------|-----------|----------|
| **Freeze** | Every time an agent transitions to FROZEN | Always (mandatory) |
| **Periodic** | Every N turns (default: 20, configurable per-agent) | Optional, enabled by default |
| **Pre-task** | Before executing a RESTRICTED-tier task | Mandatory for RESTRICTED |
| **Manual** | Operator or orchestrator request | On-demand |

The freeze checkpoint IS the primary mechanism. Because agents hydrate/freeze per-task in the virtual actor model, a checkpoint is written on every task completion. The periodic trigger handles long-running agents (conversation sessions) that may process many turns before freezing.

### 7.2 Checkpoint Retention Policy

```toml
# Per-agent TOML configuration (v3.0 §5.2 resource governor)
[checkpoint]
max_retained = 5          # Keep last 5 checkpoints per agent (default)
retain_pre_task = true    # Always retain pre-task checkpoints for RESTRICTED
compliance_retain_days = 0  # 0 = no compliance retention; >0 = retain for N days
                             # (see §9 for compliance TTL)
```

The checkpoint GC job (§9.2) enforces the `max_retained` cap. Older checkpoints beyond the cap are deleted unless `compliance_retain_days > 0`.

### 7.3 Recovery: Cold Start Protocol

On hydration, the runtime follows this protocol:

```
1. Read AgentStateRecord (key: {tenant}||{agent}||state)

2. Consistency check:
   a. working_memory_version in state == version in WorkingMemoryRecord?
      → YES: continue to step 3
      → NO: torn write detected → go to checkpoint recovery (step 2b)
   b. Checkpoint recovery:
      → Load latest CheckpointRecord (scan checkpoint keys, pick max seq)
      → Verify sha256_digest matches (tamper detection)
      → Restore AgentStateRecord and WorkingMemoryRecord from checkpoint
      → Log TornWriteRecoveryEvent to observability pipeline

3. If AgentStateRecord.status == ACTIVE (stale active guard):
   → Check last_hydrated_at age vs configured stale threshold (default 5 min)
   → If stale: force-freeze protocol (write FROZEN with ETag conflict)
   → If not stale: return AgentBusyError to orchestrator

4. Deserialize WorkingMemoryRecord (lazy-load from separate key)

5. Reconstruct ractor actor with restored state

6. Agent is now ACTIVE
```

### 7.4 Schema Versioning and Migration

The `schema_version` field on each proto message is a monotonic integer. Migration dispatch:

```rust
// In crates/af-swarm-state/src/migrations.rs

pub trait StateMigration: Send + Sync {
    fn from_version(&self) -> u32;
    fn to_version(&self) -> u32;
    fn migrate_agent_state(&self, raw: Bytes) -> Result<AgentStateRecord>;
    fn migrate_working_memory(&self, raw: Bytes) -> Result<WorkingMemoryRecord>;
}

// Migration registry — registered at startup
pub struct MigrationRegistry {
    migrations: BTreeMap<u32, Box<dyn StateMigration>>,
}

impl MigrationRegistry {
    pub fn apply_to_latest(&self, raw: Bytes, current_version: u32) -> Result<AgentStateRecord> {
        // Walk the migration chain: current_version → current_version+1 → ... → LATEST_VERSION
        // Each migration is applied in sequence
        // Migrations are idempotent (safe to re-apply)
    }
}

pub const LATEST_SCHEMA_VERSION: u32 = 1;  // increments with each breaking change
```

**Migration invariants:**
- Migrations are **additive only** during active development — no field removal until a deprecation cycle
- A field is deprecated by tagging it `[deprecated = true]` in the proto file and adding a migration that nulls it in the Rust representation
- Removal requires a tombstone migration that converts any remaining data and updates `schema_version`
- Forward compatibility: unknown proto fields are preserved by prost (they round-trip intact)

---

## 8. Per-Tenant Encryption at Rest

Encryption at rest is layered by DSE sensitivity tier (v3.0 §5.3.2). The Dapr state store sees only encrypted bytes for SENSITIVE and RESTRICTED records.

### 8.1 Encryption Tiers

| DSE Tier | Encryption at rest | Key source | Granularity |
|----------|-------------------|-----------|-------------|
| **PUBLIC** | None (Dapr store-level TLS only) | N/A | N/A |
| **INTERNAL** | None (Dapr store-level TLS only) | N/A | N/A |
| **SENSITIVE** | AES-256-GCM | Per-tenant key from Infisical | Tenant-level |
| **RESTRICTED** | FHE (TFHE-rs, existing v3.0 §5.4 primitives) | Per-tenant FHE key pair, managed by argus | Tenant-level, agent-granular IV |

> **Coordination required — argus (#133):** This design defines the encryption envelope structure (§8.2) and the key lookup interface (§8.3) but defers the key derivation hierarchy, storage, and rotation strategy to argus. Specifically needed:
> 1. Key granularity: per-tenant vs per-agent keys for SENSITIVE tier?
> 2. FHE key pair lifecycle: where are private keys stored? (Infisical? Dedicated KMS? Tier A only?)
> 3. Key rotation: how does a rotating key affect already-encrypted state records?
> 4. Compliance retention: must encrypted state be decryptable after key rotation for audit purposes?

### 8.2 Encryption Envelope

```protobuf
// Embedded in AgentStateRecord.encryption when tier >= SENSITIVE

message EncryptionEnvelope {
  EncryptionScheme scheme = 1;
  string key_id = 2;        // opaque ID — resolved via KeyProvider (§8.3)
  bytes  iv_or_nonce = 3;   // 12 bytes for AES-GCM, 16 bytes for FHE nonce
  bytes  ciphertext = 4;    // encrypted proto bytes of the wrapped record
  bytes  aad = 5;           // Additional authenticated data: tenant_id + agent_id + schema_version
                            // (not encrypted, but authenticated by GCM tag)
}

enum EncryptionScheme {
  ENCRYPTION_SCHEME_UNSPECIFIED = 0;
  PLAINTEXT = 1;      // DSE tier = PUBLIC or INTERNAL
  AES_256_GCM = 2;    // DSE tier = SENSITIVE
  FHE_TFHE_RS = 3;    // DSE tier = RESTRICTED (uses TFHE-rs from v3.0 §5.4)
}
```

**What is encrypted:**
- `AgentStateRecord` fields that contain sensitive data are serialized to a nested protobuf, AES-256-GCM or FHE encrypted, and stored in `EncryptionEnvelope.ciphertext`. The outer `AgentStateRecord` retains plaintext fields needed for key lookup: `tenant_id`, `agent_id`, `key_id`, `encryption.scheme`.
- `WorkingMemoryRecord`, `ConversationTurnRecord`, `ToolCallLogRecord` follow the same envelope pattern when the agent's DSE tier is SENSITIVE or RESTRICTED.

**FHE note:** For RESTRICTED agents, the FHE ciphertext is written to the Dapr store. The Dapr state store (Redis/PostgreSQL backend) can be on Tier C hardware and still provide the v3.0 security guarantee, because the FHE invariant holds: the state store sees only ciphertext, decryption happens in the agent process on Tier A hardware. This aligns with RFC-001 §4.4.

### 8.3 KeyProvider Interface

```rust
// crates/af-swarm-state/src/encryption.rs (skeleton)

#[async_trait]
pub trait KeyProvider: Send + Sync {
    /// Retrieve the encryption key for a given key_id
    async fn get_key(&self, key_id: &str) -> Result<EncryptionKey>;

    /// Get the current active key_id for a given tenant and sensitivity tier
    async fn current_key_id(&self, tenant_id: &str, tier: SensitivityLevel) -> Result<String>;

    /// Rotate the active key for a tenant (new key_id returned)
    /// Does NOT re-encrypt existing state — that is a separate migration job
    async fn rotate_key(&self, tenant_id: &str, tier: SensitivityLevel) -> Result<String>;
}

pub enum EncryptionKey {
    Aes256Gcm([u8; 32]),
    FheTfheRs(Box<tfhe::ClientKey>),  // TFHE-rs client key for RESTRICTED tier
}

// Production implementation backed by Infisical (v3.0 §5.2 S4)
pub struct InfisicalKeyProvider { /* ... */ }

// Test implementation with in-memory keys
pub struct InMemoryKeyProvider { /* ... */ }
```

---

## 9. TTL and Garbage Collection

**OQ-S3 RESOLVED:** Redis and PostgreSQL backends are replaced by a single unified mechanism — `expires_at` columns on `working_memory`, `conversation_turns`, and `tool_call_log` tables, with a background GC Tokio task. No split-backend complexity.

### 9.1 TTL Policy per Record Type

| Table | Default `expires_at` | Configurable? | Notes |
|-------|---------------------|--------------|-------|
| `agent_state` | NULL (no TTL) | Via explicit delete | Persists until agent deleted |
| `working_memory` | `now + 24h` | Yes (per-agent) | Auto-cleared on long idle |
| `checkpoints` | NULL (no TTL) | Via GC cap | See §9.2 |
| `conversation_turns` | `now + 30 days` | Yes (per-tenant compliance policy) | See §9.3 |
| `tool_call_log` | `now + 90 days` | Yes (per-tenant compliance policy) | See §9.3 |

`expires_at` is set at write time (Unix epoch ms). A single GC job handles all record types.

### 9.2 GC Job

```rust
// crates/af-swarm-state/src/gc.rs — runs every 5 minutes via Tokio interval

pub async fn run_gc(db: &LibsqlConn) -> Result<()> {
    let now_ms = Utc::now().timestamp_millis();

    db.execute("DELETE FROM working_memory WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        params![now_ms]).await?;
    db.execute("DELETE FROM conversation_turns WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        params![now_ms]).await?;
    db.execute("DELETE FROM tool_call_log WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        params![now_ms]).await?;

    // Checkpoint GC: retain last max_retained per agent (default 5)
    // If compliance_retain_days > 0: override — retain all within retention window
    db.execute(
        "DELETE FROM checkpoints WHERE (tenant_id, agent_id, checkpoint_seq) NOT IN (
           SELECT tenant_id, agent_id, checkpoint_seq FROM checkpoints c2
           WHERE c2.tenant_id = checkpoints.tenant_id AND c2.agent_id = checkpoints.agent_id
           ORDER BY checkpoint_seq DESC LIMIT ?1
         )",
        params![MAX_CHECKPOINTS_PER_AGENT]).await?;

    Ok(())
}
```

### 9.3 Compliance Retention

Compliance retention requirements (GDPR data subject requests, SOC 2 audit log) override the default `expires_at`. Two modes:

| Mode | Config | Behavior |
|------|--------|----------|
| **No compliance** | `compliance_retain_days = 0` | Default TTL applies |
| **Retention** | `compliance_retain_days = N` | `expires_at = now + N days` overrides default |

**GDPR erasure (right to be forgotten):** A dedicated deletion job purges all state for a given `tenant_id`:
- `DELETE FROM agent_state WHERE tenant_id = ?` + `DELETE FROM working_memory WHERE tenant_id = ?` — immediate
- `DELETE FROM conversation_turns WHERE tenant_id = ?` + `DELETE FROM tool_call_log WHERE tenant_id = ?` — immediate
- `DELETE FROM checkpoints WHERE tenant_id = ?` — immediate (or deferred if under active compliance hold)
- Delete all SurrealDB L2 records: `DELETE agent_memory WHERE tenant_id = ?` (surql)

> **Open question OQ-S4 (unchanged):** If a GDPR deletion request arrives during an active compliance hold, should we (a) delete the FHE key (render encrypted records permanently inaccessible) or (b) defer deletion until hold expires? Pending argus + legal decision.

---

## 10. Turso L1 + SurrealDB L2 Configuration

Section §10 (Dapr State Store Compatibility) is superseded. The Dapr YAML statestore component is no longer used for agent state. Dapr retains pub/sub (NATS) and workflow components unchanged.

```toml
# af-swarm-state configuration (per-node, injected via environment or TOML)

[l1_state]
# Turso embedded replica — local SQLite file (Akash node, follows agent pod)
database_url     = "file:/var/af-swarm/state.db"          # local replica path
sync_url         = "libsql://af-swarm-${env}.turso.io"    # remote Turso for replication
auth_token       = "${TURSO_AUTH_TOKEN}"                   # from Infisical
sync_interval_ms = 1000                                    # async push interval (see OQ-S7)

[l2_state]
# SurrealDB cluster — AWS deployment, stable Raft quorum (orchestrator queries only)
endpoint  = "ws://surrealdb-cluster:8000/rpc"
namespace = "af"
database  = "${TENANT_ID}"      # one SurrealDB database per tenant
username  = "${SURREAL_USER}"   # from Infisical
password  = "${SURREAL_PASS}"   # from Infisical
```

**Dapr statestore YAML** (`infrastructure/dapr/components/statestore.yaml`) is deprecated for agent state and can be removed after migration. The Dapr pub/sub component (`nats-pubsub.yaml`) and workflow component are unchanged.

---

## 11. MAST Failure Modes (New Entries)

The RFC-001 §7 compatibility matrix adds FM-19 and FM-20 to the MAST 18-mode taxonomy. This state schema adds:

| ID | Failure Mode | Detection | Recovery |
|----|-------------|-----------|---------|
| **FM-21** | **Torn-write inconsistency** (agent_state version updated, working_memory write failed) | `working_memory_ver` mismatch on hydration | `BEGIN IMMEDIATE` prevents this at the SQLite level; checkpoint recovery (§7.3) as fallback for WAL crashes |
| **FM-22** | **Version conflict storm** (multiple concurrent hydration attempts for same agent) | 3 consecutive `VersionConflict` errors | ACTIVE guard check → force-freeze (§4.3) → retry |
| **FM-23** | ~~LanceDB migration gap~~ | ~~`lancedb_record_ids` missing on new node~~ | **RESOLVED** — SurrealDB L2 is cluster-wide; no per-node migration needed |
| **FM-24** | **Encryption key unavailable** (KeyProvider unreachable at hydration time) | `KeyProvider.get_key()` returns error | Reject hydration with `EncryptionUnavailableError`; do not proceed with plaintext |
| **FM-25** | **Turso replica sync lag** — local SQLite replica is stale after a crash | On startup: compare local `max(version)` against remote; divergence detected | Re-sync from Turso remote before accepting hydrations; no data loss (WAL-based) |
| **FM-26** | **SurrealDB L2 unavailable** — orchestrator cannot query capability graph or vectors | SurrealDB connection error at dispatch-planning time | Orchestrator falls back to round-robin dispatch without capability matching; agents continue hydrating from Turso L1 unaffected |

**FM-25 and FM-26 have independent blast radii.** If SurrealDB goes down (FM-26), agent hydrations continue at full speed from Turso L1. If a Turso replica is stale (FM-25), only agents on that specific node are affected. This is a structural improvement over the original design where Redis going down blocked all hydrations.

---

## 12. Open Questions

**Resolved by OQ-9 (2026-04-20):**

| ID | Question | Status |
|----|----------|--------|
| **OQ-S1** | LanceDB node migration protocol | **RESOLVED** — SurrealDB L2 is cluster-wide; no per-node migration |
| **OQ-S3** | Redis vs PostgreSQL backend split | **RESOLVED** — Turso L1 handles both hot and cold local state with a single `expires_at` GC mechanism |

**Still open:**

| ID | Question | Depends On | Default if Not Resolved |
|----|----------|-----------|------------------------|
| **OQ-S2** | Key derivation hierarchy for per-tenant encryption (§8.3) | argus (#133) | Unblock with `InMemoryKeyProvider` in Phase 0–5; replace at Phase 6 |
| **OQ-S4** | GDPR erasure during compliance hold — delete FHE key vs defer deletion (§9.3) | argus (#133), legal | Implement erasure without compliance hold as Phase 1 behavior; block on legal for hold interaction |
| **OQ-S5** | `max_pinned_facts = 64` — per-tenant configurable or hard cap? | Product | Hard cap of 64 until product requests otherwise |
| **OQ-S6** | `tool_call_log` → Turso L1 (Phase 1) vs hash-chain audit log (Phase 12) | senku (#130), chief architect | Write to Turso L1 for Phase 1; integrate with S7 hash-chain in Phase 12 |

**New open questions from OQ-9:**

| ID | Question | Depends On | Default if Not Resolved |
|----|----------|------------|------------------------|
| **OQ-S7** | Turso sync interval and RPO — `sync_interval_ms = 1000` means max 1 second of writes at risk if a node crashes before sync. Acceptable? | senku (#130), product | 1s RPO acceptable for Phase 1; tighten to 200ms in Phase 6 |
| **OQ-S8** | SurrealDB cluster topology — replication factor, deployment target (AWS?), who owns infra? | senku (#130) | Single-node SurrealDB for Phase 1; 3-node cluster for Phase 6 |
| **OQ-S9** | RESTRICTED data in per-node SQLite files (Tier C) — FHE ciphertext is safe but argus should confirm whether the per-node file-at-rest has a different threat model than centralized Redis | argus (#133) | Treat as acceptable (same FHE invariant); argus to confirm before Phase 6 RESTRICTED deployment |

---

## 13. Crate Structure

```
crates/af-swarm-state/
├── Cargo.toml                   # add: libsql (Turso client), surrealdb
├── build.rs                     # prost-build invocation for proto codegen (unchanged)
├── migrations/
│   └── 001_initial.sql          # Turso L1 DDL (§3.1)
├── proto/
│   ├── agent_state.proto        # AgentStateRecord (surreal_record_ids updated)
│   ├── working_memory.proto
│   ├── conversation.proto
│   ├── tool_call_log.proto
│   ├── checkpoint.proto
│   ├── encryption.proto
│   └── common.proto             # shared enums (AgentLifecycleStatus, GoalStatus, etc.)
└── src/
    ├── lib.rs
    ├── generated/               # prost codegen output (gitignored)
    ├── turso_store.rs           # TursoStateStore — L1 read/write/version/hmac (replaces store.rs)
    ├── surreal_store.rs         # SurrealStateStore — L2 vector/graph/temporal queries
    ├── surreal_memory_schema.rs # SurrealDB L2 table definitions (§6)
    ├── hydrate.rs               # updated: reads from TursoStateStore instead of Dapr API
    ├── freeze.rs                # updated: BEGIN IMMEDIATE writes (§4.2)
    ├── encryption.rs            # unchanged: KeyProvider trait + EncryptionEnvelope codec
    ├── gc.rs                    # updated: expires_at GC (§9.2) replaces Redis TTL + PG GC
    ├── migrations.rs            # unchanged: MigrationRegistry + StateMigration trait
    └── error.rs                 # add: VersionConflict, TursoSyncLag, HmacVerifyFailed variants
```

---

## 14. References

1. AF Swarm Runtime v3.0 — §4.3 Memory Engine, §5.3 DSE, §5.4 FHE
   [docs/architecture/rust-swarm-runtime.md](https://github.com/alternatefutures/admin/blob/main/docs/architecture/rust-swarm-runtime.md)
2. RFC-001 (1000-Agent Amendment) — §1.2 Virtual Actor Substrate, §4.4 FHE for Agent State
   [feat/issue-127-1000-agent-amendment](https://github.com/alternatefutures/admin/blob/feat/issue-127-1000-agent-amendment/docs/architecture/reviews/1000-agent-amendment.md)
3. MemAgent (ICLR 2026 oral) — O(1) context window, 32-slot model
4. SYNAPSE (2026) — spreading activation memory graph
5. prost — [github.com/tokio-rs/prost](https://github.com/tokio-rs/prost)
6. TFHE-rs — [github.com/zama-ai/tfhe-rs](https://github.com/zama-ai/tfhe-rs)
7. Turso — Microsecond SQL latency with embedded replicas
   [turso.tech/blog/microsecond-level-sql-query-latency-with-libsql-local-replicas-5e4ae19b628b](https://turso.tech/blog/microsecond-level-sql-query-latency-with-libsql-local-replicas-5e4ae19b628b)
8. Turso for AI Agent Memory — [turso.tech/blog/powering-ai-agents-turso-voltagent](https://turso.tech/blog/powering-ai-agents-turso-voltagent)
9. SurrealDB — The Context Layer for AI Agents — [surrealdb.com/use-cases/ai-agents](https://surrealdb.com/use-cases/ai-agents)
10. SurrealDB 3.0 Benchmarks — [surrealdb.com/benchmarks](https://surrealdb.com/benchmarks)
11. libsql Rust client (Turso) — [github.com/tursodatabase/libsql](https://github.com/tursodatabase/libsql)
12. OQ-9 decision — [alternatefutures/admin#144](https://github.com/alternatefutures/admin/issues/144)
13. Argus OQ-9 security addendum (per-row HMAC + seq_no) — [alternatefutures/admin#151](https://github.com/alternatefutures/admin/pull/151)
